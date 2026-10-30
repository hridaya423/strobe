#[cfg(target_os = "macos")]
mod mac {
    use std::io::Read;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::ptr::null_mut;
    use std::time::Instant;

    use anyhow::{Context, Result, bail};
    use objc2_io_surface::IOSurfaceLockOptions;
    use vrf::mux::{Container, Muxer};
    use vrf::sink::Sink;
    use vrf_encode::turbo::TurboEncoder;
    use vrf_encode::videotoolbox::VideoToolboxEncoder;
    use vrf_encode::videotoolbox::pool::{PixelFormat, PixelPool, PoolBuffer};
    use vrf_encode::x264::X264Encoder;
    use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Quality, Rational};

    fn fill(buffer: &PoolBuffer, width: usize, luma: &[u8], chroma: &[u8]) {
        let surface = buffer.surface();
        unsafe { surface.lock(IOSurfaceLockOptions::empty(), null_mut()) };
        for (plane, source) in [(0, luma), (1, chroma)] {
            let rows = surface.height_of_plane(plane);
            let stride = surface.bytes_per_row_of_plane(plane);
            let base = surface.base_address_of_plane(plane).as_ptr().cast::<u8>();
            let bytes = unsafe { std::slice::from_raw_parts_mut(base, stride * rows) };
            for (line, from) in bytes.chunks_mut(stride).zip(source.chunks(width)) {
                line[..width].copy_from_slice(from);
            }
        }
        unsafe { surface.unlock(IOSurfaceLockOptions::empty(), null_mut()) };
    }

    fn pump(
        encoder: &mut impl Encoder,
        pool: &PixelPool,
        input: &PathBuf,
        width: usize,
        height: usize,
        fps: i64,
        frames: Option<usize>,
        out: &PathBuf,
    ) -> Result<(usize, f64)> {
        let mut command = Command::new("ffmpeg");
        command.args(["-v", "error"]);
        if input.to_string_lossy().contains('%') {
            command.args(["-framerate", &fps.to_string()]);
        }
        command.arg("-i").arg(input);
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
        let mut sink = Sink::new(Muxer::create(out, Container::Mp4, fps as u32)?, 1);
        let mut luma = vec![0u8; width * height];
        let mut chroma = vec![0u8; width * height / 2];
        let started = Instant::now();
        let mut count = 0usize;
        loop {
            if stdout.read_exact(&mut luma).is_err() {
                break;
            }
            stdout.read_exact(&mut chroma)?;
            let buffer = pool.acquire()?;
            fill(&buffer, width, &luma, &chroma);
            let pts = Rational::new(count as i64, fps);
            encoder.submit(EncoderFrame::PixelBuffer(buffer), pts)?;
            let packets = encoder.drain()?;
            sink.write(encoder, packets)?;
            count += 1;
        }
        let packets = encoder.flush()?;
        sink.write(encoder, packets)?;
        let seconds = started.elapsed().as_secs_f64();
        let muxer = sink.into_muxer();
        muxer.finish()?;
        child.wait()?;
        Ok((count, seconds))
    }

    pub fn main() -> Result<()> {
        let mut args = std::env::args().skip(1);
        let (mut input, mut out, mut lane) = (None, None, "vt".to_string());
        let (mut size, mut fps, mut frames, mut quality) =
            ((1920, 1080), 30, None, Quality::default());
        while let Some(flag) = args.next() {
            let mut value = || args.next().context("flag needs a value");
            match flag.as_str() {
                "--input" => input = Some(PathBuf::from(value()?)),
                "--out" => out = Some(PathBuf::from(value()?)),
                "--lane" => lane = value()?,
                "--fps" => fps = value()?.parse()?,
                "--frames" => frames = Some(value()?.parse()?),
                "--size" => {
                    let v = value()?;
                    let (w, h) = v.split_once('x').context("WxH")?;
                    size = (w.parse()?, h.parse()?);
                }
                "--quality" => {
                    quality = match value()?.as_str() {
                        "draft" => Quality::Draft,
                        "balanced" => Quality::Balanced,
                        "high" => Quality::High,
                        other => bail!("quality {other}"),
                    }
                }
                other => bail!("unknown flag {other}"),
            }
        }
        let (input, out) = (input.context("--input")?, out.context("--out")?);
        let (width, height) = size;
        let mut config = EncoderConfig::new(
            Codec::H264,
            width as u32,
            height as u32,
            Rational::new(fps, 1),
        );
        config.quality = quality;
        let (count, seconds) = match lane.as_str() {
            "vt" => {
                let mut encoder = VideoToolboxEncoder::new(config)?;
                let pool = encoder.pool().clone();
                pump(
                    &mut encoder,
                    &pool,
                    &input,
                    width,
                    height,
                    fps,
                    frames,
                    &out,
                )?
            }
            "x264" => {
                let mut encoder = X264Encoder::new(config, 0)?;
                let pool = PixelPool::with_size(width, height, PixelFormat::Nv12, 8)?;
                pump(
                    &mut encoder,
                    &pool,
                    &input,
                    width,
                    height,
                    fps,
                    frames,
                    &out,
                )?
            }
            "turbo" => {
                let mut encoder = TurboEncoder::new(config)?;
                let pool = encoder.pool().clone();
                let result = pump(
                    &mut encoder,
                    &pool,
                    &input,
                    width,
                    height,
                    fps,
                    frames,
                    &out,
                )?;
                eprintln!("lanes {:?}", encoder.lane_stats());
                result
            }
            other => bail!("lane {other}"),
        };
        let bytes = std::fs::metadata(&out)?.len();
        println!(
            "{lane} frames {count} {seconds:.2}s ({:.1} fps in) {bytes} bytes {:.2} Mb/s",
            count as f64 / seconds,
            bytes as f64 * 8.0 / (count as f64 / fps as f64) / 1e6
        );
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    mac::main()
}

#[cfg(not(target_os = "macos"))]
fn main() {}
