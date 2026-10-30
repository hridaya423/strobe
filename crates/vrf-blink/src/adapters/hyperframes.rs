use std::collections::HashMap;
use std::fs;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result, anyhow, ensure};
use ring::digest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use vrf::audio::{HyperframesTrack, hyperframes_clips, mix};
use vrf_encode::{Codec, Rational};

use super::{
    PerFrame, Progress, StageStats, add_track, encode_track, audio_samples, cache_dir, hash_tree, short_hex,
    since,
};
use crate::driver::{Config, Plan, Runs, Warm, render_runs};
use crate::video::NativeVideos;
use crate::skip::{self, Skip, TRACK_JS};
use crate::{Mode, Project, Ui, View};

const HYPERFRAMES_JS: &str = include_str!("../../js/hyperframes.js");
const CLOCK_JS: &str = include_str!("../../js/clock.js");
const VIDEO_JS: &str = include_str!("../../js/video.js");
const HOP_JS: &str = include_str!("../../js/hyperframes-hop.js");
const PREPARE_JS: &str = include_str!("../../js/hyperframes-prepare.mjs");
const NATIVE_JS: &str = include_str!("../../js/hyperframes-native.mjs");

const PROBE_SIZE: (i32, i32) = (640, 360);
const AUDIO_BIT_RATE: u32 = 192_000;
const FRAMES_URL: &str = "/__hyperframes_video_frames/";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub project: PathBuf,
    /// The installed `hyperframes` package. Found in the project's `node_modules`, or in an
    /// ancestor's, if absent.
    pub hyperframes: Option<PathBuf>,
    pub fps: u32,
    pub mode: Mode,
    pub plan: Plan,
    /// Mix the composition's audio into the file. Off leaves a video-only file.
    pub audio: bool,
    pub native_video: bool,
    pub skip: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Composition {
    pub width: u32,
    pub height: u32,
    pub out_width: u32,
    pub out_height: u32,
    pub frames: u64,
    pub duration: f64,
}

