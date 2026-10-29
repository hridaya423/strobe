#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::Result;
use vrf_encode::nvenc::{
    NvencEncoder, Options, POOL_SIZE, PixelFormat, PoolFrame, Preset, RateControl, Ready, Tuning,
    capabilities, nvidia_device,
};
use vrf_encode::{
    Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Quality, Rational, StreamInfo,
};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::Interface;

#[allow(dead_code)]
#[path = "../../vrf/src/mux.rs"]
mod mux;

use mux::{Container, Muxer, Sample, StreamConfig};

const WIDTH: usize = 640;
const HEIGHT: usize = 360;
const FRAMES: usize = 60;
const UPLOADS_IN_FLIGHT: usize = 8;

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

struct Planes<'a> {
    luma: &'a mut [u8],
    luma_pitch: usize,
    chroma: &'a mut [u8],
    chroma_pitch: usize,
    ten_bit: bool,
}

struct Upload {
    buffer: ID3D12Resource,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    last: u64,
}

struct Uploader {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    fence: ID3D12Fence,
    event: HANDLE,
    ring: Vec<Upload>,
    value: u64,
}

impl Uploader {
    fn new(device: &ID3D12Device, width: u32, height: u32, format: PixelFormat) -> Uploader {
        let probe = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: width as u64,
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format.dxgi(),
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        let mut total = 0u64;
        unsafe {
            device.GetCopyableFootprints(&probe, 0, 2, 0, None, None, None, Some(&mut total))
        };
        let ring = (0..UPLOADS_IN_FLIGHT)
            .map(|_| unsafe {
                let mut buffer: Option<ID3D12Resource> = None;
                device
                    .CreateCommittedResource(
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
                            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                            ..Default::default()
                        },
                        D3D12_RESOURCE_STATE_GENERIC_READ,
                        None,
                        &mut buffer,
                    )
                    .unwrap();
                let allocator: ID3D12CommandAllocator = device
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                    .unwrap();
                let list: ID3D12GraphicsCommandList = device
                    .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                    .unwrap();
                list.Close().unwrap();
                Upload {
                    buffer: buffer.unwrap(),
                    allocator,
                    list,
                    last: 0,
                }
            })
            .collect();
        Uploader {
            device: device.clone(),
            queue: unsafe {
                device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                        ..Default::default()
                    })
                    .unwrap()
            },
            fence: unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap() },
            event: unsafe { CreateEventW(None, false, false, None).unwrap() },
            ring,
            value: 0,
        }
    }

    fn wait(&self, value: u64) {
        unsafe {
            if self.fence.GetCompletedValue() < value {
                self.fence.SetEventOnCompletion(value, self.event).unwrap();
                WaitForSingleObject(self.event, INFINITE);
            }
        }
    }

    fn settle(&self) {
        self.wait(self.value);
    }

    fn write(&mut self, frame: &mut PoolFrame, fill: impl FnOnce(&mut Planes)) {
        let ten_bit = frame.format() == PixelFormat::P010;
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
        let index = self.value as usize % UPLOADS_IN_FLIGHT;
        self.wait(self.ring[index].last);
        let upload = &self.ring[index];
        let (_, height) = frame.size();
        let rows = [height as usize, height as usize / 2];
        unsafe {
            let mut mapped = std::ptr::null_mut();
            upload.buffer.Map(0, None, Some(&mut mapped)).unwrap();
            let base = mapped.cast::<u8>();
            let (first, second) = (&footprints[0], &footprints[1]);
            let luma = std::slice::from_raw_parts_mut(
                base.add(first.Offset as usize),
                first.Footprint.RowPitch as usize * rows[0],
            );
            let chroma = std::slice::from_raw_parts_mut(
                base.add(second.Offset as usize),
                second.Footprint.RowPitch as usize * rows[1],
            );
            fill(&mut Planes {
                luma,
                luma_pitch: first.Footprint.RowPitch as usize,
                chroma,
                chroma_pitch: second.Footprint.RowPitch as usize,
                ten_bit,
            });
            upload.buffer.Unmap(0, None);
            upload.allocator.Reset().unwrap();
            upload.list.Reset(&upload.allocator, None).unwrap();
            for (plane, footprint) in footprints.iter().enumerate() {
                let destination = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::ManuallyDrop::new(Some(frame.resource().clone())),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        SubresourceIndex: plane as u32,
                    },
                };
                let source = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::ManuallyDrop::new(Some(upload.buffer.clone())),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: *footprint,
                    },
                };
                upload
                    .list
                    .CopyTextureRegion(&destination, 0, 0, 0, &source, None);
                std::mem::ManuallyDrop::into_inner(destination.pResource);
                std::mem::ManuallyDrop::into_inner(source.pResource);
            }
            upload.list.Close().unwrap();
            self.queue
                .ExecuteCommandLists(&[Some(upload.list.cast::<ID3D12CommandList>().unwrap())]);
            self.value += 1;
            self.queue.Signal(&self.fence, self.value).unwrap();
        }
        self.ring[index].last = self.value;
        frame.ready = Some(Ready {
            fence: self.fence.clone(),
            value: self.value,
        });
    }
}

