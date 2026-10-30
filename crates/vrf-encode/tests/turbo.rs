#![cfg(target_os = "macos")]

use std::ptr::null_mut;
use std::sync::{Mutex, MutexGuard};
use std::thread::sleep;
use std::time::Duration;

use anyhow::Result;
use objc2_io_surface::IOSurfaceLockOptions;
use vrf_encode::turbo::{LaneKind, TurboEncoder, TurboOptions};
use vrf_encode::videotoolbox::VideoToolboxEncoder;
use vrf_encode::videotoolbox::pool::{PixelFormat, PixelPool, PoolBuffer};
use vrf_encode::x264::X264Encoder;
use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Rational};

const WIDTH: usize = 640;
const HEIGHT: usize = 360;
const RATE: Rational = Rational::new(30, 1);

static ENGINE: Mutex<()> = Mutex::new(());

fn engine() -> MutexGuard<'static, ()> {
    ENGINE.lock().unwrap_or_else(|e| e.into_inner())
}

fn hash(v: u32) -> u32 {
    let mut x = v;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^ (x >> 16)
}

fn fill(buffer: &PoolBuffer, index: usize) {
    let surface = buffer.surface();
    unsafe { surface.lock(IOSurfaceLockOptions::empty(), null_mut()) };
    for plane in 0..2 {
        let rows = surface.height_of_plane(plane);
        let stride = surface.bytes_per_row_of_plane(plane);
        let base = surface.base_address_of_plane(plane).as_ptr().cast::<u8>();
        let bytes = unsafe { std::slice::from_raw_parts_mut(base, stride * rows) };
        for (y, line) in bytes.chunks_mut(stride).enumerate() {
            for (x, byte) in line.iter_mut().enumerate() {
                let noise = (hash((x + y * stride + index * 7919) as u32) & 7) as usize;
                *byte = if plane == 0 {
                    let inside = x.abs_diff(40 + index * 5 % 500) < 60 && y.abs_diff(180) < 50;
                    let ramp = 40 + (x * 150 / WIDTH + y * 40 / HEIGHT + index * 2) % 150;
                    (if inside { 220 } else { ramp } + noise) as u8
                } else {
                    (110 + (x / 2 + index) % 40 + noise) as u8
                };
            }
        }
    }
    unsafe { surface.unlock(IOSurfaceLockOptions::empty(), null_mut()) };
}

fn config(codec: Codec) -> EncoderConfig {
    EncoderConfig::new(codec, WIDTH as u32, HEIGHT as u32, RATE)
}

fn time(index: usize) -> Rational {
    Rational::new(index as i64, 30)
}

fn nal_types(packet: &Packet) -> Vec<u8> {
    let mut rest = packet.data.as_slice();
    let mut types = Vec::new();
    while !rest.is_empty() {
        let size = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        types.push(rest[4] & 0x1f);
        rest = &rest[4 + size..];
    }
    types
}

fn feed(
    encoder: &mut impl Encoder,
    pool: &PixelPool,
    frames: usize,
    pause: Duration,
) -> Result<Vec<Packet>> {
    let mut packets = Vec::new();
    for index in 0..frames {
        let buffer = pool.acquire()?;
        fill(&buffer, index);
        encoder.submit(EncoderFrame::PixelBuffer(buffer), time(index))?;
        packets.extend(encoder.drain()?);
        sleep(pause);
    }
    packets.extend(encoder.flush()?);
    Ok(packets)
}

fn turbo_options(chunk_frames: u32) -> TurboOptions {
    TurboOptions {
        chunk_frames,
        x264_threads: 4,
        vt_headroom: 0,
        ..TurboOptions::default()
    }
}

fn assert_in_order(packets: &[Packet], frames: usize) {
    assert_eq!(packets.len(), frames);
    for (index, packet) in packets.iter().enumerate() {
        assert_eq!(packet.pts, time(index), "packet {index} pts");
        assert_eq!(packet.dts, packet.pts, "packet {index} dts");
    }
}

#[test]
fn the_x264_lane_encodes_in_order_and_obeys_a_forced_keyframe() {
    let _engine = engine();
    let mut encoder = X264Encoder::new(config(Codec::H264), 4).unwrap();
    let pool = PixelPool::with_size(
        WIDTH,
        HEIGHT,
        vrf_encode::videotoolbox::pool::PixelFormat::Nv12,
        8,
    )
    .unwrap();
    let mut packets = Vec::new();
    for index in 0..45 {
        let buffer = pool.acquire().unwrap();
        fill(&buffer, index);
        encoder
            .submit_keyframe(EncoderFrame::PixelBuffer(buffer), time(index), index == 20)
            .unwrap();
        packets.extend(encoder.drain().unwrap());
    }
    packets.extend(encoder.flush().unwrap());
    assert_in_order(&packets, 45);
    let keyframes: Vec<_> = (0..45).filter(|&i| packets[i].keyframe).collect();
    assert_eq!(keyframes, [0, 20]);
    assert!(nal_types(&packets[20]).contains(&5));
    assert!(!nal_types(&packets[21]).contains(&5));
    let info = encoder.finish().unwrap();
    assert_eq!(info.codec, Codec::H264);
    assert_eq!(info.config[0], 1);
}

