use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, ensure};
use ring::digest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use vrf_encode::{Codec, Rational};

use crate::driver::{Config, Plan, Runs, Summary, Warm, render_runs};
use crate::{Mode, Project, Ui, View};

const REMOTION_JS: &str = include_str!("../../js/remotion.js");
const CLOCK_JS: &str = include_str!("../../js/clock.js");
const BUNDLE_JS: &str = include_str!("../../js/remotion-bundle.mjs");
const SERVE_JS: &str = include_str!("../../js/remotion-serve.mjs");

const FIRST_VIEW_SIZE: (i32, i32) = (1920, 1080);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);
const NOT_HASHED: [&str; 5] = ["node_modules", ".git", ".vrf", ".cache", "out"];
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

#[derive(Debug, Serialize, Deserialize)]
pub struct StageStats {
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl StageStats {
    fn of(samples: &[Duration]) -> StageStats {
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
    pub finish_s: f64,
    pub warm: Warmth,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PerFrame {
    pub seek: StageStats,
    pub begin_to_paint: StageStats,
    pub convert: StageStats,
    pub encode: StageStats,
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
    pub capture_retries: u64,
    pub seek_retries: u64,
    #[serde(skip)]
    pub assets: Vec<FrameAssets>,
}

pub struct Bundle {
    pub dir: PathBuf,
    pub cached: bool,
}

fn cache_dir() -> PathBuf {
    std::env::var_os("VRF_CACHE_DIR")
        .map_or_else(|| std::env::temp_dir().join("vrf-cache"), PathBuf::from)
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

fn hash_tree(context: &mut digest::Context, root: &Path, dir: &Path) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if NOT_HASHED
            .iter()
            .any(|name| entry.file_name() == OsStr::new(name))
        {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            hash_tree(context, root, &path)?;
            continue;
        }
        let metadata = fs::metadata(&path)?;
        let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
        context.update(path.strip_prefix(root)?.as_os_str().as_encoded_bytes());
        context.update(&metadata.len().to_le_bytes());
        context.update(&modified.to_le_bytes());
    }
    Ok(())
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
    Ok(context.finish().as_ref()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Bundles the project with its own `@remotion/bundler` in a Node subprocess. The result is
/// cached under `VRF_CACHE_DIR` (default: the system temp dir) by a hash of the project's files,
/// the entry, the bundler version and the bundling script.
pub fn bundle(project: &Path, entry: Option<&Path>) -> Result<Bundle> {
    let project = project
        .canonicalize()
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

/// A bundled project that can open ready-to-seek views of one composition.
pub struct Page {
    project: Project,
    script: String,
    composition: String,
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
            script: format!("globalThis.__vrf_rm_config = {config};\n{REMOTION_JS}\n{CLOCK_JS}"),
            composition: serde_json::to_string(&options.composition)?,
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
            .evaluate(&format!(
                "__vrf.rm.init({{ compositionId: {} }})",
                self.composition
            ))
            .context("the composition did not initialise")?;
        Ok((view, serde_json::from_value(meta)?))
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
            Some(&self.script),
        )?;
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
        Ok(())
    }

    /// Leaves the view showing `frame` once Remotion's barrier has cleared, and returns the
    /// assets the frame registered. Frames must be seeked in increasing order for Remotion's
    /// output to match its own renderer.
    pub fn seek(view: &mut View, frame: u64, attempt: u32) -> Result<Vec<Value>> {
        let result = view.evaluate(&format!("__vrf.rm.seek({frame}, {{ attempt: {attempt} }})"))?;
        Ok(serde_json::from_value(result["assets"].clone())?)
    }
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
}

struct Loaded {
    key: Key,
    _frame_server: Offthread,
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

fn since(start: SystemTime) -> f64 {
    start.elapsed().unwrap_or_default().as_secs_f64() * 1000.0
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
        warm.prepare(1920, 1080)
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

        let project_dir = options.project.canonicalize()?;
        let key = Key {
            project: project_dir,
            bundle: bundle.dir.clone(),
            composition: options.composition.clone(),
            props: options.props.clone(),
            timeout: options.timeout,
            mode: options.mode,
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
                let frame_server = Offthread::start(&key.project, &bundle.dir)?;
                let page = Page::new(
                    Project::open(&bundle.dir, options.mode)?,
                    options,
                    Some(&frame_server),
                )?;
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
        let config = Config::new(
            composition.width,
            composition.height,
            composition.frame_rate()?,
            codec,
            count,
        );
        let (width, height) = (composition.width as i32, composition.height as i32);
        let page = &loaded.page;
        let seek_retries = AtomicU64::new(0);
        let first_ready = OnceLock::new();
        let first_frame_ms = OnceLock::new();
        let progress = Mutex::new((progress, Instant::now()));
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
                    let view = match prepared.lock().unwrap().take() {
                        Some(task) => task
                            .join()
                            .map_err(|_| anyhow!("opening the view panicked"))??,
                        None => page.open(ui, width, height)?.0,
                    };
                    first_ready.get_or_init(|| (since(command_start), open_started.elapsed()));
                    Ok(view)
                },
                |view, frame| {
                    let mut attempt = 1;
                    loop {
                        match Page::seek(view, frame, attempt) {
                            Ok(assets) => return Ok(FrameAssets { frame, assets }),
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
                |done| {
                    first_frame_ms.get_or_init(|| since(command_start));
                    let mut guard = progress.lock().unwrap();
                    let (callback, last) = &mut *guard;
                    if last.elapsed() >= PROGRESS_INTERVAL || done == count {
                        callback(done, count)?;
                        *last = Instant::now();
                    }
                    Ok(())
                },
            )
        })?;
        let render_s = render_started.elapsed();
        let Runs {
            plan,
            muxer,
            frames: mut collected,
            stats,
            lead_in_frames,
        } = runs;
        let step = Instant::now();
        muxer.finish()?;
        let finish_s = step.elapsed();

        self.loaded.push(loaded);
        while self.loaded.len() > self.projects {
            self.loaded.remove(0);
        }

        resolve_references(&mut collected);
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
            first_frame_ms: first_frame_ms.get().copied().unwrap_or_default(),
            total_ms: since(command_start),
            stages: Stages {
                bundle_s: bundle_s.as_secs_f64(),
                bundle_cached: bundle.cached,
                frame_server_s: frame_server_s.as_secs_f64(),
                open_s: open_s.as_secs_f64(),
                pipeline_s: stats.pipeline.as_secs_f64(),
                render_s: render_s.as_secs_f64(),
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
            capture_retries: stats.retries,
            seek_retries: seek_retries.into_inner(),
            assets: collected,
        })
    }
}
