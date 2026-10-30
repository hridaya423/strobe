use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use vrf::mux::{Container, Muxer};
use vrf::sink::Sink;
use vrf_encode::nvenc::{NvencEncoder, Options, Preset, RateControl, Tuning, nvidia_device};
use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Rational};

use super::win::Uploader;

pub fn main() -> Result<()> {
    let mut args = std::env::args().skip(2);
    let (mut input, mut out) = (None, None);
    let (mut size, mut fps, mut frames) = ((1920usize, 1080usize), 30i64, None);
    let mut options = Options::from(vrf_encode::Quality::Balanced);
    let mut codec = Codec::H264;
    while let Some(flag) = args.next() {
        let mut value = || args.next().context("flag needs a value");
        match flag.as_str() {
            "--input" => input = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--fps" => fps = value()?.parse()?,
            "--frames" => frames = Some(value()?.parse::<usize>()?),
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').context("WxH")?;
                size = (w.parse()?, h.parse()?);
            }
            "--codec" => {
                codec = match value()?.as_str() {
                    "h264" => Codec::H264,
                    "hevc" => Codec::Hevc,
                    "av1" => Codec::Av1,
                    other => bail!("codec {other}"),
                }
            }
            "--preset" => {
                options.preset = match value()?.as_str() {
                    "p1" => Preset::P1,
                    "p2" => Preset::P2,
                    "p3" => Preset::P3,
                    "p4" => Preset::P4,
                    "p5" => Preset::P5,
                    "p6" => Preset::P6,
                    "p7" => Preset::P7,
                    other => bail!("preset {other}"),
                }
            }
            "--tuning" => {
                options.tuning = match value()?.as_str() {
                    "hq" => Tuning::HighQuality,
                    "ll" => Tuning::LowLatency,
                    "ull" => Tuning::UltraLowLatency,
                    other => bail!("tuning {other}"),
                }
            }
            "--cq" => options.rate_control = RateControl::ConstantQuality(value()?.parse()?),
            other => bail!("unknown flag {other}"),
        }
    }
    let (input, out) = (input.context("--input")?, out.context("--out")?);
    let (width, height) = size;
    let device = nvidia_device()?;
    let encoder = NvencEncoder::with_pool_size(
        &device,
        EncoderConfig::new(codec, width as u32, height as u32, Rational::new(fps, 1)),
        options,
        8,
    )?;
    let mut encoder = encoder;
    let mut uploader = Uploader::new(&device, width as u32, height as u32)?;
    let mut command = Command::new("ffmpeg");
    command.args(["-v", "error"]);
    if input.to_string_lossy().contains('%') {
        command.args(["-framerate", &fps.to_string()]);
    }
    command.arg("-i").arg(&input);
    command.args(["-vf", "scale=out_color_matrix=bt709:out_range=tv"]);
    if let Some(frames) = frames {
        command.args(["-frames:v", &frames.to_string()]);
    }
    command.args(["-pix_fmt", "nv12", "-f", "rawvideo", "-"]);
    let mut child = command
        .stdout(Stdio::piped())
        .spawn()
        .context("spawning ffmpeg")?;
    let mut stdout = child.stdout.take().unwrap();
    let mut sink = Sink::new(Muxer::create(&out, Container::Mp4, fps as u32)?, 1);
    let mut luma = vec![0u8; width * height];
    let mut chroma = vec![0u8; width * height / 2];
    let started = Instant::now();
    let mut count = 0usize;
    loop {
        if stdout.read_exact(&mut luma).is_err() {
            break;
        }
        stdout.read_exact(&mut chroma)?;
        let frame = encoder.pool().acquire();
        uploader.write(&frame, &luma, &chroma, width)?;
        encoder.submit(EncoderFrame::D3d12(frame), Rational::new(count as i64, fps))?;
        let packets = encoder.drain()?;
        sink.write(&encoder, packets)?;
        count += 1;
    }
    let packets = encoder.flush()?;
    sink.write(&encoder, packets)?;
    let seconds = started.elapsed().as_secs_f64();
    sink.into_muxer().finish()?;
    child.wait()?;
    let bytes = std::fs::metadata(&out)?.len();
    println!(
        "{options:?} frames {count} {seconds:.2}s ({:.1} fps in) {bytes} bytes {:.2} Mb/s",
        count as f64 / seconds,
        bytes as f64 * 8.0 / (count as f64 / fps as f64) / 1e6
    );
    Ok(())
}
