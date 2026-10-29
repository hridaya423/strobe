use std::fs;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use ring::digest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use vrf::audio::{HyperframesTrack, hyperframes_clips, mix};
use vrf_encode::{Codec, Rational};

use super::{PerFrame, StageStats, add_audio, audio_samples, cache_dir, hash_tree, short_hex};
use crate::driver::{Capture, Config, Pipeline};
use crate::{Mode, Project, Ui, View};

const HYPERFRAMES_JS: &str = include_str!("../../js/hyperframes.js");
const CLOCK_JS: &str = include_str!("../../js/clock.js");
const PREPARE_JS: &str = include_str!("../../js/hyperframes-prepare.mjs");

const PROBE_SIZE: (i32, i32) = (640, 360);
const AUDIO_BIT_RATE: u32 = 192_000;
const FRAMES_URL: &str = "/__hyperframes_video_frames/";

pub struct Options {
    pub project: PathBuf,
    /// The installed `hyperframes` package. Found in the project's `node_modules`, or in an
    /// ancestor's, if absent.
    pub hyperframes: Option<PathBuf>,
    pub fps: u32,
    pub mode: Mode,
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
    frames: Vec<Vec<(String, String)>>,
    tracks: Vec<HyperframesTrack>,
}

pub struct Prepare {
    pub dir: PathBuf,
    pub cached: bool,
    prepared: Prepared,
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

fn input_hash(project: &Path, hyperframes: &Path, fps: u32) -> Result<String> {
    let mut context = digest::Context::new(&digest::SHA256);
    hash_tree(&mut context, project, project)?;
    context.update(PREPARE_JS.as_bytes());
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
pub fn prepare(project: &Path, hyperframes: Option<&Path>, fps: u32) -> Result<Prepare> {
    let project = project
        .canonicalize()
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
    let hash = input_hash(&project, &hyperframes, fps)?;
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
    let partial = root.join(format!("{hash}.partial-{}", std::process::id()));
    let _ = fs::remove_dir_all(&partial);
    let output = Command::new("node")
        .arg(&script)
        .arg(&hyperframes)
        .arg(&project)
        .arg(&partial)
        .arg(fps.to_string())
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

/// Where a composition's `<video>` elements get their pictures. Frame `n` of each video comes
/// from a JPEG that HyperFrames' own extraction wrote, chosen by its own lookup (contract items 6
/// and 23). Anything that answers `at` with the same entries can replace it.
pub struct VideoFrames {
    table: Vec<Vec<(String, String)>>,
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
                    .map(|(id, path)| json!({ "id": id, "src": format!("{FRAMES_URL}{path}") }))
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
        let hints: Vec<Value> = prepared
            .video_metadata
            .iter()
            .map(|v| json!({ "id": v.id, "width": v.width, "height": v.height }))
            .collect();
        Ok(Page {
            project,
            script: format!("{early_stub}\n{HYPERFRAMES_JS}\n{CLOCK_JS}"),
            init: json!({ "fps": [options.fps, 1], "videoMetadataHints": hints }).to_string(),
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
        let meta = view
            .evaluate(&format!("__vrf.hf.init({})", self.init))
            .context("the composition did not initialise")?;
        Ok((view, serde_json::from_value(meta)?))
    }

    /// Leaves the view showing `frame`.
    pub fn seek(view: &mut View, frame: u64, videos: Option<Value>) -> Result<()> {
        let options = videos.map_or_else(|| json!({}), |videos| json!({ "videos": videos }));
        view.evaluate(&format!("__vrf.hf.seek({frame}, {options})"))?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct Stages {
    pub prepare_s: f64,
    pub prepare_cached: bool,
    pub probe_s: f64,
    pub open_s: f64,
    pub render_s: f64,
    pub audio_s: f64,
    pub finish_s: f64,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub composition: Composition,
    pub first_frame: u64,
    pub frames: u64,
    pub elapsed_s: f64,
    pub fps: f64,
    pub stages: Stages,
    pub per_frame: PerFrame,
    pub repeats: u64,
    pub capture_retries: u64,
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
/// `frames` is an inclusive range of output frames; `None` renders all of them.
pub fn render(
    ui: &Ui,
    options: &Options,
    frames: Option<RangeInclusive<u64>>,
    codec: Codec,
    out: &Path,
) -> Result<Report> {
    let started = Instant::now();
    let step = Instant::now();
    let prepare = prepare(
        &options.project,
        options.hyperframes.as_deref(),
        options.fps,
    )?;
    let (prepare_s, prepare_cached) = (step.elapsed(), prepare.cached);
    let page = Page::new(prepare, options)?;

    let step = Instant::now();
    let (probe, composition) = page.open(ui, PROBE_SIZE.0, PROBE_SIZE.1)?;
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
    let range = frames.unwrap_or(0..=composition.frames - 1);
    let (first, last) = (*range.start(), *range.end());
    ensure!(
        first <= last && last < composition.frames,
        "frames {first}-{last} are outside the composition's {} frames",
        composition.frames
    );
    let count = last - first + 1;

    let step = Instant::now();
    let (mut view, _) = page.open(ui, composition.width as i32, composition.height as i32)?;
    let open_s = step.elapsed();

    let frame_rate = Rational::new(i64::from(options.fps), 1);
    let config = Config::new(
        composition.width,
        composition.height,
        frame_rate,
        codec,
        count,
    );
    let mut pipeline = Pipeline::new(&config, out)?;
    let mut capture = Capture::default();
    let mut seek_times = Vec::with_capacity(count as usize);

    let render_started = Instant::now();
    for index in 0..count {
        let frame = first + index;
        let step = Instant::now();
        if let Err(error) = Page::seek(&mut view, frame, page.video.at(frame)) {
            let error = match view.check_project() {
                Err(refused) => error.context(refused.to_string()),
                Ok(()) => error,
            };
            return Err(error.context(format!("seek to frame {frame}")));
        }
        seek_times.push(step.elapsed());

        match capture
            .next(&view, config.watchdog)
            .with_context(|| format!("capture frame {frame}"))?
        {
            Some(paint) => pipeline.frame(index, &paint)?,
            None => pipeline.repeat(index)?,
        }
    }
    let repeats = pipeline.repeats;
    let convert = std::mem::take(&mut pipeline.convert);
    let (mut muxer, encode) = pipeline.finish()?;
    let render_s = render_started.elapsed();
    view.check_project()?;
    view.close()?;

    let step = Instant::now();
    let project_dir = options.project.canonicalize()?;
    let clips = hyperframes_clips(&page.tracks, track_file(&project_dir))?;
    let timeline = mix(&clips, audio_samples(composition.frames, frame_rate))?;
    let from = audio_samples(first, frame_rate) as usize * 2;
    let to = from + audio_samples(count, frame_rate) as usize * 2;
    add_audio(&mut muxer, &timeline[from..to], AUDIO_BIT_RATE)?;
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
        stages: Stages {
            prepare_s: prepare_s.as_secs_f64(),
            prepare_cached,
            probe_s: probe_s.as_secs_f64(),
            open_s: open_s.as_secs_f64(),
            render_s: render_s.as_secs_f64(),
            audio_s: audio_s.as_secs_f64(),
            finish_s: finish_s.as_secs_f64(),
        },
        per_frame: PerFrame {
            seek: StageStats::of(&seek_times),
            begin_to_paint: StageStats::of(&capture.begin_to_paint),
            convert: StageStats::of(&convert),
            encode: StageStats::of(&encode),
        },
        repeats,
        capture_retries: capture.retries,
    })
}
