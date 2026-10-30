use std::io::{IsTerminal, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use vrf_blink::adapters::hyperframes;
use vrf_blink::adapters::remotion::{self, Options, RenderJob, Renderer, VideoDecoder};
use vrf_blink::daemon::{self, Job, Reply, Request};
use vrf_blink::driver::{self, Plan};
use vrf_blink::{Mode, Ui, calibration};
use vrf_encode::Codec;

const COMMAND_START: &str = "VRF_COMMAND_START_US";
const SOFTWARE_WEBGL: &str = "VRF_SOFTWARE_WEBGL";

#[derive(Parser)]
#[command(name = "vrf", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render a Remotion or HyperFrames project to a video file.
    Render(RenderArgs),
    /// Keep the renderer warm between renders. `vrf render` uses a running daemon and falls
    /// back to rendering in its own process when there is none. Projects that use WebGL render
    /// on SwiftShader, which needs a daemon of its own: start it with `VRF_SOFTWARE_WEBGL=1`.
    Daemon(DaemonArgs),
}

#[derive(Args, Clone)]
struct RenderArgs {
    /// The project directory: a Remotion project with its dependencies installed, or a
    /// HyperFrames project (an `index.html`).
    project: PathBuf,
    /// The Remotion composition to render. Required for a Remotion project.
    #[arg(long)]
    composition: Option<String>,
    #[arg(short, long)]
    output: PathBuf,
    #[arg(long, value_enum, default_value = "h264")]
    codec: CodecArg,
    /// `reference` rasterises in software and gives the pixels of Remotion's own
    /// `chrome-headless-shell`, which the delivered-file fidelity rule needs. `fast` rasterises
    /// on the GPU, as HyperFrames' own `chrome-headless-shell` does: up to 30% quicker on
    /// raster-bound pages, but blended fills and gradients land a level off Remotion's pixels
    /// and `shape-rendering: crispEdges` is anti-aliased. Remotion projects default to
    /// `reference` and HyperFrames projects to `fast`.
    #[arg(long, value_enum)]
    mode: Option<ModeArg>,
    /// The frames to render, as `first-last` (inclusive) or one frame.
    #[arg(long, value_parser = parse_frames)]
    frames: Option<RangeInclusive<u64>>,
    /// Fail on any http(s) request that is not pinned in the project's `vrf.lock.json`.
    #[arg(long)]
    offline: bool,
    /// Remotion: the file that calls `registerRoot`, relative to the project. Found if absent.
    #[arg(long)]
    entry: Option<PathBuf>,
    /// HyperFrames: the installed `hyperframes` package. Found in `node_modules` above the
    /// project if absent.
    #[arg(long)]
    hyperframes: Option<PathBuf>,
    /// HyperFrames: frames per second.
    #[arg(long, default_value_t = 30)]
    fps: u32,
    /// Input props as JSON, or the path of a JSON file.
    #[arg(long)]
    props: Option<String>,
    /// Milliseconds before a `delayRender` that is never cleared fails the frame.
    #[arg(long, default_value_t = 30_000)]
    timeout: u64,
    /// Views rendering in parallel. Measured on this machine if absent.
    #[arg(long)]
    views: Option<usize>,
    /// Frames each view renders in a row. Measured on this machine if absent.
    #[arg(long)]
    run: Option<u64>,
    /// Frames a view seeks and paints before its run, and discards.
    #[arg(long, default_value_t = driver::LEAD_IN)]
    lead_in: u64,
    /// Leave the audio out of the file.
    #[arg(long)]
    no_audio: bool,
    /// What decodes embedded video: `native` (VideoToolbox, falling back to the framework's own
    /// decoding for clips it cannot reproduce exactly) or `remotion` (the framework's own for
    /// all: Remotion's frame server, HyperFrames' ffmpeg extraction).
    #[arg(long, value_enum, default_value = "native")]
    video: VideoArg,
    /// Render every frame, even those the page proves unchanged.
    #[arg(long)]
    no_skip: bool,
    /// Print a JSON report on stdout instead of the summary line.
    #[arg(long)]
    json: bool,
    /// Render in this process even if a daemon is running.
    #[arg(long)]
    no_daemon: bool,
    /// Encode H.264 on VideoToolbox alone, without the libx264 lane.
    #[arg(long)]
    no_turbo: bool,
}

