use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use vrf_encode::nvenc::{PixelFormat, PoolFrame, Ready};
use vrf_encode::{Packet, Rational, StreamInfo};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::Interface;

#[allow(dead_code)]
#[path = "../../../vrf/src/mux.rs"]
mod mux;

use mux::{Container, Muxer, Sample, StreamConfig};

pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 360;
pub const FRAMES: usize = 60;
const UPLOADS_IN_FLIGHT: usize = 8;

pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    pub fn path(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub struct Planes<'a> {
    pub luma: &'a mut [u8],
    pub luma_pitch: usize,
    pub chroma: &'a mut [u8],
    pub chroma_pitch: usize,
    pub ten_bit: bool,
}

pub struct Upload {
    buffer: ID3D12Resource,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    last: u64,
}

pub struct Uploader {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    fence: ID3D12Fence,
    event: HANDLE,
    ring: Vec<Upload>,
    value: u64,
}

impl Uploader {
    pub fn new(device: &ID3D12Device, width: u32, height: u32, format: PixelFormat) -> Uploader {
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

    pub fn wait(&self, value: u64) {
        unsafe {
            if self.fence.GetCompletedValue() < value {
                self.fence.SetEventOnCompletion(value, self.event).unwrap();
                WaitForSingleObject(self.event, INFINITE);
            }
        }
    }

    pub fn settle(&self) {
        self.wait(self.value);
    }

    pub fn write(&mut self, frame: &mut PoolFrame, fill: impl FnOnce(&mut Planes)) {
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

pub fn flat(index: usize) -> impl FnOnce(&mut Planes) {
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

pub fn hash(x: u32, y: u32, k: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B) ^ k.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^ (h >> 12)
}

pub fn moving(k: u32, width: usize, height: usize) -> impl FnOnce(&mut Planes) {
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

pub fn pts_of(index: usize, frame_rate: Rational) -> Rational {
    Rational::new(index as i64 * frame_rate.den, frame_rate.num)
}

pub fn same(a: Rational, b: Rational) -> bool {
    a.num * b.den == b.num * a.den
}

pub fn run(program: &str, args: &[&str]) -> Vec<u8> {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub fn probe(file: &Path, entries: &str) -> String {
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

pub fn decoded_indexes(file: &Path, ten_bit: bool) -> Vec<i64> {
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

pub fn write_mp4(file: &Path, packets: Vec<Packet>, info: &StreamInfo, frame_rate: Rational) {
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
