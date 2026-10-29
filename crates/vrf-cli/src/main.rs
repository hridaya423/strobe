use std::io::{IsTerminal, Write};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use vrf_blink::adapters::remotion::{Options, RenderJob, Renderer, Report};
use vrf_blink::daemon::{self, Job, Reply, Request};
use vrf_blink::driver::{self, Plan};
use vrf_blink::{Mode, Ui, calibration};
use vrf_encode::Codec;

const COMMAND_START: &str = "VRF_COMMAND_START_US";

#[derive(Parser)]
#[command(name = "vrf", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render a Remotion project to a video file.
    Render(RenderArgs),
    /// Keep the renderer warm between renders. `vrf render` uses a running daemon and falls
    /// back to rendering in its own process when there is none.
    Daemon(DaemonArgs),
}

#[derive(Args, Clone)]
struct RenderArgs {
    /// The Remotion project directory, with its dependencies installed.
    project: PathBuf,
    #[arg(long)]
    composition: String,
    #[arg(short, long)]
    output: PathBuf,
    #[arg(long, value_enum, default_value = "h264")]
    codec: CodecArg,
    /// `fast` rasterises on the GPU. `reference` rasterises in software and gives the pixels
    /// of Remotion's own `chrome-headless-shell`.
    #[arg(long, value_enum, default_value = "fast")]
    mode: ModeArg,
    /// The frames to render, as `first-last` (inclusive) or one frame.
    #[arg(long, value_parser = parse_frames)]
    frames: Option<RangeInclusive<u64>>,
    /// Fail on any http(s) request that is not pinned in the project's `vrf.lock.json`.
    #[arg(long)]
    offline: bool,
    /// The file that calls `registerRoot`, relative to the project. Found if absent.
    #[arg(long)]
    entry: Option<PathBuf>,
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
    /// Render every frame, even those the page proves unchanged.
    #[arg(long)]
    no_skip: bool,
    /// Print a JSON report on stdout instead of the summary line.
    #[arg(long)]
    json: bool,
    /// Render in this process even if a daemon is running.
    #[arg(long)]
    no_daemon: bool,
}

#[derive(Args)]
struct DaemonArgs {
    /// Seconds with nothing to do before the daemon exits.
    #[arg(long, default_value_t = 600)]
    idle_timeout: u64,
    /// The rasterisation the daemon renders with. A request for the other mode renders in its
    /// own process.
    #[arg(long, value_enum, default_value = "fast")]
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

impl RenderArgs {
    fn job(&self, plan: Plan) -> Result<RenderJob> {
        let project = self.project.canonicalize().with_context(|| {
            format!(
                "project directory {} does not exist",
                self.project.display()
            )
        })?;
        let options = Options {
            project,
            entry: self.entry.clone(),
            composition: self.composition.clone(),
            props: props(self.props.clone())?,
            mode: if self.offline {
                Mode::Offline
            } else {
                Mode::Online
            },
            timeout: Duration::from_millis(self.timeout),
            plan,
            skip: !self.no_skip,
        };
        Ok(RenderJob {
            options,
            frames: self.frames.clone(),
            codec: self.codec.into(),
            output: std::path::absolute(&self.output)?,
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

fn render_remote(
    args: &RenderArgs,
    cpu_raster: bool,
    command_start: SystemTime,
) -> Result<Option<Report>> {
    let Some(stream) = daemon::connect(&daemon::socket_path()) else {
        return Ok(None);
    };
    let Some(plan) = calibration::cached_plan(args.views, args.run, args.lead_in)? else {
        return Ok(None);
    };
    let request = Request::new(cpu_raster, command_start, Job::Render(args.job(plan)?))?;
    let mut show = progress();
    let reply = daemon::call(
        stream,
        &request,
        &mut |ahead| eprintln!("vrf: waiting for {ahead} render(s) ahead in the daemon"),
        &mut |frame, frames| show(frame, frames),
    )?;
    Ok(match reply {
        Reply::Rendered(report, _) => Some(*report),
        Reply::Refused(reason) => {
            eprintln!("vrf: the daemon refused this render ({reason}); rendering in this process");
            None
        }
        Reply::Gone | Reply::Pong => None,
    })
}

fn render_local(ui: &Ui, args: &RenderArgs, command_start: SystemTime) -> Result<Report> {
    let plan = calibration::plan(ui, args.views, args.run, args.lead_in)?;
    let job = args.job(plan)?;
    let mut show = progress();
    Renderer::new(1).render(ui, &job, command_start, &mut |frame, frames| {
        show(frame, frames);
        Ok(())
    })
}

fn print_report(report: &Report, args: &RenderArgs) {
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(report).expect("a report serialises")
        );
    } else {
        println!(
            "{}: {} frames of {}x{} in {:.2} s = {:.1} fps, first frame at {:.0} ms",
            args.output.display(),
            report.frames,
            report.composition.width,
            report.composition.height,
            report.total_ms / 1000.0,
            report.frames as f64 / (report.total_ms / 1000.0),
            report.first_frame_ms
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

fn run_render(args: RenderArgs, cpu_raster: bool, command_start: SystemTime) -> Result<()> {
    let remote = if args.no_daemon || std::env::var_os("VRF_NO_DAEMON").is_some() {
        None
    } else {
        render_remote(&args, cpu_raster, command_start)?
    };
    let report = match remote {
        Some(report) => report,
        None => {
            unsafe { std::env::set_var("VRF_NO_DAEMON", "1") };
            vrf_blink::run({
                let args = args.clone();
                move |ui| render_local(&ui, &args, command_start)
            })?
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
            let cpu_raster = args.mode == ModeArg::Reference;
            vrf_blink::set_cpu_raster(cpu_raster);
            run_render(args, cpu_raster, command_start)
        }
        Command::Daemon(args) if args.stop => stop_daemon(),
        Command::Daemon(args) => {
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
