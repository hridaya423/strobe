#![cfg(windows)]

use std::mem::ManuallyDrop;
use std::path::PathBuf;
use std::process::Command;

use vrf::colour::{self, Chroma, Format, Rect};
use vrf::gpu::Gpu;
use vrf::gpu::convert::Converter;
use vrf::gpu::d3d12::{PoolTarget, Releaser};
use vrf::mux::{Container, Muxer};
use vrf::sink::Sink;
use vrf_encode::nvenc::{NvencEncoder, Options, PoolFrame};
use vrf_encode::{
    Codec, ColourSpace, Encoder, EncoderConfig, EncoderFrame, Matrix, Quality, Range, Rational,
};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::Interface;

const SOURCE: (u32, u32) = (1280, 720);
const RECT: Rect = Rect {
    x: 7,
    y: 9,
    width: 960,
    height: 540,
};

fn noise(seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut next = move || {
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    };
    let mut rgba = vec![255u8; (SOURCE.0 * SOURCE.1 * 4) as usize];
    for px in rgba.chunks_exact_mut(4) {
        px[..3].copy_from_slice(&next().to_le_bytes()[..3]);
    }
    rgba
}

fn bgra_texture(gpu: &Gpu, rgba: &[u8]) -> wgpu::TextureView {
    let mut bytes = rgba.to_vec();
    bytes.chunks_exact_mut(4).for_each(|px| px.swap(0, 2));
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: SOURCE.0,
            height: SOURCE.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SOURCE.0 * 4),
            rows_per_image: None,
        },
        texture.size(),
    );
    texture.create_view(&Default::default())
}

fn convert_into(
    gpu: &Gpu,
    converter: &Converter,
    releaser: &mut Releaser,
    frame: &PoolFrame,
    src: &wgpu::TextureView,
    chroma: Chroma,
) {
    let source = converter.source(src);
    let target = PoolTarget::new(gpu, converter, frame).unwrap();
    let mut commands = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = commands.begin_compute_pass(&Default::default());
        converter.encode(&mut pass, &source, &target.target, RECT, chroma);
    }
    gpu.queue.submit([commands.finish()]);
    let value = releaser.release(gpu, frame).unwrap();
    releaser.wait(value).unwrap();
}

struct Readback {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    fence: ID3D12Fence,
    event: HANDLE,
    value: u64,
}

impl Readback {
    fn new(device: &ID3D12Device) -> Readback {
        Readback {
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
            value: 0,
        }
    }

    fn planes(&mut self, frame: &PoolFrame, bytes_per_sample: usize) -> Vec<u8> {
        let desc = unsafe { frame.resource().GetDesc() };
        let mut footprints = [D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); 2];
        let mut total = 0u64;
        unsafe {
            self.device.GetCopyableFootprints(
                &desc,
                0,
                2,
                0,
                Some(footprints.as_mut_ptr()),
                None,
                None,
                Some(&mut total),
            )
        };
        let mut buffer: Option<ID3D12Resource> = None;
        unsafe {
            self.device
                .CreateCommittedResource(
                    &D3D12_HEAP_PROPERTIES {
                        Type: D3D12_HEAP_TYPE_READBACK,
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
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    None,
                    &mut buffer,
                )
                .unwrap();
        }
        let buffer = buffer.unwrap();
        let allocator: ID3D12CommandAllocator = unsafe {
            self.device
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                .unwrap()
        };
        let list: ID3D12GraphicsCommandList = unsafe {
            self.device
                .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                .unwrap()
        };
        for (plane, footprint) in footprints.iter().enumerate() {
            let source = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(frame.resource().clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: plane as u32,
                },
            };
            let destination = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(buffer.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: *footprint,
                },
            };
            unsafe {
                list.CopyTextureRegion(&destination, 0, 0, 0, &source, None);
                ManuallyDrop::into_inner(source.pResource);
                ManuallyDrop::into_inner(destination.pResource);
            }
        }
        unsafe {
            list.Close().unwrap();
            self.queue
                .ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>().unwrap())]);
            self.value += 1;
            self.queue.Signal(&self.fence, self.value).unwrap();
            self.fence
                .SetEventOnCompletion(self.value, self.event)
                .unwrap();
            WaitForSingleObject(self.event, INFINITE);
        }
        let mut out = Vec::new();
        unsafe {
            let mut mapped = std::ptr::null_mut();
            buffer.Map(0, None, Some(&mut mapped)).unwrap();
            let base = mapped.cast::<u8>();
            for (footprint, rows) in footprints
                .iter()
                .zip([RECT.height as usize, RECT.height as usize / 2])
            {
                let row_bytes = RECT.width as usize * bytes_per_sample;
                for y in 0..rows {
                    out.extend_from_slice(std::slice::from_raw_parts(
                        base.add(
                            footprint.Offset as usize + y * footprint.Footprint.RowPitch as usize,
                        ),
                        row_bytes,
                    ));
                }
            }
            buffer.Unmap(0, None);
        }
        out
    }
}

