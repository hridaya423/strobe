#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use std::sync::{Mutex, MutexGuard};

use anyhow::Result;
use objc2_io_surface::IOSurfaceLockOptions;
use vrf_encode::videotoolbox::{VideoToolboxEncoder, capabilities, pool::PoolBuffer};
use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Rational, StreamInfo};

const WIDTH: usize = 640;
const HEIGHT: usize = 360;
const FRAMES: usize = 60;

static ENGINE: Mutex<()> = Mutex::new(());

fn engine() -> MutexGuard<'static, ()> {
    ENGINE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn path(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fill(buffer: &PoolBuffer, index: usize, ten_bit: bool) {
    let surface = buffer.surface();
    let luma = 16 + 3 * index;
    unsafe { surface.lock(IOSurfaceLockOptions::empty(), null_mut()) };
    for plane in 0..2 {
        let rows = surface.height_of_plane(plane);
        let stride = surface.bytes_per_row_of_plane(plane);
        let base = surface.base_address_of_plane(plane).as_ptr().cast::<u8>();
        let row = unsafe { std::slice::from_raw_parts_mut(base, stride * rows) };
        let value = if plane == 0 { luma } else { 128 };
        for line in row.chunks_mut(stride) {
            if ten_bit {
                let sample = ((value as u16) << 2) << 6;
                for pair in line.as_chunks_mut::<2>().0 {
                    *pair = sample.to_le_bytes();
                }
            } else {
                line.fill(value as u8);
            }
        }
    }
    unsafe { surface.unlock(IOSurfaceLockOptions::empty(), null_mut()) };
}

fn encode(config: EncoderConfig, frames: usize) -> Result<(Vec<Packet>, StreamInfo)> {
    let mut encoder = VideoToolboxEncoder::new(config.clone())?;
    let mut packets = Vec::new();
    for index in 0..frames {
        let buffer = encoder.pool().acquire()?;
        fill(&buffer, index, config.ten_bit);
        let pts = Rational::new(index as i64 * config.frame_rate.den, config.frame_rate.num);
        encoder.submit(EncoderFrame::PixelBuffer(buffer), pts)?;
        packets.extend(encoder.drain()?);
    }
    packets.extend(encoder.flush()?);
    let info = encoder.finish()?;
    Ok((packets, info))
}

fn config(codec: Codec, frame_rate: Rational) -> EncoderConfig {
    let mut config = EncoderConfig::new(codec, WIDTH as u32, HEIGHT as u32, frame_rate);
    config.keyframe_interval = 30;
    config
}

fn same(a: Rational, b: Rational) -> bool {
    a.num * b.den == b.num * a.den
}

fn annex_b(packets: &[Packet], info: &StreamInfo) -> Vec<u8> {
    let (length_size, parameter_sets) = match info.codec {
        Codec::H264 => parse_avcc(&info.config),
        Codec::Hevc => parse_hvcc(&info.config),
        Codec::ProRes422 => unreachable!(),
    };
    let mut out = Vec::new();
    for packet in packets {
        if packet.keyframe {
            for set in &parameter_sets {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(set);
            }
        }
        let mut rest = packet.data.as_slice();
        while !rest.is_empty() {
            let (prefix, tail) = rest.split_at(length_size);
            let size = prefix.iter().fold(0usize, |n, b| n << 8 | *b as usize);
            let (nal, tail) = tail.split_at(size);
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
            rest = tail;
        }
    }
    out
}

fn parse_avcc(config: &[u8]) -> (usize, Vec<Vec<u8>>) {
    assert_eq!(config[0], 1);
    let length_size = (config[4] & 3) as usize + 1;
    let mut sets = Vec::new();
    let mut at = 5;
    let sps = (config[at] & 31) as usize;
    at += 1;
    for _ in 0..sps {
        let size = u16::from_be_bytes([config[at], config[at + 1]]) as usize;
        sets.push(config[at + 2..at + 2 + size].to_vec());
        at += 2 + size;
    }
    let pps = config[at] as usize;
    at += 1;
    for _ in 0..pps {
        let size = u16::from_be_bytes([config[at], config[at + 1]]) as usize;
        sets.push(config[at + 2..at + 2 + size].to_vec());
        at += 2 + size;
    }
    (length_size, sets)
}

fn parse_hvcc(config: &[u8]) -> (usize, Vec<Vec<u8>>) {
    assert_eq!(config[0], 1);
    let length_size = (config[21] & 3) as usize + 1;
    let mut sets = Vec::new();
    let mut at = 23;
    for _ in 0..config[22] {
        let count = u16::from_be_bytes([config[at + 1], config[at + 2]]) as usize;
        at += 3;
        for _ in 0..count {
            let size = u16::from_be_bytes([config[at], config[at + 1]]) as usize;
            sets.push(config[at + 2..at + 2 + size].to_vec());
            at += 2 + size;
        }
    }
    (length_size, sets)
}

fn run(program: &str, args: &[&str]) -> Vec<u8> {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn probe(file: &Path, entries: &str) -> String {
    let out = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            entries,
            "-of",
            "csv=p=0",
            file.to_str().unwrap(),
        ],
    );
    String::from_utf8(out).unwrap().trim().to_string()
}