#[derive(Args)]
struct DaemonArgs {
    /// Seconds with nothing to do before the daemon exits.
    #[arg(long, default_value_t = 600)]
    idle_timeout: u64,
    /// The rasterisation the daemon renders with. A request for the other mode renders in its
    /// own process.
    #[arg(long, value_enum, default_value = "reference")]
    mode: ModeArg,
    /// Projects whose bundle, frame server and views stay loaded.
    #[arg(long, default_value_t = 2)]
    projects: usize,
    /// Stop the running daemon after its current renders.
    #[arg(long)]
    stop: bool,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum ModeArg {
    Fast,
    Reference,
}

#[derive(Clone, Copy, ValueEnum)]
enum VideoArg {
    Native,
    Remotion,
}

#[derive(Clone, Copy, ValueEnum)]
enum CodecArg {
    H264,
    Hevc,
    Prores,
}

impl From<CodecArg> for Codec {
    fn from(codec: CodecArg) -> Codec {
        match codec {
            CodecArg::H264 => Codec::H264,
            CodecArg::Hevc => Codec::Hevc,
            CodecArg::Prores => Codec::ProRes422,
        }
    }
}

fn parse_frames(text: &str) -> Result<RangeInclusive<u64>, String> {
    let (first, last) = text.split_once('-').unwrap_or((text, text));
    let number = |part: &str| part.parse::<u64>().map_err(|e| format!("{part:?}: {e}"));
    Ok(number(first)?..=number(last)?)
}

fn props(argument: Option<String>) -> Result<Option<String>> {
    match argument {
        Some(text) if !text.trim_start().starts_with('{') => {
            let text = std::fs::read_to_string(&text)
                .with_context(|| format!("read props file {text}"))?;
            Ok(Some(text))
        }
        other => Ok(other),
    }
}

enum Work {
    Remotion(RenderJob),
    Hyperframes(hyperframes::RenderJob),
}

fn is_remotion(project: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(project.join("package.json")) else {
        return false;
    };
    let package: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    ["dependencies", "devDependencies"].iter().any(|kind| {
        package[kind]
            .as_object()
            .is_some_and(|deps| deps.contains_key("remotion"))
    })
}

impl RenderArgs {
    fn is_remotion(&self) -> bool {
        self.composition.is_some() || is_remotion(&self.project)
    }

    fn cpu_raster(&self) -> bool {
        match self.mode {
            Some(mode) => mode == ModeArg::Reference,
            None => self.is_remotion() || hyperframes::CPU_RASTER,
        }
    }

    fn remembers_webgl(&self) -> bool {
        self.composition.as_deref().is_some_and(|composition| {
            remotion::known_webgl(&self.project, self.entry.as_deref(), composition)
        })
    }