#[test]
fn the_x264_lane_rejects_other_codecs_odd_sizes_and_ten_bit() {
    assert!(X264Encoder::new(config(Codec::Hevc), 0).is_err());
    let mut odd = config(Codec::H264);
    odd.width = 641;
    assert!(X264Encoder::new(odd, 0).is_err());
    let mut ten = config(Codec::H264);
    ten.ten_bit = true;
    assert!(X264Encoder::new(ten, 0).is_err());
}

#[test]
fn turbo_h264_returns_every_frame_in_order_with_an_idr_at_each_chunk_start() {
    let _engine = engine();
    let mut encoder = TurboEncoder::with_options(config(Codec::H264), turbo_options(30)).unwrap();
    let pool = encoder.pool().clone();
    let packets = feed(&mut encoder, &pool, 300, Duration::ZERO).unwrap();
    assert_in_order(&packets, 300);
    for chunk in 0..10 {
        let first = &packets[chunk * 30];
        assert!(first.keyframe, "chunk {chunk} does not start on a keyframe");
        assert!(nal_types(first).contains(&5), "chunk {chunk} has no IDR");
    }
    let stats = encoder.lane_stats();
    assert_eq!(stats.iter().map(|l| l.chunks).sum::<u64>(), 10);
    assert_eq!(stats.iter().map(|l| l.frames).sum::<u64>(), 300);
    assert!(
        stats.iter().all(|l| l.chunks > 0),
        "both lanes should have encoded a chunk: {stats:?}"
    );
    let info = encoder.finish().unwrap();
    let record = parse_avcc(&info.config);
    assert_eq!(record.0, 2, "one SPS per lane");
    assert_eq!(record.1, 2, "one PPS per lane");
}

fn parse_avcc(config: &[u8]) -> (usize, usize) {
    assert_eq!(config[0], 1);
    let mut at = 6;
    for _ in 0..(config[5] & 31) {
        at += 2 + u16::from_be_bytes([config[at], config[at + 1]]) as usize;
    }
    let pps = config[at] as usize;
    ((config[5] & 31) as usize, pps)
}

#[test]
fn a_renderer_slower_than_the_media_engine_never_wakes_the_cpu_lane() {
    let _engine = engine();
    let options = TurboOptions {
        chunk_frames: 30,
        x264_threads: 4,
        ..TurboOptions::default()
    };
    let mut encoder = TurboEncoder::with_options(config(Codec::H264), options).unwrap();
    let pool = encoder.pool().clone();
    let packets = feed(&mut encoder, &pool, 150, Duration::from_millis(3)).unwrap();
    assert_in_order(&packets, 150);
    let stats = encoder.lane_stats();
    let cpu = stats.iter().find(|l| l.kind == LaneKind::X264).unwrap();
    assert_eq!(cpu.chunks, 0, "{stats:?}");
}

#[test]
fn turbo_hevc_uses_videotoolbox_chunks_only() {
    let _engine = engine();
    let mut encoder = TurboEncoder::with_options(config(Codec::Hevc), turbo_options(30)).unwrap();
    assert!(
        encoder
            .lane_stats()
            .iter()
            .all(|l| l.kind == LaneKind::VideoToolbox)
    );
    let pool = encoder.pool().clone();
    let packets = feed(&mut encoder, &pool, 120, Duration::ZERO).unwrap();
    assert_in_order(&packets, 120);
    for chunk in 0..4 {
        assert!(packets[chunk * 30].keyframe, "chunk {chunk}");
    }
    let info = encoder.finish().unwrap();
    assert_eq!(info.codec, Codec::Hevc);
    assert!(!info.config.is_empty());
}

#[test]
fn frames_must_arrive_in_order_on_whole_frame_times() {
    let _engine = engine();
    let mut encoder = TurboEncoder::with_options(config(Codec::H264), turbo_options(30)).unwrap();
    let pool = encoder.pool().clone();
    let buffer = pool.acquire().unwrap();
    fill(&buffer, 0);
    encoder
        .submit(EncoderFrame::PixelBuffer(buffer.clone()), time(0))
        .unwrap();
    let skipped = encoder.submit(EncoderFrame::PixelBuffer(buffer.clone()), time(2));
    assert!(skipped.unwrap_err().to_string().contains("out of order"));
    let between = encoder.submit(EncoderFrame::PixelBuffer(buffer), Rational::new(1, 60));
    assert!(between.unwrap_err().to_string().contains("whole frame"));
}