fn flat(index: usize) -> impl FnOnce(&mut Planes) {
    move |planes| {
        let luma = 16 + 3 * index;
        for (plane, pitch, value) in [
            (&mut *planes.luma, planes.luma_pitch, luma),
            (&mut *planes.chroma, planes.chroma_pitch, 128),
        ] {
            for line in plane.chunks_mut(pitch) {
                if planes.ten_bit {
                    let sample = ((value as u16) << 2) << 6;
                    for pair in line.as_chunks_mut::<2>().0 {
                        *pair = sample.to_le_bytes();
                    }
                } else {
                    line.fill(value as u8);
                }
            }
        }
    }
}

fn hash(x: u32, y: u32, k: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B) ^ k.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^ (h >> 12)
}

fn moving(k: u32, width: usize, height: usize) -> impl FnOnce(&mut Planes) {
    move |planes| {
        for y in 0..height {
            for x in 0..width {
                let base = (x as u32 * 3 / 8 + y as u32 / 3 + k * 9) & 0xFF;
                let noise = hash(x as u32, y as u32, k) & 15;
                let block = (x as i32 - (k as i32 * 60) % (width as i32 - 220)).unsigned_abs()
                    < 100
                    && (y as i32 - height as i32 * 2 / 5).unsigned_abs() < 100;
                planes.luma[y * planes.luma_pitch + x] = if block {
                    235
                } else {
                    (16 + (base * 200 / 255) + noise).min(235) as u8
                };
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                planes.chroma[y * planes.chroma_pitch + x * 2] =
                    (64 + ((x as u32 + k * 5) & 127)) as u8;
                planes.chroma[y * planes.chroma_pitch + x * 2 + 1] =
                    (64 + ((y as u32 * 2 + k * 3) & 127)) as u8;
            }
        }
    }
}

fn config(codec: Codec, frame_rate: Rational) -> EncoderConfig {
    let mut config = EncoderConfig::new(codec, WIDTH as u32, HEIGHT as u32, frame_rate);
    config.keyframe_interval = 30;
    config
}

fn options(b_frames: u32) -> Options {
    Options {
        preset: Preset::P4,
        tuning: Tuning::HighQuality,
        rate_control: RateControl::ConstantQuality(24),
        b_frames,
    }
}

fn pts_of(index: usize, frame_rate: Rational) -> Rational {
    Rational::new(index as i64 * frame_rate.den, frame_rate.num)
}

fn encode(
    config: EncoderConfig,
    options: Options,
    frames: usize,
) -> Result<(Vec<Packet>, StreamInfo)> {
    let device = nvidia_device()?;
    let mut encoder = NvencEncoder::new(&device, config.clone(), options)?;
    let mut uploader = Uploader::new(
        &device,
        config.width,
        config.height,
        encoder.pool().format(),
    );
    let mut packets = Vec::new();
    for index in 0..frames {
        let mut frame = encoder.pool().acquire();
        uploader.write(&mut frame, flat(index));
        encoder.submit(EncoderFrame::D3d12(frame), pts_of(index, config.frame_rate))?;
        packets.extend(encoder.drain()?);
    }
    packets.extend(encoder.flush()?);
    let info = encoder.finish()?;
    Ok((packets, info))
}

