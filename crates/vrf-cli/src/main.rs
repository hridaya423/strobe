use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use vrf_blink::adapters::{hyperframes, remotion};
use vrf_blink::{Mode, Ui};
use vrf_encode::Codec;

#[derive(Parser)]
#[command(name = "vrf", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render a Remotion or HyperFrames project to a video file.
    Render {
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
        /// Leave the audio out of the file.
        #[arg(long)]
        no_audio: bool,
        /// Print a JSON report on stdout instead of the summary line.
        #[arg(long)]
        json: bool,
    },
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

struct Rendered {
    report: serde_json::Value,
    line: String,
    json: bool,
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

fn summary(
    output: &Path,
    frames: u64,
    (width, height): (u32, u32),
    elapsed_s: f64,
    fps: f64,
) -> String {
    format!(
        "{}: {frames} frames of {width}x{height} in {elapsed_s:.2} s = {fps:.1} fps",
        output.display()
    )
}

fn execute(ui: Ui, cli: Cli) -> Result<Rendered> {
    let Command::Render {
        project,
        composition,
        output,
        codec,
        mode: _,
        frames,
        offline,
        entry,
        hyperframes: hyperframes_package,
        fps,
        props: props_argument,
        timeout,
        no_audio,
        json,
    } = cli.command;
    let mode = if offline { Mode::Offline } else { Mode::Online };
    let (report, line) = if composition.is_some() || is_remotion(&project) {
        let options = remotion::Options {
            project,
            entry,
            composition: composition.context("a Remotion project needs --composition")?,
            props: props(props_argument)?,
            mode,
            timeout: Duration::from_millis(timeout),
            audio: !no_audio,
        };
        let report = remotion::render(&ui, &options, frames, codec.into(), &output)?;
        let line = summary(
            &output,
            report.frames,
            (report.composition.width, report.composition.height),
            report.elapsed_s,
            report.fps,
        );
        (serde_json::to_value(&report)?, line)
    } else {
        let options = hyperframes::Options {
            project,
            hyperframes: hyperframes_package,
            fps,
            mode,
            audio: !no_audio,
        };
        let report = hyperframes::render(&ui, &options, frames, codec.into(), &output)?;
        let line = summary(
            &output,
            report.frames,
            (report.composition.width, report.composition.height),
            report.elapsed_s,
            report.fps,
        );
        (serde_json::to_value(&report)?, line)
    };
    Ok(Rendered { report, line, json })
}

fn main() -> ExitCode {
    let is_helper = std::env::args().any(|arg| arg.starts_with("--type="));
    let cli = (!is_helper).then(Cli::parse);
    if let Some(Cli {
        command: Command::Render {
            mode: ModeArg::Reference,
            ..
        },
    }) = &cli
    {
        vrf_blink::set_cpu_raster(true);
    }
    let outcome =
        vrf_blink::run(move |ui| execute(ui, cli.expect("a helper process never runs the worker")));
    match outcome {
        Ok(Rendered { report, line, json }) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("a report serialises")
                );
            } else {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