#[test]
fn turbo_rejects_codecs_with_their_own_engine() {
    assert!(TurboEncoder::new(config(Codec::ProRes422)).is_err());
}

fn halftone(x: usize, y: usize, index: usize) -> u8 {
    let dots = ((x + index * 3) as f64 * 0.9).sin() * (y as f64 * 0.9).sin();
    let noise = (hash((x + y * WIDTH + index * 7919) as u32) & 7) as f64 - 4.0;
    (128.0 + 28.0 * dots + noise).round().clamp(0.0, 255.0) as u8
}

fn fill_halftone(buffer: &PoolBuffer, index: usize) {
    let surface = buffer.surface();
    unsafe { surface.lock(IOSurfaceLockOptions::empty(), null_mut()) };
    for plane in 0..2 {
        let rows = surface.height_of_plane(plane);
        let stride = surface.bytes_per_row_of_plane(plane);
        let base = surface.base_address_of_plane(plane).as_ptr().cast::<u8>();
        let bytes = unsafe { std::slice::from_raw_parts_mut(base, stride * rows) };
        for (y, line) in bytes.chunks_mut(stride).enumerate() {
            for (x, byte) in line.iter_mut().enumerate() {
                *byte = if plane == 0 { halftone(x, y, index) } else { 128 };
            }
        }
    }
    unsafe { surface.unlock(IOSurfaceLockOptions::empty(), null_mut()) };
}

fn annex_b(packets: &[Packet], avcc: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 6;
    for _ in 0..(avcc[5] & 31) {
        let size = u16::from_be_bytes([avcc[at], avcc[at + 1]]) as usize;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&avcc[at + 2..at + 2 + size]);
        at += 2 + size;
    }
    let pps = avcc[at] as usize;
    at += 1;
    for _ in 0..pps {
        let size = u16::from_be_bytes([avcc[at], avcc[at + 1]]) as usize;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&avcc[at + 2..at + 2 + size]);
        at += 2 + size;
    }
    for packet in packets {
        let mut rest = packet.data.as_slice();
        while !rest.is_empty() {
            let size = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&rest[4..4 + size]);
            rest = &rest[4 + size..];
        }
    }
    out
}

fn over_eight(encoder: &mut impl Encoder, pool: &PixelPool, frames: usize, name: &str) -> Result<f64> {
    let packets = feed_with(encoder, pool, frames, fill_halftone)?;
    assert_in_order(&packets, frames);
    let info = encoder.stream_info()?;
    let stream = std::env::temp_dir().join(format!("vrf-halftone-{name}-{}.h264", std::process::id()));
    std::fs::write(&stream, annex_b(&packets, &info.config))?;
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&stream)
        .args(["-pix_fmt", "gray", "-f", "rawvideo", "-"])
        .output()?;
    std::fs::remove_file(&stream)?;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout.len(), frames * WIDTH * HEIGHT);
    let mut over = 0usize;
    for (index, frame) in output.stdout.chunks(WIDTH * HEIGHT).enumerate() {
        for (at, luma) in frame.iter().enumerate() {
            let source = halftone(at % WIDTH, at / WIDTH, index);
            if luma.abs_diff(source) > 8 {
                over += 1;
            }
        }
    }
    Ok(over as f64 / (frames * WIDTH * HEIGHT) as f64)
}

fn feed_with(
    encoder: &mut impl Encoder,
    pool: &PixelPool,
    frames: usize,
    paint: fn(&PoolBuffer, usize),
) -> Result<Vec<Packet>> {
    let mut packets = Vec::new();
    for index in 0..frames {
        let buffer = pool.acquire()?;
        paint(&buffer, index);
        encoder.submit(EncoderFrame::PixelBuffer(buffer), time(index))?;
        packets.extend(encoder.drain()?);
    }
    packets.extend(encoder.flush()?);
    Ok(packets)
}

#[test]
fn the_cpu_lane_holds_fine_detail_as_well_as_videotoolbox() {
    let _engine = engine();
    let frames = 60;
    let mut vt = VideoToolboxEncoder::new(config(Codec::H264)).unwrap();
    let pool = vt.pool().clone();
    let vt_over = over_eight(&mut vt, &pool, frames, "vt").unwrap();
    let mut x264 = X264Encoder::new(config(Codec::H264), 4).unwrap();
    let pool = PixelPool::with_size(WIDTH, HEIGHT, PixelFormat::Nv12, 8).unwrap();
    let x264_over = over_eight(&mut x264, &pool, frames, "x264").unwrap();
    eprintln!("over 8: VideoToolbox {vt_over:.5}, x264 {x264_over:.5}");
    assert!(
        x264_over <= vt_over * 1.25,
        "the x264 lane has {x264_over} of its luma over 8 against VideoToolbox's {vt_over}"
    );
}