fn same(a: Rational, b: Rational) -> bool {
    a.num * b.den == b.num * a.den
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

fn write_mp4(file: &Path, packets: Vec<Packet>, info: &StreamInfo, frame_rate: Rational) {
    let mut muxer = Muxer::create(file, Container::Mp4, frame_rate.num as u32).unwrap();
    muxer
        .start_chunk(StreamConfig::try_from(info).unwrap())
        .unwrap();
    for packet in packets {
        muxer
            .write(&Sample::from_packet(packet, info.timebase, frame_rate.den as u32).unwrap())
            .unwrap();
    }
    muxer.finish().unwrap();
}

fn check_packets(packets: &[Packet], info: &StreamInfo, frame_rate: Rational, reorders: bool) {
    assert_eq!(packets.len(), FRAMES);
    assert_eq!((info.width, info.height), (WIDTH as u32, HEIGHT as u32));
    assert_eq!(info.timebase, Rational::new(1, frame_rate.num));
    let mut by_pts: Vec<&Packet> = packets.iter().collect();
    by_pts.sort_by_key(|p| p.pts.num * 1_000_000 / p.pts.den);
    for (index, packet) in by_pts.iter().enumerate() {
        assert!(
            same(packet.pts, pts_of(index, frame_rate)),
            "frame {index}: pts {:?}",
            packet.pts
        );
        assert!(!packet.data.is_empty());
    }
    let keyframes: Vec<usize> = (0..FRAMES).filter(|i| by_pts[*i].keyframe).collect();
    assert_eq!(keyframes, [0, 30]);
    for pair in packets.windows(2) {
        let (a, b) = (pair[0].dts, pair[1].dts);
        assert!(
            a.num * b.den < b.num * a.den,
            "dts must increase: {a:?} then {b:?}"
        );
    }
    for packet in packets {
        let (dts, pts) = (packet.dts, packet.pts);
        assert!(
            dts.num * pts.den <= pts.num * dts.den,
            "dts {dts:?} after pts {pts:?}"
        );
    }
    if reorders {
        assert!(
            packets.windows(2).any(|pair| {
                pair[1].pts.num * pair[0].pts.den < pair[0].pts.num * pair[1].pts.den
            }),
            "no packet left presentation order, so no B-frame was used"
        );
    } else {
        assert!(
            packets.windows(2).all(|pair| {
                pair[0].pts.num * pair[1].pts.den < pair[1].pts.num * pair[0].pts.den
            })
        );
    }
}

fn check_mp4(codec: Codec, ten_bit: bool, b_frames: u32, name: &str, expected: &str) {
    let _engine = engine();
    let frame_rate = Rational::new(30000, 1001);
    let mut config = config(codec, frame_rate);
    config.ten_bit = ten_bit;
    let (packets, info) = encode(config, options(b_frames), FRAMES).unwrap();
    check_packets(
        &packets,
        &info,
        frame_rate,
        b_frames > 0 && codec != Codec::Av1,
    );
    assert_eq!(info.codec, codec);
    assert!(!info.config.is_empty());

    let scratch = Scratch::new(name);
    let file = scratch.path("out.mp4");
    write_mp4(&file, packets, &info, frame_rate);
    assert_eq!(
        probe(&file, "stream=codec_name,width,height,nb_read_frames"),
        format!("{expected},{WIDTH},{HEIGHT},60")
    );
    if ten_bit {
        assert_eq!(probe(&file, "stream=pix_fmt"), "yuv420p10le");
    }
    let indexes = decoded_indexes(&file, ten_bit);
    assert_eq!(indexes, (0..FRAMES as i64).collect::<Vec<_>>());
    assert_eq!(indexes[37], 37);
}

#[test]
fn h264_encodes_60_frames_and_frame_37_decodes_as_37() {
    check_mp4(Codec::H264, false, 0, "nv-h264", "h264");
}

#[test]
fn hevc_encodes_60_frames_and_frame_37_decodes_as_37() {
    check_mp4(Codec::Hevc, false, 0, "nv-hevc", "hevc");
}

#[test]
fn hevc_main10_encodes_p010_frames() {
    check_mp4(Codec::Hevc, true, 0, "nv-hevc10", "hevc");
}

#[test]
fn av1_encodes_60_frames_and_frame_37_decodes_as_37() {
    check_mp4(Codec::Av1, false, 0, "nv-av1", "av1");
}

#[test]
fn av1_encodes_10_bit_p010_frames() {
    check_mp4(Codec::Av1, true, 0, "nv-av1-10", "av1");
}

#[test]
fn h264_with_b_frames_keeps_presentation_order_and_valid_dts() {
    check_mp4(Codec::H264, false, 2, "nv-h264-b", "h264");
}

#[test]
fn hevc_with_b_frames_keeps_presentation_order_and_valid_dts() {
    check_mp4(Codec::Hevc, false, 2, "nv-hevc-b", "hevc");
}

#[test]
fn av1_with_b_frames_emits_temporal_units_in_presentation_order() {
    check_mp4(Codec::Av1, false, 2, "nv-av1-b", "av1");
}

fn check_late_drain(codec: Codec) {
    let _engine = engine();
    let watchdog = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        eprintln!("deadlock: 300 undrained frames did not finish within 60 s");
        std::process::exit(1);
    });
    let device = nvidia_device().unwrap();
    let mut encoder = NvencEncoder::new(
        &device,
        config(codec, Rational::new(30, 1)),
        Quality::Balanced.into(),
    )
    .unwrap();
    let mut uploader = Uploader::new(&device, WIDTH as u32, HEIGHT as u32, PixelFormat::Nv12);
    for index in 0..300 {
        let mut frame = encoder.pool().acquire();
        uploader.write(&mut frame, flat(index % 70));
        encoder
            .submit(EncoderFrame::D3d12(frame), Rational::new(index as i64, 30))
            .unwrap();
    }
    let packets = encoder.flush().unwrap();
    assert_eq!(packets.len(), 300);
    encoder.finish().unwrap();
    drop(watchdog);
}