#[derive(Deserialize)]
struct VideoSize {
    id: String,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Prepared {
    total_frames: u64,
    has_video: bool,
    video_metadata: Vec<VideoSize>,
    frames: Vec<Vec<(String, Value)>>,
    #[serde(default)]
    natives: HashMap<String, Value>,
    tracks: Vec<HyperframesTrack>,
}

pub struct Prepare {
    pub dir: PathBuf,
    pub cached: bool,
    prepared: Prepared,
}

impl Prepare {
    pub fn native_videos(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.prepared.natives.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }
}

fn find_hyperframes(project: &Path) -> Result<PathBuf> {
    project
        .ancestors()
        .map(|dir| dir.join("node_modules/hyperframes"))
        .find(|dir| dir.join("dist").is_dir())
        .with_context(|| {
            format!(
                "no hyperframes package in node_modules above {}; pass one with --hyperframes",
                project.display()
            )
        })
}

fn input_hash(project: &Path, hyperframes: &Path, fps: u32, native: &str) -> Result<String> {
    let mut context = digest::Context::new(&digest::SHA256);
    hash_tree(&mut context, project, project)?;
    context.update(PREPARE_JS.as_bytes());
    context.update(NATIVE_JS.as_bytes());
    context.update(native.as_bytes());
    context.update(&fps.to_le_bytes());
    let manifest = hyperframes.join("package.json");
    context.update(
        &fs::read(&manifest).with_context(|| format!("{} is missing", manifest.display()))?,
    );
    Ok(short_hex(context))
}

/// Compiles the project and extracts its video frames with the project's `hyperframes` package in a
/// Node subprocess, the way `hyperframes render` does before it opens a browser. The result is
/// cached under `VRF_CACHE_DIR` (default: the system temp dir) by a hash of the project's files,
/// the package version, the frame rate and the preparation script.
pub fn prepare(
    project: &Path,
    hyperframes: Option<&Path>,
    fps: u32,
    native: Option<&[String]>,
) -> Result<Prepare> {
    let project = crate::canonical(project)
        .with_context(|| format!("project directory {} does not exist", project.display()))?;
    ensure!(
        project.join("index.html").is_file(),
        "{} has no index.html",
        project.display()
    );
    let hyperframes = match hyperframes {
        Some(dir) => dir.to_owned(),
        None => find_hyperframes(&project)?,
    };
    let native = match native {
        Some(extract) => json!({ "native": true, "extract": extract }).to_string(),
        None => "{}".to_owned(),
    };
    let hash = input_hash(&project, &hyperframes, fps, &native)?;
    let root = cache_dir().join("hyperframes");
    let dir = root.join(&hash);
    let read = |dir: PathBuf, cached| -> Result<Prepare> {
        let text = fs::read(dir.join("prepared.json"))?;
        Ok(Prepare {
            dir,
            cached,
            prepared: serde_json::from_slice(&text)?,
        })
    };
    if dir.join("prepared.json").is_file() {
        return read(dir, true);
    }
    fs::create_dir_all(&root)?;
    let script = root.join("hyperframes-prepare.mjs");
    fs::write(&script, PREPARE_JS)?;
    fs::write(root.join("hyperframes-native.mjs"), NATIVE_JS)?;
    let partial = root.join(format!("{hash}.partial-{}", std::process::id()));
    let _ = fs::remove_dir_all(&partial);
    let output = Command::new("node")
        .arg(&script)
        .arg(&hyperframes)
        .arg(&project)
        .arg(&partial)
        .arg(fps.to_string())
        .arg(&native)
        .output()
        .context("run node to prepare the project")?;
    ensure!(
        output.status.success(),
        "preparing {} failed:\n{}",
        project.display(),
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    );
    if let Err(error) = fs::rename(&partial, &dir) {
        ensure!(
            dir.join("prepared.json").is_file(),
            "cannot move the prepared project into place: {error}"
        );
        let _ = fs::remove_dir_all(&partial);
    }
    read(dir, false)
}

pub fn prepare_for(options: &Options) -> Result<Prepare> {
    let hyperframes = options.hyperframes.as_deref();
    if !options.native_video {
        return prepare(&options.project, hyperframes, options.fps, None);
    }
    let first = prepare(&options.project, hyperframes, options.fps, Some(&[]))?;
    let refused = NativeVideos::new(
        &first.prepared.natives,
        &[&first.dir.join("plan/compiled"), &options.project],
    )?
    .refused();
    if refused.is_empty() {
        return Ok(first);
    }
    prepare(&options.project, hyperframes, options.fps, Some(&refused))
}

/// Where a composition's `<video>` elements get their pictures. Frame `n` of each video comes
/// from a JPEG that HyperFrames' own extraction wrote, chosen by its own lookup (contract items 6
/// and 23). Anything that answers `at` with the same entries can replace it.
pub struct VideoFrames {
    table: Vec<Vec<(String, Value)>>,
    enabled: bool,
}

impl VideoFrames {
    /// The videos to show at output frame `frame`, as the `videos` option of `__vrf.hf.seek`.
    /// `None` leaves video elements alone; a composition with videos always gets a list, empty
    /// when none is on screen.
    pub fn at(&self, frame: u64) -> Option<Value> {
        self.enabled.then(|| {
            self.table.get(frame as usize).map_or(json!([]), |entries| {
                entries
                    .iter()
                    .map(|(id, shown)| match shown {
                        Value::String(path) => json!({ "id": id, "src": format!("{FRAMES_URL}{path}") }),
                        slot => json!({ "id": id, "slot": slot }),
                    })
                    .collect()
            })
        })
    }
}

/// A prepared project that can open ready-to-seek views of its composition.
pub struct Page {
    project: Project,
    script: String,
    init: String,
    pub video: VideoFrames,
    native: NativeVideos,
    tracks: Vec<HyperframesTrack>,
    frames: u64,
}

impl Page {
    pub fn new(prepare: Prepare, options: &Options) -> Result<Page> {
        let Prepare {
            dir,
            prepared,
            cached: _,
        } = prepare;
        let early_stub = fs::read_to_string(dir.join("early-stub.js"))?;
        let project = Project::open_layers(
            dir.join("serve"),
            &[dir.join("plan/compiled"), options.project.clone()],
            options.mode,
        )?;
        let native = NativeVideos::new(&prepared.natives, &[&dir.join("plan/compiled"), &options.project])?;
        let hints: Vec<Value> = prepared
            .video_metadata
            .iter()
            .map(|v| json!({ "id": v.id, "width": v.width, "height": v.height }))
            .collect();
        Ok(Page {
            project,
            script: format!(
                "{early_stub}\n{}\n{}\n{HYPERFRAMES_JS}\n{CLOCK_JS}\n{}",
                if native.is_empty() { "" } else { VIDEO_JS },
                if native.is_empty() { "" } else { HOP_JS },
                if options.skip { TRACK_JS } else { "" }
            ),
            init: json!({ "fps": [options.fps, 1], "videoMetadataHints": hints, "nativeVideos": native.params() })
                .to_string(),
            native,
            video: VideoFrames {
                table: prepared.frames,
                enabled: prepared.has_video,
            },
            tracks: prepared.tracks,
            frames: prepared.total_frames,
        })
    }

