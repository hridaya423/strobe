use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use ring::digest;
use serde::{Deserialize, Serialize};
use vrf::audio::SAMPLE_RATE;
use vrf::mux::{AudioTrack, Muxer};
#[cfg(target_os = "macos")]
use vrf_encode::audiotoolbox::encode_aac;
#[cfg(windows)]
use vrf_encode::mediafoundation::encode_aac;
use vrf_encode::{AacConfig, AacStream, Rational};

use crate::driver::Summary;

mod audio;
pub mod hyperframes;
pub mod remotion;

const NOT_HASHED: [&str; 6] = [
    "node_modules",
    ".git",
    ".vrf",
    ".cache",
    ".hyperframes",
    "out",
];

#[derive(Debug, Serialize, Deserialize)]
pub struct StageStats {
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl StageStats {
    pub(crate) fn of(samples: &[Duration]) -> StageStats {
        let summary = Summary::of(samples);
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        StageStats {
            mean_ms: ms(summary.mean),
            p50_ms: ms(summary.p50),
            p99_ms: ms(summary.p99),
            max_ms: ms(samples.iter().copied().max().unwrap_or_default()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PerFrame {
    pub seek: StageStats,
    pub begin_to_paint: StageStats,
    pub convert: StageStats,
    pub encode: StageStats,
}

const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) fn since(start: SystemTime) -> f64 {
    start.elapsed().unwrap_or_default().as_secs_f64() * 1000.0
}

/// Reports frames done to a callback at most every 50 ms, and notes when the first one arrived.
pub(crate) struct Progress<'a> {
    callback: Mutex<(&'a mut (dyn FnMut(u64, u64) -> Result<()> + Send), Instant)>,
    total: u64,
    command_start: SystemTime,
    first_frame_ms: OnceLock<f64>,
}

impl<'a> Progress<'a> {
    pub(crate) fn new(
        callback: &'a mut (dyn FnMut(u64, u64) -> Result<()> + Send),
        total: u64,
        command_start: SystemTime,
    ) -> Progress<'a> {
        Progress {
            callback: Mutex::new((callback, Instant::now())),
            total,
            command_start,
            first_frame_ms: OnceLock::new(),
        }
    }

    pub(crate) fn frame(&self, done: u64) -> Result<()> {
        self.first_frame_ms
            .get_or_init(|| since(self.command_start));
        let mut guard = self.callback.lock().unwrap();
        let (callback, last) = &mut *guard;
        if last.elapsed() >= PROGRESS_INTERVAL || done == self.total {
            callback(done, self.total)?;
            *last = Instant::now();
        }
        Ok(())
    }

    pub(crate) fn first_frame_ms(&self) -> f64 {
        self.first_frame_ms.get().copied().unwrap_or_default()
    }
}

pub(crate) fn cache_dir() -> PathBuf {
    std::env::var_os("VRF_CACHE_DIR")
        .map_or_else(|| std::env::temp_dir().join("vrf-cache"), PathBuf::from)
}

pub(crate) fn hash_tree(context: &mut digest::Context, root: &Path, dir: &Path) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if dir == root
            && NOT_HASHED
                .iter()
                .any(|name| entry.file_name() == OsStr::new(name))
        {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            hash_tree(context, root, &path)?;
            continue;
        }
        context.update(path.strip_prefix(root)?.as_os_str().as_encoded_bytes());
        match fs::metadata(&path) {
            Ok(metadata) => {
                let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
                context.update(&metadata.len().to_le_bytes());
                context.update(&modified.to_le_bytes());
            }
            Err(error) if file_type.is_symlink() && error.kind() == std::io::ErrorKind::NotFound => {
                context.update(fs::read_link(&path)?.as_os_str().as_encoded_bytes());
            }
            Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
        }
    }
    Ok(())
}

pub(crate) fn contained_join(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = root.to_owned();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(path)
}

pub(crate) fn partial_path(root: &Path, name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    root.join(format!(
        "{name}.partial-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

pub(crate) fn write_if_changed(path: &Path, contents: &str) -> Result<()> {
    if fs::read(path).is_ok_and(|existing| existing == contents.as_bytes()) {
        return Ok(());
    }
    let name = path.file_name().context("a script path has a file name")?;
    let partial = partial_path(
        path.parent().context("a script path has a parent")?,
        &name.to_string_lossy(),
    );
    fs::write(&partial, contents)?;
    fs::rename(&partial, path)?;
    Ok(())
}

pub(crate) fn short_hex(context: digest::Context) -> String {
    context.finish().as_ref()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Timeline samples for `frames` frames at `frame_rate`.
pub(crate) fn audio_samples(frames: u64, frame_rate: Rational) -> u64 {
    let rate = u128::from(SAMPLE_RATE) * frames as u128 * frame_rate.den as u128;
    ((rate + frame_rate.num as u128 / 2) / frame_rate.num as u128) as u64
}

/// Encodes interleaved stereo `pcm` as AAC at `bit_rate`. A mix that is silent everywhere gives
/// no track, so a project with no audio stays video-only.
pub(crate) fn encode_track(pcm: &[f32], bit_rate: u32) -> Result<Option<AacStream>> {
    if pcm.iter().all(|sample| *sample == 0.0) {
        return Ok(None);
    }
    encode_aac(
        pcm,
        AacConfig {
            sample_rate: SAMPLE_RATE,
            channels: 2,
            bit_rate,
        },
    )
    .map(Some)
}

pub(crate) fn add_track(muxer: &mut Muxer, stream: Option<AacStream>) -> Result<()> {
    match stream {
        Some(stream) => muxer.set_audio(AudioTrack::from(&stream)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Tree {
            let dir = std::env::temp_dir().join(format!("vrf-hash-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tree(dir)
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn hash(&self) -> Result<String> {
            let mut context = digest::Context::new(&digest::SHA256);
            hash_tree(&mut context, &self.0, &self.0)?;
            Ok(short_hex(context))
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn partial_paths_differ_for_every_call() {
        let root = Path::new("/cache");
        let first = partial_path(root, "abc");
        assert_ne!(first, partial_path(root, "abc"));
        assert!(first.starts_with(root));
    }

    #[test]
    fn a_script_is_replaced_by_rename_and_left_alone_when_unchanged() -> Result<()> {
        let tree = Tree::new("script");
        let script = tree.0.join("run.mjs");
        write_if_changed(&script, "one")?;
        assert_eq!(fs::read_to_string(&script)?, "one");
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&script)?);
        write_if_changed(&script, "one")?;
        #[cfg(unix)]
        assert_eq!(inode, std::os::unix::fs::MetadataExt::ino(&fs::metadata(&script)?));
        write_if_changed(&script, "two")?;
        assert_eq!(fs::read_to_string(&script)?, "two");
        let leftovers = fs::read_dir(&tree.0)?.count();
        assert_eq!(leftovers, 1);
        Ok(())
    }

    #[test]
    fn contained_join_refuses_parent_and_absolute_paths() {
        let root = Path::new("/project");
        assert_eq!(contained_join(root, "a/b.mp3"), Some(root.join("a/b.mp3")));
        assert_eq!(contained_join(root, "./a.mp3"), Some(root.join("a.mp3")));
        assert_eq!(contained_join(root, "../a.mp3"), None);
        assert_eq!(contained_join(root, "a/../../a.mp3"), None);
        assert_eq!(contained_join(root, "/etc/passwd"), None);
    }

    #[test]
    fn sources_under_a_nested_out_folder_change_the_hash() -> Result<()> {
        let tree = Tree::new("nested-out");
        tree.write("src/out/scene.tsx", "a");
        tree.write("public/out/logo.svg", "a");
        let before = tree.hash()?;
        tree.write("src/out/scene.tsx", "changed");
        let after_src = tree.hash()?;
        assert_ne!(before, after_src);
        tree.write("public/out/logo.svg", "changed");
        assert_ne!(after_src, tree.hash()?);
        Ok(())
    }

    #[test]
    fn the_root_level_output_and_dependency_folders_do_not_change_the_hash() -> Result<()> {
        let tree = Tree::new("root-out");
        tree.write("src/index.ts", "a");
        let before = tree.hash()?;
        for name in NOT_HASHED {
            tree.write(&format!("{name}/file"), "x");
        }
        assert_eq!(before, tree.hash()?);
        tree.write("out/file", "changed again, longer");
        assert_eq!(before, tree.hash()?);
        Ok(())
    }

    #[test]
    fn a_lock_file_at_the_project_root_changes_the_hash() -> Result<()> {
        let tree = Tree::new("lock");
        tree.write("package.json", "{}");
        tree.write("package-lock.json", "1");
        let before = tree.hash()?;
        tree.write("package-lock.json", "22");
        assert_ne!(before, tree.hash()?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_does_not_fail_the_hash() -> Result<()> {
        let tree = Tree::new("dangling");
        tree.write("index.html", "a");
        std::os::unix::fs::symlink("missing-target", tree.0.join("link"))?;
        let before = tree.hash()?;
        fs::remove_file(tree.0.join("link"))?;
        std::os::unix::fs::symlink("other-target", tree.0.join("link"))?;
        assert_ne!(before, tree.hash()?);
        Ok(())
    }
}
