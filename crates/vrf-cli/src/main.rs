use std::io::{IsTerminal, Write};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use vrf_blink::adapters::remotion::{Options, RenderJob, Renderer, Report};
use vrf_blink::driver;
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
    /// Print a JSON report on stdout instead of the summary line.
    #[arg(long)]
    json: bool,
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
    fn job(&self, ui: &Ui) -> Result<RenderJob> {
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
            plan: calibration::plan(ui, self.views, self.run, self.lead_in)?,
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

fn render_local(ui: &Ui, args: &RenderArgs, command_start: SystemTime) -> Result<Report> {
    let job = args.job(ui)?;
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

fn run_render(args: RenderArgs, command_start: SystemTime) -> Result<()> {
    let report = vrf_blink::run({
        let args = args.clone();
        move |ui| render_local(&ui, &args, command_start)
    })?;
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
            vrf_blink::set_cpu_raster(args.mode == ModeArg::Reference);
            run_render(args, command_start)
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
