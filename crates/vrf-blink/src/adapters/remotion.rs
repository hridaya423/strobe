use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, ensure};
use percent_encoding::percent_decode_str;
use ring::digest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use vrf::audio::{RemotionAsset, remotion_clips};
use vrf_encode::{Codec, ColourSpace, Matrix, Range, Rational};

use super::hyperframes;
use super::{
    PerFrame, Progress, StageStats, add_track, audio_samples, cache_dir, hash_tree, short_hex,
    since,
};
use crate::driver::{Config, Plan, Runs, Warm, render_runs};
use crate::adapters::audio::LiveAudio;
use crate::skip::{self, Skip, TRACK_JS};
use crate::video::{Clips, Link, StartFallback};
use crate::{Mode, Project, Ui, View};

const VIDEO_JS: &str = include_str!("../../js/video.js");
const REMOTION_JS: &str = include_str!("../../js/remotion.js");
const CLOCK_JS: &str = include_str!("../../js/clock.js");
const BUNDLE_JS: &str = include_str!("../../js/remotion-bundle.mjs");
const SERVE_JS: &str = include_str!("../../js/remotion-serve.mjs");

const FIRST_VIEW_SIZE: (i32, i32) = (1920, 1080);
const ENTRY_CANDIDATES: [&str; 8] = [
    "src/index.ts",
    "src/index.tsx",
    "src/index.js",
    "src/index.jsx",
    "remotion/index.ts",
    "remotion/index.tsx",
    "index.ts",
    "index.js",
];
const REMOTION_COMMANDS: [&str; 6] = [
    "studio",
    "preview",
    "render",
    "bundle",
    "still",
    "compositions",
];
const RETRY_MARK: &str = "Rendering the frame will be retried";
const AUDIO_BIT_RATE: u32 = 320_000;

/// What decodes the frames of `<OffthreadVideo>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VideoDecoder {
    /// VideoToolbox in this process, drawn by the page's own WebGL surface. Clips it cannot
    /// reproduce exactly (alpha, ProRes, 10-bit, remote URLs) go to Remotion's frame server,
    /// which is started the first time one is needed.
    #[default]
    Native,
    /// Remotion's own frame server, a Node process, for every clip.
    Remotion,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub project: PathBuf,
    /// The file that calls `registerRoot`. Found from `package.json` or the usual places if absent.
    pub entry: Option<PathBuf>,
    pub composition: String,
    /// The input props as JSON text.
    pub props: Option<String>,
    pub mode: Mode,
    pub timeout: Duration,
    pub plan: Plan,
    /// Mix the composition's audio into the file. Off leaves a video-only file.
    pub audio: bool,
    pub video: VideoDecoder,
    pub skip: bool,
}

/// One render: what to render, which frames of it, the codec, and where the file goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderJob {
    pub options: Options,
    /// An inclusive range of composition frames; `None` renders all of them.
    pub frames: Option<RangeInclusive<u64>>,
    pub codec: Codec,
    pub output: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Composition {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_in_frames: u64,
}

impl Composition {
    /// The exact frame rate: a whole number, or a whole number over 1001.
    pub fn frame_rate(&self) -> Result<Rational> {
        [1, 1001]
            .into_iter()
            .map(|den| Rational::new((self.fps * den as f64).round() as i64, den))
            .find(|rate| (rate.num as f64 / rate.den as f64 - self.fps).abs() < 1e-3)
            .with_context(|| format!("frame rate {} is not a whole number or n/1001", self.fps))
    }
}

/// What Remotion registered for one frame: audio and video the final mix needs. `src` is
/// resolved, so a `same-as-` reference is replaced by the source it points at.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameAssets {
    pub frame: u64,
    pub assets: Vec<Value>,
}