#[test]
fn h264_caller_that_drains_only_at_the_end_does_not_deadlock() {
    check_late_drain(Codec::H264);
}

#[test]
fn hevc_caller_that_drains_only_at_the_end_does_not_deadlock() {
    check_late_drain(Codec::Hevc);
}

#[test]
fn av1_caller_that_drains_only_at_the_end_does_not_deadlock() {
    check_late_drain(Codec::Av1);
}

#[test]
fn lookahead_preset_with_b_frames_and_late_drain_does_not_deadlock() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let mut options = Options::from(Quality::High);
    options.b_frames = 3;
    let mut encoder =
        NvencEncoder::new(&device, config(Codec::Hevc, Rational::new(30, 1)), options).unwrap();
    let mut uploader = Uploader::new(&device, WIDTH as u32, HEIGHT as u32, PixelFormat::Nv12);
    for index in 0..200 {
        let mut frame = encoder.pool().acquire();
        uploader.write(&mut frame, flat(index % 70));
        encoder
            .submit(EncoderFrame::D3d12(frame), Rational::new(index as i64, 30))
            .unwrap();
    }
    assert_eq!(encoder.flush().unwrap().len(), 200);
}

#[test]
fn pool_holds_48_stable_textures_and_blocks_when_exhausted() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let encoder = NvencEncoder::new(
        &device,
        config(Codec::H264, Rational::new(30, 1)),
        Quality::Draft.into(),
    )
    .unwrap();
    let pool = encoder.pool();
    assert_eq!(POOL_SIZE, 48);
    let held: Vec<PoolFrame> = (0..POOL_SIZE).map(|_| pool.acquire()).collect();
    let pointers: std::collections::HashSet<_> = held
        .iter()
        .map(|f| f.resource().as_raw() as usize)
        .collect();
    assert_eq!(pointers.len(), POOL_SIZE);
    assert!(pool.try_acquire().is_none());
    let waiter = {
        let pool = pool.clone();
        std::thread::spawn(move || pool.acquire().resource().as_raw() as usize)
    };
    std::thread::sleep(Duration::from_millis(100));
    assert!(!waiter.is_finished());
    drop(held);
    assert!(pointers.contains(&waiter.join().unwrap()));
}

#[test]
fn ten_bit_is_rejected_for_h264() {
    let device = nvidia_device().unwrap();
    let mut config = config(Codec::H264, Rational::new(30, 1));
    config.ten_bit = true;
    let error = NvencEncoder::new(&device, config, Quality::Balanced.into())
        .err()
        .unwrap();
    assert!(error.to_string().contains("ten_bit"), "{error}");
}

#[test]
fn a_frame_from_another_encoders_pool_is_rejected() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let make = || {
        NvencEncoder::new(
            &device,
            config(Codec::H264, Rational::new(30, 1)),
            Quality::Draft.into(),
        )
        .unwrap()
    };
    let (mut first, second) = (make(), make());
    let frame = second.pool().acquire();
    let error = first
        .submit(EncoderFrame::D3d12(frame), Rational::new(0, 30))
        .err()
        .unwrap();
    assert!(error.to_string().contains("another encoder"), "{error}");
}

#[test]
fn pts_must_increase() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let mut encoder = NvencEncoder::new(
        &device,
        config(Codec::H264, Rational::new(30, 1)),
        Quality::Draft.into(),
    )
    .unwrap();
    let frame = encoder.pool().acquire();
    encoder
        .submit(EncoderFrame::D3d12(frame), Rational::new(1, 30))
        .unwrap();
    let frame = encoder.pool().acquire();
    let error = encoder
        .submit(EncoderFrame::D3d12(frame), Rational::new(1, 30))
        .err()
        .unwrap();
    assert!(error.to_string().contains("pts"), "{error}");
}

#[test]
fn capabilities_report_the_codecs_the_gpu_opens() {
    let device = nvidia_device().unwrap();
    let caps = capabilities(&device).unwrap();
    println!("{caps:#?}");
    assert!(caps.api_version.0 >= 12);
    let codecs: Vec<Codec> = caps.codecs.iter().map(|c| c.codec).collect();
    assert!(codecs.contains(&Codec::H264) && codecs.contains(&Codec::Hevc));
    assert!(caps.codecs.iter().all(|c| c.engines >= 1));
}

