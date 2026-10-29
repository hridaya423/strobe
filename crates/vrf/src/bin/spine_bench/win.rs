use std::mem::ManuallyDrop;
use std::sync::Barrier;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use vrf_encode::nvenc::{
    NvencEncoder, Options, POOL_SIZE, PixelFormat, PoolFrame, Preset, RateControl, Tuning,
    nvidia_device,
};
use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Rational};
use windows::Win32::Foundation::{FILETIME, HANDLE};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetProcessTimes, INFINITE, WaitForSingleObject,
};
use windows::core::Interface;

const FRAME_RATE: Rational = Rational::new(60, 1);
const DISTINCT: usize = 16;

struct Args {
    codecs: Vec<Codec>,
    sizes: Vec<(u32, u32)>,
    sessions: Vec<usize>,
    presets: Vec<Preset>,
    frames: usize,
    runs: usize,
    pool: usize,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        codecs: vec![Codec::H264, Codec::Hevc, Codec::Av1],
        sizes: vec![(1920, 1080), (3840, 2160)],
        sessions: vec![1, 2],
        presets: vec![Preset::P1, Preset::P4],
        frames: 600,
        runs: 5,
        pool: POOL_SIZE,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--codec" => {
                args.codecs = value()?
                    .split(',')
                    .map(|codec| match codec {
                        "h264" => Ok(Codec::H264),
                        "hevc" => Ok(Codec::Hevc),
                        "av1" => Ok(Codec::Av1),
                        other => bail!("unknown codec {other}"),
                    })
                    .collect::<Result<_>>()?
            }
            "--size" => {
                args.sizes = value()?
                    .split(',')
                    .map(|size| match size {
                        "1080p" => Ok((1920, 1080)),
                        "4k" => Ok((3840, 2160)),
                        other => bail!("unknown size {other}"),
                    })
                    .collect::<Result<_>>()?
            }
            "--sessions" => {
                args.sessions = value()?
                    .split(',')
                    .map(|n| Ok(n.parse()?))
                    .collect::<Result<_>>()?
            }
            "--preset" => {
                args.presets = value()?
                    .split(',')
                    .map(|preset| match preset {
                        "p1" => Ok(Preset::P1),
                        "p4" => Ok(Preset::P4),
                        "p7" => Ok(Preset::P7),
                        other => bail!("unknown preset {other}"),
                    })
                    .collect::<Result<_>>()?
            }
            "--frames" => args.frames = value()?.parse()?,
            "--runs" => args.runs = value()?.parse()?,
            "--pool" => args.pool = value()?.parse()?,
            other => bail!("unknown flag {other}"),
        }
    }
    Ok(args)
}

fn filetime_seconds(time: FILETIME) -> f64 {
    ((u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)) as f64 / 1e7
}

fn cpu_seconds() -> f64 {
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
        .unwrap()
    };
    filetime_seconds(kernel) + filetime_seconds(user)
}

fn hash(x: u32, y: u32, k: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B) ^ k.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^ (h >> 12)
}

fn picture(k: u32, width: usize, height: usize) -> (Vec<u8>, Vec<u8>) {
    let mut luma = vec![0u8; width * height];
    for y in 0..height {
        for x in 0..width {
            let base = (x as u32 * 3 / 8 + y as u32 / 3 + k * 9) & 0xFF;
            let noise = hash(x as u32, y as u32, k) & 15;
            let block = (x as i32 - (k as i32 * 60) % (width as i32 - 220)).unsigned_abs() < 100
                && (y as i32 - height as i32 * 2 / 5).unsigned_abs() < 100;
            luma[y * width + x] = if block {
                235
            } else {
                (16 + (base * 200 / 255) + noise).min(235) as u8
            };
        }
    }
    let mut chroma = vec![0u8; width * height / 2];
    for y in 0..height / 2 {
        for x in 0..width / 2 {
            chroma[y * width + x * 2] = (64 + ((x as u32 + k * 5) & 127)) as u8;
            chroma[y * width + x * 2 + 1] = (64 + ((y as u32 * 2 + k * 3) & 127)) as u8;
        }
    }
    (luma, chroma)
}

