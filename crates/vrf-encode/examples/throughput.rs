#[cfg(target_os = "macos")]
mod bench {
    use std::ptr::null_mut;
    use std::time::Instant;

    use anyhow::{Result, bail};
    use objc2_io_surface::IOSurfaceLockOptions;
    use vrf_encode::videotoolbox::VideoToolboxEncoder;
    use vrf_encode::videotoolbox::pool::POOL_SIZE;
    use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Rational};

    fn cpu_seconds() -> f64 {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        seconds(usage.ru_utime) + seconds(usage.ru_stime)
    }

    fn texture() -> Vec<u8> {
        let mut state = 0x9E3779B97F4A7C15u64;
        (0..1usize << 17)
            .map(|k| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let noise = ((state >> 40) & 15) as f64 - 7.5;
                let k = k as f64;
                (128.0 + 55.0 * (k / 211.0).sin() + 35.0 * (k / 37.0).sin() + noise * 0.8)
                    .clamp(20.0, 230.0) as u8
            })
            .collect()
    }

    pub fn run() -> Result<()> {
        let mut codec = Codec::H264;
        let mut frames = 600usize;
        let (mut width, mut height) = (1920u32, 1080u32);
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let value = args.next();
            match (arg.as_str(), value.as_deref()) {
                ("--codec", Some("h264")) => codec = Codec::H264,
                ("--codec", Some("hevc")) => codec = Codec::Hevc,
                ("--codec", Some("prores")) => codec = Codec::ProRes422,
                ("--frames", Some(n)) => frames = n.parse()?,
                ("--width", Some(n)) => width = n.parse()?,
                ("--height", Some(n)) => height = n.parse()?,
                _ => bail!(
                    "usage: throughput [--codec h264|hevc|prores] [--frames N] [--width W] [--height H]"
                ),
            }
        }

        let mut config = EncoderConfig::new(codec, width, height, Rational::new(30, 1));
        config.keyframe_interval = 120;
        let mut encoder = VideoToolboxEncoder::new(config)?;
        println!("capabilities: {:?}", encoder.capabilities());

        let table = texture();
        let warm = (0..POOL_SIZE)
            .map(|_| encoder.pool().acquire())
            .collect::<Result<Vec<_>>>()?;
        for (n, buffer) in warm.iter().enumerate() {
            let surface = buffer.surface();
            unsafe { surface.lock(IOSurfaceLockOptions::empty(), null_mut()) };
            for plane in 0..2 {
                let stride = surface.bytes_per_row_of_plane(plane);
                let rows = surface.height_of_plane(plane);
                let base = surface.base_address_of_plane(plane).as_ptr().cast::<u8>();
                for y in 0..rows {
                    let row =
                        unsafe { std::slice::from_raw_parts_mut(base.add(y * stride), stride) };
                    let at = (y * 7 + 3 * n + plane * 4001) & 0xFFFF;
                    for (x, byte) in row.iter_mut().enumerate() {
                        *byte = table[(at + x) & 0xFFFF];
                    }
                }
            }
            unsafe { surface.unlock(IOSurfaceLockOptions::empty(), null_mut()) };
        }
        drop(warm);

        let (mut out_frames, mut out_bytes) = (0usize, 0usize);
        let cpu = cpu_seconds();
        let start = Instant::now();
        for index in 0..frames {
            let buffer = encoder.pool().acquire()?;
            encoder.submit(
                EncoderFrame::PixelBuffer(buffer),
                Rational::new(index as i64, 30),
            )?;
            for packet in encoder.drain()? {
                out_frames += 1;
                out_bytes += packet.data.len();
            }
        }
        for packet in encoder.flush()? {
            out_frames += 1;
            out_bytes += packet.data.len();
        }
        let seconds = start.elapsed().as_secs_f64();
        let cpu = cpu_seconds() - cpu;
        println!(
            "RESULT {codec:?} {width}x{height} in_flight={POOL_SIZE} frames_out={out_frames} \
             time={seconds:.3}s fps={:.1} bitrate={:.1}Mb/s cpu={:.2}ms/frame",
            out_frames as f64 / seconds,
            out_bytes as f64 * 8.0 / (out_frames as f64 / 30.0) / 1e6,
            cpu / frames as f64 * 1000.0,
        );
        encoder.finish()?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    bench::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {}