fn decoded_indexes(file: &Path, ten_bit: bool) -> Vec<i64> {
    let pix_fmt = if ten_bit { "gray10le" } else { "gray" };
    let raw = run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            file.to_str().unwrap(),
            "-vf",
            "extractplanes=y",
            "-pix_fmt",
            pix_fmt,
            "-f",
            "rawvideo",
            "-",
        ],
    );
    let (bytes, black, step) = if ten_bit {
        (2, 64.0, 12.0)
    } else {
        (1, 16.0, 3.0)
    };
    raw.chunks_exact(WIDTH * HEIGHT * bytes)
        .map(|frame| {
            let sum: u64 = if ten_bit {
                frame
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|s| u16::from_le_bytes(*s) as u64)
                    .sum()
            } else {
                frame.iter().map(|b| *b as u64).sum()
            };
            ((sum as f64 / (WIDTH * HEIGHT) as f64 - black) / step).round() as i64
        })
        .collect()
}

fn check_packets(packets: &[Packet], info: &StreamInfo, frame_rate: Rational) {
    assert_eq!(packets.len(), FRAMES);
    assert_eq!((info.width, info.height), (WIDTH as u32, HEIGHT as u32));
    assert_eq!(info.timebase, Rational::new(1, frame_rate.num));
    for (index, packet) in packets.iter().enumerate() {
        let pts = Rational::new(index as i64 * frame_rate.den, frame_rate.num);
        assert!(same(packet.pts, pts), "frame {index}: pts {:?}", packet.pts);
        assert!(!packet.data.is_empty());
    }
    let keyframes: Vec<usize> = (0..FRAMES).filter(|i| packets[*i].keyframe).collect();
    assert_eq!(keyframes, [0, 30]);
}

fn check_elementary_stream(codec: Codec, extension: &str, name: &str, ten_bit: bool) {
    let _engine = engine();
    let frame_rate = Rational::new(30000, 1001);
    let mut config = config(codec, frame_rate);
    config.ten_bit = ten_bit;
    let (packets, info) = encode(config, FRAMES).unwrap();
    check_packets(&packets, &info, frame_rate);
    assert_eq!(info.codec, codec);
    assert_eq!(info.config[0], 1);

    let scratch = Scratch::new(name);
    let file = scratch.path(&format!("out.{extension}"));
    fs::write(&file, annex_b(&packets, &info)).unwrap();
    assert_eq!(probe(&file, "stream=nb_read_frames"), "60");
    if ten_bit {
        assert_eq!(probe(&file, "stream=pix_fmt"), "yuv420p10le");
    }
    let indexes = decoded_indexes(&file, ten_bit);
    assert_eq!(indexes, (0..FRAMES as i64).collect::<Vec<_>>());
    assert_eq!(indexes[37], 37);
}

