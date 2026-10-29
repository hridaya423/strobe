use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use vrf_blink::adapters::remotion::{self, Options, Report, VideoDecoder};
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
    /// Render a Remotion project to a video file.
    Render {
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
        /// What decodes <OffthreadVideo>: `native` (VideoToolbox, falling back to Remotion's frame
        /// server for clips it cannot reproduce exactly) or `remotion` (its frame server for all).
        #[arg(long, value_enum, default_value = "native")]
        video: VideoArg,
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

struct Rendered {
    report: Report,
    output: PathBuf,
    json: bool,
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
        props: props_argument,
        timeout,
        video,
        json,
    } = cli.command;
    let options = Options {
        project,
        entry,
        composition,
        props: props(props_argument)?,
        mode: if offline { Mode::Offline } else { Mode::Online },
        timeout: Duration::from_millis(timeout),
        video: match video {
            VideoArg::Native => VideoDecoder::Native,
            VideoArg::Remotion => VideoDecoder::Remotion,
        },
    };
    let report = remotion::render(&ui, &options, frames, codec.into(), &output)?;
    Ok(Rendered {
        report,
        output,
        json,
    })
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
        Ok(Rendered {
            report,
            output,
            json,
        }) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("a report serialises")
                );
            } else {
                println!(
                    "{}: {} frames of {}x{} in {:.2} s = {:.1} fps",
                    output.display(),
                    report.frames,
                    report.composition.width,
                    report.composition.height,
                    report.elapsed_s,
                    report.fps
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