struct Uploader {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    fence: ID3D12Fence,
    event: HANDLE,
    buffer: ID3D12Resource,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    value: u64,
}

impl Uploader {
    fn new(device: &ID3D12Device, width: u32, height: u32) -> Result<Uploader> {
        let probe = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: u64::from(width),
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: PixelFormat::Nv12.dxgi(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        let mut total = 0u64;
        unsafe {
            device.GetCopyableFootprints(&probe, 0, 2, 0, None, None, None, Some(&mut total))
        };
        let mut buffer: Option<ID3D12Resource> = None;
        unsafe {
            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_UPLOAD,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Width: total,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_UNKNOWN,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut buffer,
            )?;
        }
        let allocator: ID3D12CommandAllocator =
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)? };
        let list: ID3D12GraphicsCommandList = unsafe {
            device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)?
        };
        unsafe { list.Close()? };
        Ok(Uploader {
            device: device.clone(),
            queue: unsafe {
                device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    ..Default::default()
                })?
            },
            fence: unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE)? },
            event: unsafe { CreateEventW(None, false, false, None)? },
            buffer: buffer.context("no upload buffer")?,
            allocator,
            list,
            value: 0,
        })
    }

    fn write(&mut self, frame: &PoolFrame, luma: &[u8], chroma: &[u8], width: usize) -> Result<()> {
        let desc = unsafe { frame.resource().GetDesc() };
        let mut footprints = [D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); 2];
        unsafe {
            self.device.GetCopyableFootprints(
                &desc,
                0,
                2,
                0,
                Some(footprints.as_mut_ptr()),
                None,
                None,
                None,
            )
        };
        unsafe {
            let mut mapped = std::ptr::null_mut();
            self.buffer.Map(0, None, Some(&mut mapped))?;
            let base = mapped.cast::<u8>();
            for (footprint, plane) in footprints.iter().zip([luma, chroma]) {
                let pitch = footprint.Footprint.RowPitch as usize;
                for (row, line) in plane.chunks(width).enumerate() {
                    std::ptr::copy_nonoverlapping(
                        line.as_ptr(),
                        base.add(footprint.Offset as usize + row * pitch),
                        width,
                    );
                }
            }
            self.buffer.Unmap(0, None);
            self.allocator.Reset()?;
            self.list.Reset(&self.allocator, None)?;
            for (plane, footprint) in footprints.iter().enumerate() {
                let destination = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: ManuallyDrop::new(Some(frame.resource().clone())),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        SubresourceIndex: plane as u32,
                    },
                };
                let source = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: ManuallyDrop::new(Some(self.buffer.clone())),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: *footprint,
                    },
                };
                self.list
                    .CopyTextureRegion(&destination, 0, 0, 0, &source, None);
                ManuallyDrop::into_inner(destination.pResource);
                ManuallyDrop::into_inner(source.pResource);
            }
            self.list.Close()?;
            self.queue
                .ExecuteCommandLists(&[Some(self.list.cast::<ID3D12CommandList>()?)]);
            self.value += 1;
            self.queue.Signal(&self.fence, self.value)?;
            if self.fence.GetCompletedValue() < self.value {
                self.fence.SetEventOnCompletion(self.value, self.event)?;
                WaitForSingleObject(self.event, INFINITE);
            }
        }
        Ok(())
    }
}

fn options(preset: Preset) -> Options {
    Options {
        preset,
        tuning: Tuning::HighQuality,
        rate_control: RateControl::ConstantQuality(27),
        b_frames: 0,
    }
}

struct Session {
    encoder: NvencEncoder,
}