    /// Opens a view of `width` x `height` with the adapter and the virtual clock injected, and
    /// initialises the composition in it.
    pub fn open(&self, ui: &Ui, width: i32, height: i32) -> Result<(View, Composition)> {
        let mut view = View::open_project(
            ui,
            &self.project,
            "index.html",
            width,
            height,
            Some(&self.script),
        )?;
        if !self.native.is_empty() {
            self.native.attach(&mut view);
        }
        let meta = view
            .evaluate(&format!("__vrf.hf.init({})", self.init))
            .context("the composition did not initialise")?;
        Ok((view, serde_json::from_value(meta)?))
    }

    /// Leaves the view showing `frame`.
    pub fn seek(view: &mut View, frame: u64, videos: Option<Value>, ask: bool) -> Result<bool> {
        let options = videos.map_or_else(|| json!({}), |videos| json!({ "videos": videos }));
        let (_, unchanged) =
            skip::evaluate(view, &format!("__vrf.hf.seek({frame}, {options})"), ask)?;
        Ok(unchanged)
    }
}

/// One render: what to render, which frames of it, the codec, and where the file goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderJob {
    pub options: Options,
    /// An inclusive range of output frames; `None` renders all of them.
    pub frames: Option<RangeInclusive<u64>>,
    pub codec: Codec,
    pub output: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Stages {
    pub prepare_s: f64,
    pub prepare_cached: bool,
    pub probe_s: f64,
    pub render_s: f64,
    pub audio_s: f64,
    pub finish_s: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub composition: Composition,
    pub first_frame: u64,
    pub frames: u64,
    pub elapsed_s: f64,
    pub fps: f64,
    pub views: usize,
    pub run: u64,
    pub lead_in: u64,
    pub lead_in_frames: u64,
    /// Command start to the first view being ready to seek.
    pub setup_ms: f64,
    /// Command start to the first frame handed to the encoder.
    pub first_frame_ms: f64,
    /// Command start to the finished file.
    pub total_ms: f64,
    pub stages: Stages,
    pub per_frame: PerFrame,
    pub repeats: u64,
    pub skipped: u64,
    pub capture_retries: u64,
}

fn static_frames(probe: &mut View) -> Result<Vec<u64>> {
    let plan = probe.evaluate("__vrf.hf.staticFrames()")?;
    Ok(serde_json::from_value(plan["frames"].clone())?)
}

fn track_file(project: &Path) -> impl Fn(&str) -> Result<PathBuf> + '_ {
    move |src| {
        let path = project.join(src);
        ensure!(path.is_file(), "audio source {src} is not in the project");
        Ok(path)
    }
}