/// What a render found already prepared.
#[derive(Debug, Serialize, Deserialize)]
pub struct Warmth {
    pub gpu: bool,
    pub frame_server: bool,
    pub view: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Stages {
    pub bundle_s: f64,
    pub bundle_cached: bool,
    pub frame_server_s: f64,
    pub open_s: f64,
    pub pipeline_s: f64,
    pub render_s: f64,
    pub audio_s: f64,
    pub finish_s: f64,
    pub warm: Warmth,
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
    pub seek_retries: u64,
    #[serde(skip)]
    pub assets: Vec<FrameAssets>,
}

/// The error of a render whose page creates a WebGL context while CEF runs WebGL on the GPU.
/// Remotion's own renderer runs it on SwiftShader, and a shader that hashes with `sin()` gives
/// other values there, so the caller renders again after [`crate::set_software_webgl`].
pub const NEEDS_SOFTWARE_WEBGL: &str = "the page uses WebGL; render again with software WebGL";

pub struct Bundle {
    pub dir: PathBuf,
    pub cached: bool,
}

fn entry_in_scripts(project: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(project.join("package.json")).ok()?;
    let package: Value = serde_json::from_str(&text).ok()?;
    package["scripts"].as_object()?.values().find_map(|script| {
        let words: Vec<&str> = script.as_str()?.split_whitespace().collect();
        let at = words.iter().position(|w| *w == "remotion")?;
        let command = words.get(at + 1)?;
        REMOTION_COMMANDS.contains(command).then_some(())?;
        words[at + 2..]
            .iter()
            .find(|w| {
                [".ts", ".tsx", ".js", ".jsx"]
                    .iter()
                    .any(|ext| w.ends_with(ext))
            })
            .map(PathBuf::from)
    })
}

fn find_entry(project: &Path) -> Result<PathBuf> {
    entry_in_scripts(project)
        .or_else(|| {
            ENTRY_CANDIDATES
                .iter()
                .map(PathBuf::from)
                .find(|candidate| project.join(candidate).is_file())
        })
        .with_context(|| {
            format!(
                "no Remotion entry file in {}; pass one with --entry",
                project.display()
            )
        })
}

fn input_hash(project: &Path, entry: &Path) -> Result<String> {
    let mut context = digest::Context::new(&digest::SHA256);
    hash_tree(&mut context, project, project)?;
    context.update(entry.as_os_str().as_encoded_bytes());
    context.update(BUNDLE_JS.as_bytes());
    let bundler = project.join("node_modules/@remotion/bundler/package.json");
    context.update(&fs::read(&bundler).with_context(|| {
        format!(
            "{} is missing; run the project's package install",
            bundler.display()
        )
    })?);
    Ok(short_hex(context))
}

struct Located {
    project: PathBuf,
    entry: PathBuf,
    hash: String,
    root: PathBuf,
    dir: PathBuf,
}

fn locate(project: &Path, entry: Option<&Path>) -> Result<Located> {
    let project = crate::canonical(project)
        .with_context(|| format!("project directory {} does not exist", project.display()))?;
    let entry = match entry {
        Some(entry) => entry.to_owned(),
        None => find_entry(&project)?,
    };
    let entry = if entry.is_absolute() {
        entry
    } else {
        project.join(entry)
    };
    ensure!(
        entry.is_file(),
        "entry file {} does not exist",
        entry.display()
    );
    let hash = input_hash(&project, &entry)?;
    let root = cache_dir().join("remotion");
    let dir = root.join(&hash);
    Ok(Located {
        project,
        entry,
        hash,
        root,
        dir,
    })
}

fn webgl_mark(bundle: &Path, composition: &str) -> PathBuf {
    let mut context = digest::Context::new(&digest::SHA256);
    context.update(composition.as_bytes());
    bundle.join(format!("webgl-{}", short_hex(context)))
}

fn remember_webgl(bundle: &Path, composition: &str) {
    let _ = fs::write(webgl_mark(bundle, composition), b"");
}

/// Whether an earlier render of this composition found that its page uses WebGL. The answer
/// is kept beside the bundle, so it is known before CEF starts and the render can begin on
/// SwiftShader instead of aborting on the GPU first.
pub fn known_webgl(project: &Path, entry: Option<&Path>, composition: &str) -> bool {
    locate(project, entry).is_ok_and(|found| webgl_mark(&found.dir, composition).is_file())
}

/// Bundles the project with its own `@remotion/bundler` in a Node subprocess. The result is
/// cached under `VRF_CACHE_DIR` (default: the system temp dir) by a hash of the project's files,
/// the entry, the bundler version and the bundling script.
pub fn bundle(project: &Path, entry: Option<&Path>) -> Result<Bundle> {
    let Located {
        project,
        entry,
        hash,
        root,
        dir,
    } = locate(project, entry)?;
    if dir.join("index.html").is_file() {
        return Ok(Bundle { dir, cached: true });
    }
    fs::create_dir_all(&root)?;
    let script = root.join("remotion-bundle.mjs");
    fs::write(&script, BUNDLE_JS)?;
    let partial = root.join(format!("{hash}.partial-{}", std::process::id()));
    let _ = fs::remove_dir_all(&partial);
    let output = Command::new("node")
        .arg(&script)
        .arg(&project)
        .arg(&entry)
        .arg(&partial)
        .output()
        .context("run node to bundle the project")?;
    ensure!(
        output.status.success(),
        "bundling {} failed:\n{}",
        entry.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    if let Err(error) = fs::rename(&partial, &dir) {
        ensure!(
            dir.join("index.html").is_file(),
            "cannot move the bundle into place: {error}"
        );
        let _ = fs::remove_dir_all(&partial);
    }
    Ok(Bundle { dir, cached: false })
}

/// Remotion's own frame server for `<OffthreadVideo>`, run by the project's `@remotion/renderer`
/// in a Node subprocess. The page asks it for one image per video frame.
pub struct Offthread {
    child: Child,
    stdin: Option<ChildStdin>,
    pub port: u16,
    pub serve_url: String,
}

impl Offthread {
    pub fn start(project: &Path, bundle_dir: &Path) -> Result<Offthread> {
        let script = cache_dir().join("remotion").join("remotion-serve.mjs");
        fs::create_dir_all(script.parent().context("cache path has no parent")?)?;
        fs::write(&script, SERVE_JS)?;
        let mut child = Command::new("node")
            .arg(&script)
            .arg(project)
            .arg(bundle_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("run node to serve video frames")?;
        let stdin = child.stdin.take();
        let mut line = String::new();
        BufReader::new(child.stdout.take().context("no stdout")?).read_line(&mut line)?;
        let started: Value =
            serde_json::from_str(&line).context("the frame server did not start")?;
        let port = started["offthreadPort"]
            .as_u64()
            .context("the frame server reported no port")?;
        Ok(Offthread {
            child,
            stdin,
            port: port as u16,
            serve_url: started["serveUrl"]
                .as_str()
                .context("the frame server reported no URL")?
                .to_owned(),
        })
    }
}

impl Drop for Offthread {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// Remotion's frame server, started on first use for the clips the native path refuses.
pub struct Fallback {
    project_dir: PathBuf,
    bundle_dir: PathBuf,
    project: Project,
    server: Mutex<Option<Offthread>>,
}

impl Fallback {
    pub fn new(project_dir: &Path, bundle_dir: &Path, project: Project) -> Result<Fallback> {
        Ok(Fallback {
            project_dir: crate::canonical(&project_dir)?,
            bundle_dir: bundle_dir.to_owned(),
            project,
            server: Mutex::new(None),
        })
    }

    fn port_and_url(&self) -> Result<(u16, String)> {
        let mut server = self.server.lock().unwrap();
        if server.is_none() {
            let started = Offthread::start(&self.project_dir, &self.bundle_dir)?;
            self.project
                .allow_direct(format!("http://localhost:{}/", started.port));
            *server = Some(started);
        }
        let server = server.as_ref().expect("the frame server was just started");
        Ok((server.port, server.serve_url.clone()))
    }

    /// Whether the frame server had to be started.
    pub fn started(&self) -> bool {
        self.server.lock().unwrap().is_some()
    }
}

struct Native {
    clips: Arc<Clips>,
    fallback: Arc<Fallback>,
}

/// A bundled project that can open ready-to-seek views of one composition.
pub struct Page {
    project: Project,
    config: Value,
    track: bool,
    composition: String,
    native: Option<Native>,
}

impl Page {
    pub fn new(project: Project, options: &Options, frames: Option<&Offthread>) -> Result<Page> {
        let port = frames.map_or(0, |server| server.port);
        project.allow_direct(format!("http://localhost:{port}/"));
        let config = json!({
            "proxyPort": port,
            "serveUrl": frames.map(|server| &server.serve_url),
            "inputProps": options.props.as_deref().unwrap_or("{}"),
            "timeoutInMilliseconds": options.timeout.as_millis() as u64,
            "captureMode": "raw",
        });
        Ok(Page {
            project,
            config,
            track: options.skip,
            composition: serde_json::to_string(&options.composition)?,
            native: None,
        })
    }

    /// Decodes `<OffthreadVideo>` natively instead of through Remotion's frame server, which
    /// `fallback` starts only for a clip the native path refuses.
    pub fn with_native_video(mut self, fallback: Arc<Fallback>) -> Page {
        self.config["nativeVideo"] = Value::Bool(true);
        self.native = Some(Native {
            clips: Arc::default(),
            fallback,
        });
        self
    }

    fn script(&self) -> String {
        let video = if self.native.is_some() { VIDEO_JS } else { "" };
        let track = if self.track { TRACK_JS } else { "" };
        format!(
            "globalThis.__vrf_rm_config = {};\n{video}\n{REMOTION_JS}\n{CLOCK_JS}\n{track}",
            self.config
        )
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
            Some(&self.script()),
        )?;
        self.link(&mut view);
        let meta = view
            .evaluate(&format!(
                "__vrf.rm.init({{ compositionId: {} }})",
                self.composition
            ))
            .context("the composition did not initialise")?;
        ensure!(
            crate::webgl_settled() || meta["webgl"] != true,
            "{NEEDS_SOFTWARE_WEBGL}"
        );
        Ok((view, serde_json::from_value(meta)?))
    }

    fn link(&self, view: &mut View) {
        if let Some(native) = &self.native {
            let fallback = native.fallback.clone();
            let start: StartFallback = Arc::new(move || fallback.port_and_url());
            Link::attach(view, self.project.clone(), native.clips.clone(), start);
        }
    }

    /// Opens a view at a guessed size and resolves the composition in it without mounting it, so
    /// the size is known before the page draws anything. [`Page::fit`] finishes the job.
    fn open_unmounted(&self, ui: &Ui) -> Result<(View, Composition)> {
        let mut view = View::launch_project(
            ui,
            &self.project,
            "index.html",
            FIRST_VIEW_SIZE.0,
            FIRST_VIEW_SIZE.1,
            Some(&self.script()),
        )?;
        self.link(&mut view);
        let meta = view
            .evaluate(&format!(
                "__vrf.rm.resolve({{ compositionId: {} }})",
                self.composition
            ))
            .context("the composition did not resolve")?;
        Ok((view, serde_json::from_value(meta)?))
    }

    /// Resizes a view from [`Page::open_unmounted`] to the composition's size before anything
    /// has painted, settles begin frames at that size and mounts the composition.
    fn fit(view: &mut View, composition: &Composition) -> Result<()> {
        if (composition.width as i32, composition.height as i32) != FIRST_VIEW_SIZE {
            view.resize(composition.width as i32, composition.height as i32);
        }
        view.settle()?;
        view.evaluate("__vrf.rm.mount()")
            .context("the composition did not mount")?;
        view.settle()?;
        ensure!(
            crate::webgl_settled() || view.evaluate("__vrf.rm.webgl()")? != true,
            "{NEEDS_SOFTWARE_WEBGL}"
        );
        Ok(())
    }

    /// Leaves the view showing `frame` once Remotion's barrier has cleared, and returns the
    /// assets the frame registered. Frames must be seeked in increasing order for Remotion's
    /// output to match its own renderer.
    pub fn seek(
        view: &mut View,
        frame: u64,
        attempt: u32,
        ask: bool,
    ) -> Result<(Vec<Value>, bool)> {
        let (result, unchanged) = skip::evaluate(
            view,
            &format!("__vrf.rm.seek({frame}, {{ attempt: {attempt} }})"),
            ask,
        )?;
        ensure!(
            result["webgl"] != true || crate::webgl_settled(),
            "{NEEDS_SOFTWARE_WEBGL}"
        );
        Ok((serde_json::from_value(result["assets"].clone())?, unchanged))
    }
}

fn project_file(url: &str, roots: &[&Path]) -> Result<PathBuf> {
    let relative = url
        .strip_prefix("vrf://project/")
        .map(|rest| rest.split(['?', '#']).next().unwrap_or_default())
        .and_then(|rest| percent_decode_str(rest).decode_utf8().ok())
        .with_context(|| format!("audio source {url} is not a file of the project"))?;
    roots
        .iter()
        .map(|root| root.join(&*relative))
        .find(|path| path.is_file())
        .with_context(|| format!("audio source {url} is not in the project or its bundle"))
}

fn audio_assets(assets: &[Value]) -> Result<Vec<RemotionAsset>, serde_json::Error> {
    assets
        .iter()
        .filter(|asset| matches!(asset["type"].as_str(), Some("audio" | "video")))
        .map(|asset| serde_json::from_value::<RemotionAsset>(asset.clone()))
        .collect()
}

fn resolve_references(frames: &mut [FrameAssets]) {
    let sources: HashMap<(String, u64), String> = frames
        .iter()
        .flat_map(|f| &f.assets)
        .filter_map(|asset| {
            let src = asset["src"]
                .as_str()
                .filter(|s| !s.starts_with("same-as-"))?;
            Some((
                (asset["id"].as_str()?.to_owned(), asset["frame"].as_u64()?),
                src.to_owned(),
            ))
        })
        .collect();
    for asset in frames.iter_mut().flat_map(|f| &mut f.assets) {
        let Some(reference) = asset["src"]
            .as_str()
            .and_then(|s| s.strip_prefix("same-as-"))
        else {
            continue;
        };
        let source = reference
            .rsplit_once('-')
            .and_then(|(id, frame)| sources.get(&(id.to_owned(), frame.parse().ok()?)));
        if let Some(source) = source.cloned() {
            asset["src"] = Value::String(source);
        }
    }
}

#[derive(PartialEq)]
struct Key {
    project: PathBuf,
    bundle: PathBuf,
    composition: String,
    props: Option<String>,
    timeout: Duration,
    mode: Mode,
    video: VideoDecoder,
    skip: bool,
}

struct Loaded {
    key: Key,
    _frame_server: Option<Offthread>,
    page: Page,
    composition: Option<Composition>,
    spare: Option<View>,
}

impl Loaded {
    fn fill(&mut self, ui: &Ui) -> Result<()> {
        let Some(composition) = self.composition else {
            return Ok(());
        };
        if self.spare.is_none() {
            let (view, _) =
                self.page
                    .open(ui, composition.width as i32, composition.height as i32)?;
            self.spare = Some(view);
        }
        Ok(())
    }

    fn take_view(&mut self) -> Option<View> {
        let mut view = self.spare.take()?;
        view.is_alive().then_some(view)
    }
}

enum Ready {
    Warm(View),
    Open,
    Unmounted(View),
}

/// Renders Remotion compositions and keeps what the next render can reuse: the GPU state and
/// pixel pools, and for the last few projects their bundle, frame server and one spare ready
/// view. A render with several views takes the spare for its first and opens the rest. A
/// one-shot caller drops it after one render.
pub struct Renderer {
    warm: Option<Warm>,
    loaded: Vec<Loaded>,
    projects: usize,
}

impl Renderer {
    /// Keeps `projects` loaded projects.
    pub fn new(projects: usize) -> Renderer {
        Renderer {
            warm: None,
            loaded: Vec::new(),
            projects,
        }
    }

    /// Creates the GPU state and a 1080p pixel pool now rather than in the first render.
    pub fn prepare(&mut self) -> Result<()> {
        let warm = match self.warm.as_mut() {
            Some(warm) => warm,
            None => self.warm.insert(Warm::new()?),
        };
        warm.prepare(1920, 1080, ColourSpace::BT709)?;
        warm.prepare(1920, 1080, colour(Codec::H264))
    }

    /// Renders a HyperFrames composition on the same GPU state and pixel pools. Nothing of the
    /// project stays loaded: its preparation is cached on disk and its views are opened fresh.
    pub fn render_hyperframes(
        &mut self,
        ui: &Ui,
        job: &hyperframes::RenderJob,
        command_start: SystemTime,
        progress: &mut (dyn FnMut(u64, u64) -> Result<()> + Send),
    ) -> Result<hyperframes::Report> {
        let warm = match self.warm.as_mut() {
            Some(warm) => warm,
            None => self.warm.insert(Warm::new()?),
        };
        hyperframes::render(ui, job, warm, command_start, progress)
    }

    /// Opens the spare view for the project rendered last, so the next render of it starts on a
    /// loaded page.
    pub fn refill(&mut self, ui: &Ui) -> Result<()> {
        let Some(loaded) = self.loaded.last_mut() else {
            return Ok(());
        };
        loaded.fill(ui).inspect_err(|_| {
            self.loaded.pop();
        })
    }

    /// Closes every kept view and frame server, and returns once the views are closed.
    pub fn clear(&mut self) {
        for loaded in self.loaded.drain(..) {
            if let Some(view) = loaded.spare {
                let _ = view.close();
            }
        }
    }

    /// Renders a Remotion composition to a video file: bundle, serve through `vrf://project/`,
    /// read the composition's size, rate and length from the page, then drive each frame through
    /// the page's barrier, one begin frame, the GPU convert, the encoder and the muxer.
    ///
    /// Each view seeks `options.plan.lead_in` frames before its run without capturing them,
    /// because Remotion's output depends on the frames before it. A frame that fails with
    /// `delayRender` retries left is retried on a fresh page with the next attempt number.
    ///
    /// `command_start` is when the user's command began; `first_frame_ms` and `setup_ms` count
    /// from it. `progress` gets the frames done and the total, and a failure from it stops the
    /// render.
    pub fn render(
        &mut self,
        ui: &Ui,
        job: &RenderJob,
        command_start: SystemTime,
        progress: &mut (dyn FnMut(u64, u64) -> Result<()> + Send),
    ) -> Result<Report> {
        let RenderJob {
            options,
            frames,
            codec,
            output: out,
        } = job;
        let (codec, out) = (*codec, out.as_path());
        let started = Instant::now();
        let gpu_task = self.warm.is_none().then(|| thread::spawn(Warm::new));

        let step = Instant::now();
        let bundle = bundle(&options.project, options.entry.as_deref())?;
        let bundle_s = step.elapsed();

        let project_dir = crate::canonical(&options.project)?;
        let key = Key {
            project: project_dir,
            bundle: bundle.dir.clone(),
            composition: options.composition.clone(),
            props: options.props.clone(),
            timeout: options.timeout,
            mode: options.mode,
            video: options.video,
            skip: options.skip,
        };
        let kept = self
            .loaded
            .iter()
            .position(|loaded| loaded.key == key)
            .map(|at| self.loaded.remove(at));
        let frame_server_warm = kept.is_some();
        let step = Instant::now();
        let mut loaded = match kept {
            Some(loaded) => loaded,
            None => {
                let project = Project::open(&bundle.dir, options.mode)?;
                let frame_server = match options.video {
                    VideoDecoder::Remotion => Some(Offthread::start(&key.project, &bundle.dir)?),
                    VideoDecoder::Native => None,
                };
                let mut page = Page::new(project.clone(), options, frame_server.as_ref())?;
                if options.video == VideoDecoder::Native {
                    let fallback = Fallback::new(&key.project, &bundle.dir, project)?;
                    page = page.with_native_video(Arc::new(fallback));
                }
                Loaded {
                    key,
                    _frame_server: frame_server,
                    page,
                    composition: None,
                    spare: None,
                }
            }
        };
        let frame_server_s = step.elapsed();
        loaded.page.project.clear_failures();

        let open_started = Instant::now();
        let (composition, ready) = match (loaded.composition, loaded.take_view()) {
            (Some(composition), Some(view)) => (composition, Ready::Warm(view)),
            (Some(composition), None) => (composition, Ready::Open),
            (None, _) => {
                let (view, composition) = loaded.page.open_unmounted(ui)?;
                (composition, Ready::Unmounted(view))
            }
        };
        loaded.composition = Some(composition);
        let view_warm = matches!(ready, Ready::Warm(_));
        ensure!(
            composition.width % 2 == 0 && composition.height % 2 == 0,
            "{}x{} has an odd side; padding to even is not implemented",
            composition.width,
            composition.height
        );
        let range = frames
            .clone()
            .unwrap_or(0..=composition.duration_in_frames - 1);
        let (first, last) = (*range.start(), *range.end());
        ensure!(
            first <= last && last < composition.duration_in_frames,
            "frames {first}-{last} are outside the composition's {} frames",
            composition.duration_in_frames
        );
        let count = last - first + 1;

        let gpu_warm = gpu_task.is_none();
        let warm = match gpu_task {
            Some(task) => self
                .warm
                .insert(task.join().map_err(|_| anyhow!("GPU setup panicked"))??),
            None => self.warm.as_mut().context("the GPU state is missing")?,
        };
        let mut config = Config::new(
            composition.width,
            composition.height,
            composition.frame_rate()?,
            codec,
            count,
        );
        config.skip = options.skip.then(Skip::tracked);
        config.colour = colour(codec);
        let (width, height) = (composition.width as i32, composition.height as i32);
        let page = &loaded.page;
        let live = options.audio.then(|| {
            let (bundle_dir, project) = (bundle.dir.clone(), loaded.key.project.clone());
            LiveAudio::start(
                count as usize,
                composition.fps,
                audio_samples(count, config.frame_rate),
                AUDIO_BIT_RATE,
                move |url| project_file(url, &[&bundle_dir, &project]),
            )
        });
        let seek_retries = AtomicU64::new(0);
        let first_ready = OnceLock::new();
        let progress = Progress::new(progress, count, command_start);
        let render_started = Instant::now();
        let runs = thread::scope(|scope| {
            let prepared = Mutex::new(Some(scope.spawn(|| match ready {
                Ready::Warm(view) => Ok(view),
                Ready::Open => page.open(ui, width, height).map(|(view, _)| view),
                Ready::Unmounted(mut view) => {
                    Page::fit(&mut view, &composition)?;
                    Ok(view)
                }
            })));
            render_runs(
                &config,
                options.plan,
                first,
                out,
                warm,
                || {
                    let task = prepared.lock().unwrap().take();
                    let view = match task {
                        Some(task) => task
                            .join()
                            .map_err(|_| anyhow!("opening the view panicked"))??,
                        None => page.open(ui, width, height)?.0,
                    };
                    first_ready.get_or_init(|| (since(command_start), open_started.elapsed()));
                    Ok(view)
                },
                |view, frame, ask| {
                    let mut attempt = 1;
                    loop {
                        match Page::seek(view, frame, attempt, ask) {
                            Ok((assets, unchanged)) => {
                                return Ok((FrameAssets { frame, assets }, unchanged));
                            }
                            Err(error) if error.to_string().contains(RETRY_MARK) => {
                                let (fresh, _) = page.open(ui, width, height)?;
                                std::mem::replace(view, fresh).close()?;
                                attempt += 1;
                                seek_retries.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(error) => {
                                return Err(match view.check_project() {
                                    Err(refused) => error.context(refused.to_string()),
                                    Ok(()) => error,
                                });
                            }
                        }
                    }
                },
                |done, index, seen: &FrameAssets| {
                    if let Some(live) = &live {
                        live.report(index as usize, audio_assets(&seen.assets).unwrap_or_default());
                    }
                    progress.frame(done)
                },
            )
        })
        .inspect_err(|error| {
            if format!("{error:#}").contains(NEEDS_SOFTWARE_WEBGL) {
                remember_webgl(&bundle.dir, &options.composition);
            }
        })?;
        let render_s = render_started.elapsed();
        let Runs {
            plan,
            mut muxer,
            frames: mut collected,
            stats,
            lead_in_frames,
        } = runs;

        resolve_references(&mut collected);
        let step = Instant::now();
        if let Some(live) = live {
            let per_frame_assets = collected
                .iter()
                .map(|f| audio_assets(&f.assets))
                .collect::<Result<Vec<_>, _>>()?;
            let clips = remotion_clips(&per_frame_assets, composition.fps, |url| {
                project_file(url, &[&bundle.dir, &loaded.key.project])
            })?;
            add_track(&mut muxer, live.finish(&clips)?)?;
        }
        let audio_s = step.elapsed();
        let step = Instant::now();
        muxer.finish()?;
        let finish_s = step.elapsed();

        self.loaded.push(loaded);
        while self.loaded.len() > self.projects {
            self.loaded.remove(0);
        }

        let elapsed = started.elapsed();
        let (setup_ms, open_s) = first_ready.get().copied().unwrap_or_default();
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
            setup_ms,
            first_frame_ms: progress.first_frame_ms(),
            total_ms: since(command_start),
            stages: Stages {
                bundle_s: bundle_s.as_secs_f64(),
                bundle_cached: bundle.cached,
                frame_server_s: frame_server_s.as_secs_f64(),
                open_s: open_s.as_secs_f64(),
                pipeline_s: stats.pipeline.as_secs_f64(),
                render_s: render_s.as_secs_f64(),
                audio_s: audio_s.as_secs_f64(),
                finish_s: finish_s.as_secs_f64(),
                warm: Warmth {
                    gpu: gpu_warm,
                    frame_server: frame_server_warm,
                    view: view_warm,
                },
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
            seek_retries: seek_retries.into_inner(),
            assets: collected,
        })
    }
}

pub(crate) fn colour(codec: Codec) -> ColourSpace {
    match codec {
        Codec::H264 | Codec::Hevc => ColourSpace {
            matrix: Matrix::Bt601,
            range: Range::Full,
        },
        Codec::Av1 | Codec::ProRes422 => ColourSpace::BT709,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_composition_that_used_webgl_is_remembered_for_its_bundle() -> Result<()> {
        let project = std::env::temp_dir().join(format!("vrf-known-webgl-{}", std::process::id()));
        let bundler = project.join("node_modules/@remotion/bundler");
        fs::create_dir_all(&bundler)?;
        fs::create_dir_all(project.join("src"))?;
        fs::write(bundler.join("package.json"), "{}")?;
        fs::write(project.join("src/index.ts"), "")?;
        let found = locate(&project, None)?;
        fs::create_dir_all(&found.dir)?;
        assert!(!known_webgl(&project, None, "Shader"));
        remember_webgl(&found.dir, "Shader");
        assert!(known_webgl(&project, None, "Shader"));
        assert!(!known_webgl(&project, None, "Other"));
        fs::write(project.join("src/index.ts"), "changed")?;
        assert!(!known_webgl(&project, None, "Shader"));
        fs::remove_dir_all(&found.dir)?;
        fs::remove_dir_all(&project)?;
        Ok(())
    }
}