    fn work(&self, plan: Plan) -> Result<Work> {
        let project = self.project.canonicalize().with_context(|| {
            format!(
                "project directory {} does not exist",
                self.project.display()
            )
        })?;
        let mode = if self.offline {
            Mode::Offline
        } else {
            Mode::Online
        };
        let plan = Plan {
            turbo: !self.no_turbo,
            ..plan
        };
        let output = std::path::absolute(&self.output)?;
        Ok(if self.is_remotion() {
            let options = Options {
                project,
                entry: self.entry.clone(),
                composition: self
                    .composition
                    .clone()
                    .context("a Remotion project needs --composition")?,
                props: props(self.props.clone())?,
                mode,
                timeout: Duration::from_millis(self.timeout),
                plan,
                audio: !self.no_audio,
                video: match self.video {
                    VideoArg::Native => VideoDecoder::Native,
                    VideoArg::Remotion => VideoDecoder::Remotion,
                },
                skip: !self.no_skip,
            };
            Work::Remotion(RenderJob {
                options,
                frames: self.frames.clone(),
                codec: self.codec.into(),
                output,
            })
        } else {
            let options = hyperframes::Options {
                project,
                hyperframes: self.hyperframes.clone(),
                fps: self.fps,
                mode,
                plan,
                audio: !self.no_audio,
                native_video: matches!(self.video, VideoArg::Native),
                skip: !self.no_skip,
            };
            Work::Hyperframes(hyperframes::RenderJob {
                options,
                frames: self.frames.clone(),
                codec: self.codec.into(),
                output,
            })
        })
    }
}

fn progress() -> impl FnMut(u64, u64) {
    let terminal = std::io::stderr().is_terminal();
    move |frame, frames| {
        if terminal {
            let mut stderr = std::io::stderr();
            let _ = write!(stderr, "\r{frame}/{frames} frames");
            if frame == frames {
                let _ = write!(stderr, "\r\x1b[K");
            }
            let _ = stderr.flush();
        }
    }
}

fn command_start() -> SystemTime {
    if let Some(micros) = std::env::var(COMMAND_START)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
    {
        return UNIX_EPOCH + Duration::from_micros(micros);
    }
    let now = SystemTime::now();
    let micros = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    unsafe { std::env::set_var(COMMAND_START, micros.to_string()) };
    now
}

struct Rendered {
    report: serde_json::Value,
    frames: u64,
    width: u32,
    height: u32,
    first_frame_ms: f64,
    total_ms: f64,
}

impl From<remotion::Report> for Rendered {
    fn from(report: remotion::Report) -> Rendered {
        Rendered {
            report: serde_json::to_value(&report).expect("a report serialises"),
            frames: report.frames,
            width: report.composition.width,
            height: report.composition.height,
            first_frame_ms: report.first_frame_ms,
            total_ms: report.total_ms,
        }
    }
}

impl From<hyperframes::Report> for Rendered {
    fn from(report: hyperframes::Report) -> Rendered {
        Rendered {
            report: serde_json::to_value(&report).expect("a report serialises"),
            frames: report.frames,
            width: report.composition.width,
            height: report.composition.height,
            first_frame_ms: report.first_frame_ms,
            total_ms: report.total_ms,
        }
    }
}

fn render_remote(
    args: &RenderArgs,
    cpu_raster: bool,
    command_start: SystemTime,
) -> Result<Option<Rendered>> {
    let Some(stream) = daemon::connect(&daemon::socket_path()) else {
        return Ok(None);
    };
    let Some(plan) = calibration::cached_plan(args.views, args.run, args.lead_in)? else {
        return Ok(None);
    };
    let job = match args.work(plan)? {
        Work::Remotion(job) => Job::Render(job),
        Work::Hyperframes(job) => Job::Hyperframes(job),
    };
    let request = Request::new(cpu_raster, command_start, job)?;
    let mut show = progress();
    let reply = daemon::call(
        stream,
        &request,
        &mut |ahead| eprintln!("vrf: waiting for {ahead} render(s) ahead in the daemon"),
        &mut |frame, frames| show(frame, frames),
    )?;
    Ok(match reply {
        Reply::Rendered(report, _) => Some((*report).into()),
        Reply::RenderedHyperframes(report) => Some((*report).into()),
        Reply::Refused(reason) => {
            eprintln!("vrf: the daemon refused this render ({reason}); rendering in this process");
            None
        }
        Reply::Gone | Reply::Pong => None,
    })
}

fn render_local(ui: &Ui, args: &RenderArgs, command_start: SystemTime) -> Result<Rendered> {
    let plan = calibration::plan(ui, args.views, args.run, args.lead_in)?;
    let work = args.work(plan)?;
    let mut show = progress();
    let mut report = |frame, frames| {
        show(frame, frames);
        Ok(())
    };
    let mut renderer = Renderer::new(1);
    Ok(match work {
        Work::Remotion(job) => renderer
            .render(ui, &job, command_start, &mut report)?
            .into(),
        Work::Hyperframes(job) => renderer
            .render_hyperframes(ui, &job, command_start, &mut report)?
            .into(),
    })
}

fn print_report(rendered: &Rendered, args: &RenderArgs) {
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rendered.report).expect("a report serialises")
        );
    } else {
        println!(
            "{}: {} frames of {}x{} in {:.2} s = {:.1} fps, first frame at {:.0} ms",
            args.output.display(),
            rendered.frames,
            rendered.width,
            rendered.height,
            rendered.total_ms / 1000.0,
            rendered.frames as f64 / (rendered.total_ms / 1000.0),
            rendered.first_frame_ms
        );
    }
}