#[test]
fn h264_encodes_60_frames_and_frame_37_decodes_as_37() {
    check_elementary_stream(Codec::H264, "h264", "h264", false);
}

#[test]
fn hevc_encodes_60_frames_and_frame_37_decodes_as_37() {
    check_elementary_stream(Codec::Hevc, "hevc", "hevc", false);
}

#[test]
fn hevc_main10_encodes_p010_frames() {
    check_elementary_stream(Codec::Hevc, "hevc", "hevc10", true);
}

#[test]
fn prores_encodes_60_frames_and_frame_37_decodes_as_37() {
    let _engine = engine();
    let frame_rate = Rational::new(30, 1);
    let mut config = config(Codec::ProRes422, frame_rate);
    config.keyframe_interval = 1;
    let (packets, info) = encode(config, FRAMES).unwrap();
    assert_eq!(packets.len(), FRAMES);
    assert_eq!(info.codec, Codec::ProRes422);
    assert!(info.config.is_empty());
    assert!(packets.iter().all(|p| p.keyframe));

    let scratch = Scratch::new("prores");
    for (index, packet) in packets.iter().enumerate() {
        fs::write(
            scratch.path(&format!("frame{index:03}.prores")),
            &packet.data,
        )
        .unwrap();
    }
    let file = scratch.path("out.mov");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-f",
            "image2",
            "-c:v",
            "prores",
            "-framerate",
            "30",
            "-i",
            scratch.path("frame%03d.prores").to_str().unwrap(),
            "-c",
            "copy",
            file.to_str().unwrap(),
        ],
    );
    assert_eq!(probe(&file, "stream=nb_read_frames"), "60");
    let indexes = decoded_indexes(&file, true);
    assert_eq!(indexes, (0..FRAMES as i64).collect::<Vec<_>>());
    assert_eq!(indexes[37], 37);
}

#[test]
fn capabilities_are_read_from_the_session() {
    let _engine = engine();
    for codec in [Codec::H264, Codec::Hevc] {
        let caps = capabilities(codec).unwrap();
        println!("{codec:?}: {caps:?}");
        assert!(caps.parallelization_limit >= 1);
        assert!(caps.min_subdivision_frames >= 1);
    }
}

#[test]
fn sessions_never_exceed_the_recommended_limit() {
    let _engine = engine();
    let config = config(Codec::H264, Rational::new(30, 1));
    let limit = capabilities(Codec::H264).unwrap().parallelization_limit;
    let open: Vec<_> = (0..limit)
        .map(|_| VideoToolboxEncoder::new(config.clone()).unwrap())
        .collect();
    let error = VideoToolboxEncoder::new(config.clone()).err().unwrap();
    assert!(
        error.to_string().contains("parallelization limit"),
        "{error}"
    );
    drop(open);
    VideoToolboxEncoder::new(config).unwrap();
}

#[test]
fn ten_bit_is_rejected_outside_hevc() {
    let mut config = config(Codec::H264, Rational::new(30, 1));
    config.ten_bit = true;
    let error = VideoToolboxEncoder::new(config).err().unwrap();
    assert!(error.to_string().contains("ten_bit"), "{error}");
}

#[test]
fn a_caller_that_drains_only_at_the_end_does_not_deadlock() {
    let _engine = engine();
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(60));
        eprintln!("deadlock: 300 undrained frames did not finish within 60 s");
        std::process::exit(1);
    });
    let config = config(Codec::H264, Rational::new(30, 1));
    let mut encoder = VideoToolboxEncoder::new(config).unwrap();
    for index in 0..300 {
        let buffer = encoder.pool().acquire().unwrap();
        fill(&buffer, index % 70, false);
        encoder
            .submit(
                EncoderFrame::PixelBuffer(buffer),
                Rational::new(index as i64, 30),
            )
            .unwrap();
    }
    let packets = encoder.flush().unwrap();
    assert_eq!(packets.len(), 300);
    encoder.finish().unwrap();
}