/// Renders a HyperFrames composition to a video file: prepare it with HyperFrames' own compile and
/// frame extraction, serve it through `vrf://project/`, then drive each frame through the page's
/// seek, one begin frame, the GPU convert, the encoder and the muxer. The audio tracks are mixed
/// and added at the end.
///
/// `command_start` is when the user's command began; `first_frame_ms` and `setup_ms` count from
/// it. `progress` gets the frames done and the total, and a failure from it stops the render.
pub fn render(
    ui: &Ui,
    job: &RenderJob,
    warm: &mut Warm,
    command_start: SystemTime,
    progress: &mut (dyn FnMut(u64, u64) -> Result<()> + Send),
) -> Result<Report> {
    let RenderJob {
        options,
        frames,
        codec,
        output: out,
    } = job;
    let started = Instant::now();
    let step = Instant::now();
    let prepare = prepare_for(options)?;
    let (prepare_s, prepare_cached) = (step.elapsed(), prepare.cached);
    let page = Page::new(prepare, options)?;

    let step = Instant::now();
    let (mut probe, composition) = page.open(ui, PROBE_SIZE.0, PROBE_SIZE.1)?;
    let static_frames = options
        .skip
        .then(|| static_frames(&mut probe))
        .transpose()?;
    probe.close()?;
    let probe_s = step.elapsed();
    ensure!(
        composition.frames == page.frames,
        "the page counts {} frames and HyperFrames' plan {}",
        composition.frames,
        page.frames
    );
    ensure!(
        composition.out_width == composition.width && composition.out_height == composition.height,
        "{}x{} has an odd side; padding to even is not implemented",
        composition.width,
        composition.height
    );
    let range = frames.clone().unwrap_or(0..=composition.frames - 1);
    let (first, last) = (*range.start(), *range.end());
    ensure!(
        first <= last && last < composition.frames,
        "frames {first}-{last} are outside the composition's {} frames",
        composition.frames
    );
    let count = last - first + 1;

    let frame_rate = Rational::new(i64::from(options.fps), 1);
    let mut config = Config::new(
        composition.width,
        composition.height,
        frame_rate,
        *codec,
        count,
    );
    config.skip = static_frames.map(Skip::planned);
    let (width, height) = (composition.width as i32, composition.height as i32);
    let progress = Progress::new(progress, count, command_start);
    let first_ready = OnceLock::new();
    let track = options
        .audio
        .then(|| -> Result<_> {
            let project_dir = crate::canonical(&options.project)?;
            let clips = hyperframes_clips(&page.tracks, track_file(&project_dir))?;
            let (from, length) = (
                audio_samples(first, frame_rate),
                audio_samples(composition.frames, frame_rate),
            );
            let to = from + audio_samples(count, frame_rate);
            Ok(thread::spawn(move || {
                let timeline = mix(&clips, length)?;
                encode_track(&timeline[from as usize * 2..to as usize * 2], AUDIO_BIT_RATE)
            }))
        })
        .transpose()?;
    let render_started = Instant::now();
    let runs = render_runs(
        &config,
        options.plan,
        first,
        out,
        warm,
        || {
            let view = page.open(ui, width, height)?.0;
            first_ready.get_or_init(|| since(command_start));
            Ok(view)
        },
        |view, frame, ask| {
            Page::seek(view, frame, page.video.at(frame), ask)
                .map(|unchanged| ((), unchanged))
                .map_err(|error| match view.check_project() {
                    Err(refused) => error.context(refused.to_string()),
                    Ok(()) => error,
                })
        },
        |done, _, _| progress.frame(done),
    )?;
    let render_s = render_started.elapsed();
    let Runs {
        plan,
        mut muxer,
        stats,
        lead_in_frames,
        ..
    } = runs;

    let step = Instant::now();
    if let Some(track) = track {
        add_track(&mut muxer, track.join().map_err(|_| anyhow!("the audio mix panicked"))??)?;
    }
    let audio_s = step.elapsed();
    let step = Instant::now();
    muxer.finish()?;
    let finish_s = step.elapsed();

    let elapsed = started.elapsed();
    Ok(Report {
        composition,
        first_frame: first,
        frames: count,
        elapsed_s: elapsed.as_secs_f64(),
        fps: count as f64 / elapsed.as_secs_f64(),
        views: plan.views,
        run: plan.run.min(count),
        lead_in: plan.lead_in,
        lead_in_frames,
        setup_ms: first_ready.get().copied().unwrap_or_default(),
        first_frame_ms: progress.first_frame_ms(),
        total_ms: since(command_start),
        stages: Stages {
            prepare_s: prepare_s.as_secs_f64(),
            prepare_cached,
            probe_s: probe_s.as_secs_f64(),
            render_s: render_s.as_secs_f64(),
            audio_s: audio_s.as_secs_f64(),
            finish_s: finish_s.as_secs_f64(),
        },
        per_frame: PerFrame {
            seek: StageStats::of(&stats.seek),
            begin_to_paint: StageStats::of(&stats.begin_to_paint),
            convert: StageStats::of(&stats.convert),
            encode: StageStats::of(&stats.encode),
        },
        repeats: stats.repeats,
        skipped: stats.skipped,
        capture_retries: stats.retries,
    })
}
