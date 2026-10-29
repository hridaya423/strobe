use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;

use anyhow::Result;
use objc2_io_surface::IOSurfaceLockOptions;
use vrf::mux::{Container, Muxer};
use vrf::sink::Sink;
use vrf_encode::turbo::{LaneStats, TurboEncoder, TurboOptions};
use vrf_encode::videotoolbox::VideoToolboxEncoder;
use vrf_encode::videotoolbox::pool::{PixelPool, PoolBuffer};
use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Quality, Rational};

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

const BLOCKS: usize = 9;

pub fn nv12_frame(index: usize, width: usize, height: usize) -> (Vec<u8>, Vec<u8>) {
    let mut luma = vec![0u8; width * height];
    let mut chroma = vec![128u8; width * height / 2];
    let phase = index as f32;
    for (y, line) in luma.chunks_mut(width).enumerate() {
        let row = (y + index * 3 / 2) % 8 == 0;
        for (x, byte) in line.iter_mut().enumerate() {
            let (fx, fy) = (x as f32, y as f32);
            let wave = 40.0 * (fx * 0.21).sin() * (fy * 0.17).cos()
                + 25.0 * ((fx + fy) * 0.45 + phase * 0.02).sin();
            let noise = (hash((x + y * width) as u32) & 15) as f32;
            let grid = if row { 50.0 } else { 0.0 };
            *byte = (110.0 + wave + noise + grid) as u8;
        }
    }
    for k in 0..BLOCKS {
        let (w, h) = (width / 6 + k * 9, height / 7 + k * 5);
        let x0 = (k * 131 + index * (k + 2) / 4) % (width - w);
        let y0 = (k * 89 + index * (k % 3 + 1) / 3) % (height - h);
        for y in y0..y0 + h {
            luma[y * width + x0..y * width + x0 + w].fill(40 + 24 * k as u8);
        }
        for y in y0 / 2..(y0 + h) / 2 {
            for pair in chroma[y * width + x0 / 2 * 2..y * width + (x0 + w) / 2 * 2].chunks_mut(2) {
                pair.copy_from_slice(&[90 + 18 * k as u8, 200 - 15 * k as u8]);
            }
        }
    }
    let (x0, x1, y0) = (width / 4, 3 * width / 4, height / 2);
    for y in y0..height - 8 {
        for x in x0..x1 {
            let cell = hash((x / 3 + ((y + index / 2) / 3) * width) as u32);
            luma[y * width + x] = if cell & 3 == 0 {
                235
            } else {
                20 + (cell >> 8 & 31) as u8
            };
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
    let content = Content::new(WIDTH, HEIGHT, 120);
    encode_content(out, &content, frames, options, audio, Quality::default())
}

fn pump(
    encoder: &mut impl Encoder,
    pool: &PixelPool,
    content: &Content,
    frames: usize,
    out: &Path,
    audio: Option<vrf::mux::AudioTrack>,
) -> Result<()> {
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
        sink.write(encoder, packets)?;
    }
    let packets = encoder.flush()?;
    sink.write(encoder, packets)?;
    let mut muxer = sink.into_muxer();
    if let Some(audio) = audio {
        muxer.set_audio(audio)?;
    }
    muxer.finish()
}

pub fn config_for(content: &Content, quality: Quality) -> EncoderConfig {
    let mut config = EncoderConfig::new(
        Codec::H264,
        content.width as u32,
        content.height as u32,
        RATE,
    );
    config.quality = quality;
    config
}

pub fn encode_content(
    out: &Path,
    content: &Content,
    frames: usize,
    options: TurboOptions,
    audio: Option<vrf::mux::AudioTrack>,
    quality: Quality,
) -> Result<Vec<LaneStats>> {
    let config = config_for(content, quality);
    let mut encoder = TurboEncoder::with_options(config, options)?;
    let pool = encoder.pool().clone();
    pump(&mut encoder, &pool, content, frames, out, audio)?;
    Ok(encoder.lane_stats())
}

pub fn encode_single(out: &Path, content: &Content, frames: usize, quality: Quality) -> Result<()> {
    let config = config_for(content, quality);
    let mut encoder = VideoToolboxEncoder::new(config)?;
    let pool = encoder.pool().clone();
    pump(&mut encoder, &pool, content, frames, out, None)
}

pub fn encode_x264(out: &Path, content: &Content, frames: usize, quality: Quality) -> Result<()> {
    let config = config_for(content, quality);
    let mut encoder = vrf_encode::x264::X264Encoder::new(config, 8)?;
    let pool = PixelPool::with_size(
        content.width,
        content.height,
        vrf_encode::videotoolbox::pool::PixelFormat::Nv12,
        8,
    )?;
    pump(&mut encoder, &pool, content, frames, out, None)
}