fn stop_daemon() -> Result<()> {
    let Some(stream) = daemon::connect(&daemon::socket_path()) else {
        println!("no daemon is running");
        return Ok(());
    };
    let request = Request::new(false, SystemTime::now(), Job::Stop)?;
    daemon::call(stream, &request, &mut |_| {}, &mut |_, _| {})?;
    println!("daemon stopped");
    Ok(())
}

fn needs_software_webgl(error: &anyhow::Error) -> bool {
    !vrf_blink::software_webgl() && format!("{error:#}").contains(remotion::NEEDS_SOFTWARE_WEBGL)
}

fn render_again_with_software_webgl() -> Result<()> {
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(std::env::args_os().skip(1))
        .env(SOFTWARE_WEBGL, "1")
        .status()?;
    std::process::exit(status.code().unwrap_or(1));
}

fn run_render(args: RenderArgs, cpu_raster: bool, command_start: SystemTime) -> Result<()> {
    let remote = if args.no_daemon || std::env::var_os("VRF_NO_DAEMON").is_some() {
        None
    } else {
        match render_remote(&args, cpu_raster, command_start) {
            Err(error) if needs_software_webgl(&error) => {
                return render_again_with_software_webgl();
            }
            remote => remote?,
        }
    };
    let report = match remote {
        Some(report) => report,
        None => {
            unsafe { std::env::set_var("VRF_NO_DAEMON", "1") };
            let outcome = vrf_blink::run({
                let args = args.clone();
                move |ui| render_local(&ui, &args, command_start)
            });
            match outcome {
                Err(error) if needs_software_webgl(&error) => {
                    return render_again_with_software_webgl();
                }
                outcome => outcome?,
            }
        }
    };
    print_report(&report, &args);
    Ok(())
}

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg.starts_with("--type=")) {
        vrf_blink::run(|_| ());
        return ExitCode::FAILURE;
    }
    let command_start = command_start();
    let outcome = match Cli::parse().command {
        Command::Render(args) => {
            let cpu_raster = args.cpu_raster();
            vrf_blink::set_cpu_raster(cpu_raster);
            vrf_blink::set_software_webgl(
                std::env::var_os(SOFTWARE_WEBGL).is_some() || args.remembers_webgl(),
            );
            run_render(args, cpu_raster, command_start)
        }
        Command::Daemon(args) if args.stop => {
            vrf_blink::set_software_webgl(std::env::var_os(SOFTWARE_WEBGL).is_some());
            stop_daemon()
        }
        Command::Daemon(args) => {
            vrf_blink::set_software_webgl(std::env::var_os(SOFTWARE_WEBGL).is_some());
            let cpu_raster = args.mode == ModeArg::Reference;
            vrf_blink::set_cpu_raster(cpu_raster);
            let config = daemon::Config {
                socket: daemon::socket_path(),
                idle: Duration::from_secs(args.idle_timeout),
                cpu_raster,
                projects: args.projects,
            };
            vrf_blink::run(move |ui| daemon::serve(&ui, &config))
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_args(project: &Path, extra: &[&str]) -> RenderArgs {
        let mut argv = vec!["vrf", "render", project.to_str().unwrap(), "-o", "out.mp4"];
        argv.extend(extra);
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Render(args) => args,
            _ => unreachable!(),
        }
    }

    fn project(name: &str, package: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vrf-cli-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(package) = package {
            std::fs::write(dir.join("package.json"), package).unwrap();
        }
        dir
    }

    #[test]
    fn a_hyperframes_project_rasterises_on_the_gpu_and_a_remotion_project_in_software() {
        let hyperframes = project("hyperframes", None);
        let remotion = project("remotion", Some(r#"{"dependencies":{"remotion":"4.0.0"}}"#));
        assert!(!render_args(&hyperframes, &[]).cpu_raster());
        assert!(render_args(&remotion, &[]).cpu_raster());
    }

    #[test]
    fn mode_overrides_the_framework_default() {
        let hyperframes = project("hyperframes-mode", None);
        assert!(render_args(&hyperframes, &["--mode", "reference"]).cpu_raster());
        let remotion = project("remotion-mode", Some(r#"{"dependencies":{"remotion":"4.0.0"}}"#));
        assert!(!render_args(&remotion, &["--mode", "fast"]).cpu_raster());
    }
}