impl Session {
    fn open(
        device: &ID3D12Device,
        codec: Codec,
        (width, height): (u32, u32),
        preset: Preset,
        pool: usize,
        pictures: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<Session> {
        let encoder = NvencEncoder::with_pool_size(
            device,
            EncoderConfig::new(codec, width, height, FRAME_RATE),
            options(preset),
            pool,
        )?;
        let mut uploader = Uploader::new(device, width, height)?;
        let held: Vec<_> = (0..pool).map(|_| encoder.pool().acquire()).collect();
        for (slot, frame) in held.iter().enumerate() {
            let (luma, chroma) = &pictures[slot % pictures.len()];
            uploader.write(frame, luma, chroma, width as usize)?;
        }
        drop(held);
        Ok(Session { encoder })
    }

    fn run(mut self, frames: usize, start: &Barrier) -> Result<(usize, usize)> {
        start.wait();
        let (mut out_frames, mut out_bytes) = (0, 0);
        for index in 0..frames {
            let frame = self.encoder.pool().acquire();
            self.encoder.submit(
                EncoderFrame::D3d12(frame),
                Rational::new(index as i64, FRAME_RATE.num),
            )?;
            for packet in self.encoder.drain()? {
                out_frames += 1;
                out_bytes += packet.data.len();
            }
        }
        for packet in self.encoder.flush()? {
            out_frames += 1;
            out_bytes += packet.data.len();
        }
        Ok((out_frames, out_bytes))
    }
}

struct Run {
    fps: f64,
    cpu_ms: f64,
    mbps: f64,
}

fn once(
    device: &ID3D12Device,
    codec: Codec,
    size: (u32, u32),
    preset: Preset,
    sessions: usize,
    frames: usize,
    pool: usize,
    pictures: &[(Vec<u8>, Vec<u8>)],
) -> Result<Run> {
    let opened = (0..sessions)
        .map(|_| Session::open(device, codec, size, preset, pool, pictures))
        .collect::<Result<Vec<_>>>()?;
    let start = Barrier::new(sessions + 1);
    let (seconds, cpu, results) = std::thread::scope(|scope| {
        let handles: Vec<_> = opened
            .into_iter()
            .map(|session| {
                let start = &start;
                scope.spawn(move || session.run(frames, start))
            })
            .collect();
        start.wait();
        let began = Instant::now();
        let cpu = cpu_seconds();
        let results: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("session thread panicked"))
            .collect();
        (began.elapsed().as_secs_f64(), cpu_seconds() - cpu, results)
    });
    let (mut out_frames, mut out_bytes) = (0, 0);
    for result in results {
        let (f, b) = result?;
        out_frames += f;
        out_bytes += b;
    }
    Ok(Run {
        fps: out_frames as f64 / seconds,
        cpu_ms: cpu / out_frames as f64 * 1000.0,
        mbps: out_bytes as f64 * 8.0 / (out_frames as f64 / FRAME_RATE.num as f64) / 1e6,
    })
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

pub fn main() -> Result<()> {
    let args = parse_args()?;
    let device = nvidia_device()?;
    for &size in &args.sizes {
        let pictures: Vec<_> = (0..DISTINCT as u32)
            .map(|k| picture(k, size.0 as usize, size.1 as usize))
            .collect();
        for &codec in &args.codecs {
            for &preset in &args.presets {
                for &sessions in &args.sessions {
                    let label = format!(
                        "{codec:?} {}x{} {preset:?} sessions={sessions}",
                        size.0, size.1
                    );
                    let mut runs = Vec::new();
                    let warm = once(
                        &device,
                        codec,
                        size,
                        preset,
                        sessions,
                        args.frames,
                        args.pool,
                        &pictures,
                    );
                    if let Err(error) = warm {
                        println!("SKIP {label}: {error:#}");
                        continue;
                    }
                    for _ in 0..args.runs {
                        runs.push(once(
                            &device,
                            codec,
                            size,
                            preset,
                            sessions,
                            args.frames,
                            args.pool,
                            &pictures,
                        )?);
                    }
                    let mut fps: Vec<f64> = runs.iter().map(|r| r.fps).collect();
                    let min = fps.iter().copied().fold(f64::INFINITY, f64::min);
                    let max = fps.iter().copied().fold(0.0, f64::max);
                    let median_fps = median(&mut fps);
                    let cpu = median(&mut runs.iter().map(|r| r.cpu_ms).collect::<Vec<_>>());
                    let mbps = median(&mut runs.iter().map(|r| r.mbps).collect::<Vec<_>>());
                    println!(
                        "RESULT {label} frames={} runs={} median_fps={median_fps:.1} min={min:.1} max={max:.1} \
                         cpu={cpu:.3}ms/frame bitrate={mbps:.1}Mb/s",
                        args.frames, args.runs
                    );
                }
            }
        }
    }
    Ok(())
}