fn assert_same(what: &str, got: &[u8], want: &[u8]) {
    assert_eq!(got.len(), want.len(), "{what}: length");
    if let Some(i) = got.iter().zip(want).position(|(a, b)| a != b) {
        let wrong = got.iter().zip(want).filter(|(a, b)| a != b).count();
        panic!(
            "{what}: {wrong} bytes differ, first at {i}: got {} want {}",
            got[i], want[i]
        );
    }
}

fn encoder(gpu: &Gpu, codec: Codec, ten_bit: bool, pool: usize) -> NvencEncoder {
    let mut config = EncoderConfig::new(codec, RECT.width, RECT.height, Rational::new(30, 1));
    config.ten_bit = ten_bit;
    NvencEncoder::with_pool_size(
        &gpu.d3d12_device().unwrap(),
        config,
        Options::from(Quality::Balanced),
        pool,
    )
    .unwrap()
}

#[test]
fn converting_into_pool_textures_matches_the_cpu_oracle() {
    matches_the_cpu_oracle(
        &[
            (Format::Nv12, Codec::H264, false),
            (Format::P010, Codec::Hevc, true),
        ],
        Chroma::Swscale,
    );
}

#[test]
fn exact_full_range_bt601_into_pool_textures_matches_the_cpu_oracle() {
    matches_the_cpu_oracle(
        &[(Format::Nv12, Codec::H264, false)],
        Chroma::Exact(ColourSpace {
            matrix: Matrix::Bt601,
            range: Range::Full,
        }),
    );
}

fn matches_the_cpu_oracle(cases: &[(Format, Codec, bool)], chroma: Chroma) {
    let gpu = Gpu::new().unwrap();
    for &(format, codec, ten_bit) in cases {
        let encoder = encoder(&gpu, codec, ten_bit, 4);
        let converter = Converter::new(&gpu, format);
        let mut releaser = Releaser::new(&gpu).unwrap();
        let mut readback = Readback::new(&gpu.d3d12_device().unwrap());
        let frames: Vec<_> = (0..3).map(|seed| noise(10 + seed)).collect();
        let views: Vec<_> = frames.iter().map(|f| bgra_texture(&gpu, f)).collect();
        let bytes_per_sample = if ten_bit { 2 } else { 1 };
        for round in 0..6 {
            let held: Vec<_> = (0..4).map(|_| encoder.pool().acquire()).collect();
            for (i, frame) in held.iter().enumerate() {
                convert_into(
                    &gpu,
                    &converter,
                    &mut releaser,
                    frame,
                    &views[(round + i) % 3],
                    chroma,
                );
            }
            for (i, frame) in held.iter().enumerate() {
                let want = colour::convert(
                    &frames[(round + i) % 3],
                    SOURCE.0,
                    RECT,
                    chroma,
                    format,
                );
                assert_same(
                    &format!("{format:?} round {round} texture {i}"),
                    &readback.planes(frame, bytes_per_sample),
                    &want,
                );
            }
        }
    }
}

fn run(program: &str, args: &[&str]) -> Vec<u8> {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn frames_converted_into_the_pool_decode_to_their_own_colours() {
    let gpu = Gpu::new().unwrap();
    let converter = Converter::new(&gpu, Format::Nv12);
    let mut releaser = Releaser::new(&gpu).unwrap();
    let mut encoder = encoder(&gpu, Codec::H264, false, 6);
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("d3d12_pool.mp4");
    let mut sink = Sink::new(Muxer::create(&out, Container::Mp4, 30).unwrap(), 1);
    let grey = |k: u32| (20 + k * 7) as u8;
    let views: Vec<_> = (0..24)
        .map(|k| {
            let mut rgba = vec![grey(k); (SOURCE.0 * SOURCE.1 * 4) as usize];
            rgba.chunks_exact_mut(4).for_each(|px| px[3] = 255);
            bgra_texture(&gpu, &rgba)
        })
        .collect();
    for (k, view) in views.iter().enumerate() {
        let frame = encoder.pool().acquire();
        convert_into(
            &gpu,
            &converter,
            &mut releaser,
            &frame,
            view,
            Chroma::Swscale,
        );
        encoder
            .submit(EncoderFrame::D3d12(frame), Rational::new(k as i64, 30))
            .unwrap();
        let packets = encoder.drain().unwrap();
        sink.write(&encoder, packets).unwrap();
    }
    let packets = encoder.flush().unwrap();
    sink.write(&encoder, packets).unwrap();
    sink.into_muxer().finish().unwrap();
    let raw = run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            out.to_str().unwrap(),
            "-vf",
            "format=gray",
            "-f",
            "rawvideo",
            "-",
        ],
    );
    let size = (RECT.width * RECT.height) as usize;
    assert_eq!(raw.len(), 24 * size);
    for (k, frame) in raw.chunks_exact(size).enumerate() {
        let mean = frame.iter().map(|&v| v as u64).sum::<u64>() / size as u64;
        let want = grey(k as u32) as u64;
        assert!(
            mean.abs_diff(want) <= 3,
            "frame {k} decoded to grey {mean}, expected {want}"
        );
    }
}