#[test]
fn quality_levels_order_the_bitrate() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let mut sizes = Vec::new();
    for quality in [Quality::Draft, Quality::Balanced, Quality::High] {
        let mut encoder = NvencEncoder::new(
            &device,
            config(Codec::H264, Rational::new(30, 1)),
            quality.into(),
        )
        .unwrap();
        let mut uploader = Uploader::new(&device, WIDTH as u32, HEIGHT as u32, PixelFormat::Nv12);
        let mut bytes = 0;
        for index in 0..FRAMES {
            let mut frame = encoder.pool().acquire();
            uploader.write(&mut frame, moving(index as u32, WIDTH, HEIGHT));
            encoder
                .submit(EncoderFrame::D3d12(frame), Rational::new(index as i64, 30))
                .unwrap();
        }
        for packet in encoder.flush().unwrap() {
            bytes += packet.data.len();
        }
        println!("{quality:?}: {bytes} bytes");
        sizes.push(bytes);
    }
    assert!(sizes[0] < sizes[1] && sizes[1] < sizes[2], "{sizes:?}");
}

fn session(
    device: &ID3D12Device,
    codec: Codec,
    preset: Preset,
    tuning: Tuning,
    frames: usize,
) -> (f64, usize) {
    let (width, height) = (1920usize, 1080usize);
    let mut config = EncoderConfig::new(codec, width as u32, height as u32, Rational::new(60, 1));
    config.keyframe_interval = 250;
    let options = Options {
        preset,
        tuning,
        rate_control: match tuning {
            Tuning::HighQuality => RateControl::Vbr(20_000_000),
            _ => RateControl::Cbr(20_000_000),
        },
        b_frames: 0,
    };
    let mut encoder = NvencEncoder::new(device, config, options).unwrap();
    let mut uploader = Uploader::new(device, width as u32, height as u32, PixelFormat::Nv12);
    let mut warm: Vec<PoolFrame> = (0..POOL_SIZE).map(|_| encoder.pool().acquire()).collect();
    for (k, frame) in warm.iter_mut().enumerate() {
        uploader.write(frame, moving(k as u32, width, height));
    }
    uploader.settle();
    drop(warm);
    let (mut count, mut bytes) = (0, 0);
    let start = Instant::now();
    for index in 0..frames {
        let frame = encoder.pool().acquire();
        encoder
            .submit(EncoderFrame::D3d12(frame), Rational::new(index as i64, 60))
            .unwrap();
        for packet in encoder.drain().unwrap() {
            count += 1;
            bytes += packet.data.len();
        }
    }
    for packet in encoder.flush().unwrap() {
        count += 1;
        bytes += packet.data.len();
    }
    let seconds = start.elapsed().as_secs_f64();
    assert_eq!(count, frames);
    (seconds, bytes)
}

#[test]
#[ignore = "run with --ignored --nocapture: 600 frames per cell, about three minutes"]
fn throughput_at_1080p_one_and_two_sessions() {
    let _engine = engine();
    let device = nvidia_device().unwrap();
    let frames = 600;
    println!("codec tuning preset sessions per_session_fps total_fps mbps_at_60");
    for tuning in [Tuning::HighQuality, Tuning::LowLatency] {
        for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
            for preset in [Preset::P1, Preset::P4, Preset::P7] {
                for sessions in [1usize, 2] {
                    let runs: Vec<(f64, usize)> = std::thread::scope(|scope| {
                        (0..sessions)
                            .map(|_| {
                                scope.spawn(|| session(&device, codec, preset, tuning, frames))
                            })
                            .collect::<Vec<_>>()
                            .into_iter()
                            .map(|handle| handle.join().unwrap())
                            .collect()
                    });
                    let per_session =
                        runs.iter().map(|(s, _)| frames as f64 / s).sum::<f64>() / sessions as f64;
                    let longest = runs.iter().map(|(s, _)| *s).fold(0.0, f64::max);
                    let total = (frames * sessions) as f64 / longest;
                    let mbps = runs[0].1 as f64 * 8.0 / frames as f64 * 60.0 / 1e6;
                    println!(
                        "{codec:?} {tuning:?} {preset:?} {sessions} {per_session:.1} {total:.1} {mbps:.1}"
                    );
                    assert!(
                        per_session >= 60.0,
                        "{codec:?} {preset:?} runs below real time"
                    );
                }
            }
        }
    }
}
