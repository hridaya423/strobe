use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;

use anyhow::Result;
use objc2_io_surface::IOSurfaceLockOptions;
use vrf::mux::{Container, Muxer};
use vrf::sink::Sink;
use vrf_encode::turbo::{TurboEncoder, TurboOptions};
use vrf_encode::videotoolbox::pool::PoolBuffer;
use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Rational};

pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 360;
pub const RATE: Rational = Rational::new(30, 1);

pub struct Dir(PathBuf);

impl Dir {
    pub fn new(name: &str) -> Dir {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("turbo-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Dir(path)
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn run(program: &str, args: &[&str]) -> String {
    let out = Command::new(program).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

pub fn probe(file: &Path, args: &[&str]) -> Vec<HashMap<String, String>> {
    let mut full = vec!["-v", "error", "-of", "compact=p=0"];
    full.extend_from_slice(args);
    full.push(file.to_str().unwrap());
    run("ffprobe", &full)
        .lines()
        .map(|line| {
            line.split('|')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .collect()
}

pub fn hash(v: u32) -> u32 {
    let mut x = v;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^ (x >> 16)
}

pub fn nv12_frame(index: usize, width: usize, height: usize) -> (Vec<u8>, Vec<u8>) {
    let mut luma = vec![0u8; width * height];
    for (y, line) in luma.chunks_mut(width).enumerate() {
        for (x, byte) in line.iter_mut().enumerate() {
            let noise = (hash((x + y * width + index * 7919) as u32) & 7) as usize;
            let inside =
                x.abs_diff(40 + index * 5 % (width - 100)) < 60 && y.abs_diff(height / 2) < 50;
            let ramp = 40 + (x * 150 / width + y * 40 / height + index * 2) % 150;
            *byte = (if inside { 220 } else { ramp } + noise) as u8;
        }
    }
    let mut chroma = vec![0u8; width * height / 2];
    for (y, line) in chroma.chunks_mut(width).enumerate() {
        for (x, byte) in line.iter_mut().enumerate() {
            let noise = (hash((x + y * width + index * 104729) as u32) & 7) as usize;
            *byte = (110 + (x / 2 + index) % 40 + noise) as u8;
        }
    }
    (luma, chroma)
}

pub struct Content {
    pub width: usize,
    pub height: usize,
    frames: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Content {
    pub fn new(width: usize, height: usize, cycle: usize) -> Content {
        Content {
            width,
            height,
            frames: (0..cycle).map(|i| nv12_frame(i, width, height)).collect(),
        }
    }

    pub fn frame(&self, index: usize) -> &(Vec<u8>, Vec<u8>) {
        &self.frames[index % self.frames.len()]
    }

    pub fn fill(&self, buffer: &PoolBuffer, index: usize) {
        let (luma, chroma) = self.frame(index);
        let surface = buffer.surface();
        unsafe { surface.lock(IOSurfaceLockOptions::empty(), null_mut()) };
        for (plane, source) in [(0, luma), (1, chroma)] {
            let rows = surface.height_of_plane(plane);
            let stride = surface.bytes_per_row_of_plane(plane);
            let base = surface.base_address_of_plane(plane).as_ptr().cast::<u8>();
            let bytes = unsafe { std::slice::from_raw_parts_mut(base, stride * rows) };
            for (line, from) in bytes.chunks_mut(stride).zip(source.chunks(self.width)) {
                line[..self.width].copy_from_slice(from);
            }
        }
        unsafe { surface.unlock(IOSurfaceLockOptions::empty(), null_mut()) };
    }
}

pub fn options(chunk_frames: u32) -> TurboOptions {
    TurboOptions {
        chunk_frames,
        x264_threads: 4,
        vt_headroom: 0,
        ..TurboOptions::default()
    }
}

pub fn encode_turbo(
    out: &Path,
    frames: usize,
    options: TurboOptions,
    audio: Option<vrf::mux::AudioTrack>,
) -> Result<Vec<vrf_encode::turbo::LaneStats>> {
    let config = EncoderConfig::new(Codec::H264, WIDTH as u32, HEIGHT as u32, RATE);
    let content = Content::new(WIDTH, HEIGHT, 120);
    let mut encoder = TurboEncoder::with_options(config, options)?;
    let pool = encoder.pool().clone();
    let mut sink = Sink::new(
        Muxer::create(out, Container::Mp4, RATE.num as u32)?,
        RATE.den as u32,
    );
    for index in 0..frames {
        let buffer = pool.acquire()?;
        content.fill(&buffer, index);
        let pts = Rational::new(index as i64 * RATE.den, RATE.num);
        encoder.submit(EncoderFrame::PixelBuffer(buffer), pts)?;
        let packets = encoder.drain()?;
        sink.write(&encoder, packets)?;
    }
    let packets = encoder.flush()?;
    sink.write(&encoder, packets)?;
    let stats = encoder.lane_stats();
    let mut muxer = sink.into_muxer();
    if let Some(audio) = audio {
        muxer.set_audio(audio)?;
    }
    muxer.finish()?;
    Ok(stats)
}
