#![cfg(target_os = "macos")]

use std::collections::HashSet;

use objc2_core_video::{
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use vrf::colour::{self, Chroma, Format, Rect};
use vrf::gpu::Gpu;
use vrf::gpu::convert::Converter;
use vrf::gpu::metal::PoolTargets;
use vrf_encode::videotoolbox::pool::{PixelFormat, PixelPool, PoolBuffer};

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
    targets: &mut PoolTargets,
    buffer: &PoolBuffer,
    src: &wgpu::TextureView,
) {
    let source = converter.source(src);
    let target = targets.get(gpu, converter, buffer).unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        converter.encode(&mut pass, &source, target, RECT, Chroma::Swscale);
    }
    gpu.queue.submit([encoder.finish()]);
}

fn read_planes(buffer: &PoolBuffer, bytes_per_sample: usize) -> Vec<u8> {
    let flags = CVPixelBufferLockFlags::ReadOnly;
    unsafe { CVPixelBufferLockBaseAddress(buffer.pixel_buffer(), flags) };
    let surface = buffer.surface();
    let mut out = Vec::new();
    for (plane, row_bytes, rows) in [
        (
            0,
            RECT.width as usize * bytes_per_sample,
            RECT.height as usize,
        ),
        (
            1,
            RECT.width as usize * bytes_per_sample,
            RECT.height as usize / 2,
        ),
    ] {
        let base = surface.base_address_of_plane(plane).as_ptr().cast::<u8>();
        let stride = surface.bytes_per_row_of_plane(plane);
        for y in 0..rows {
            out.extend_from_slice(unsafe {
                std::slice::from_raw_parts(base.add(y * stride), row_bytes)
            });
        }
    }
    unsafe { CVPixelBufferUnlockBaseAddress(buffer.pixel_buffer(), flags) };
    out
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

fn pool_for(format: Format) -> PixelPool {
    let pixel_format = match format {
        Format::Nv12 => PixelFormat::Nv12,
        Format::P010 => PixelFormat::P010,
    };
    PixelPool::new(RECT.width as usize, RECT.height as usize, pixel_format).unwrap()
}

#[test]
fn converting_into_a_pool_buffer_matches_the_cpu_oracle() {
    let gpu = Gpu::new().unwrap();
    let rgba = noise(1);
    let src = bgra_texture(&gpu, &rgba);
    for format in [Format::Nv12, Format::P010] {
        let pool = pool_for(format);
        let converter = Converter::new(&gpu, format);
        let mut targets = PoolTargets::default();
        let buffer = pool.acquire().unwrap();
        convert_into(&gpu, &converter, &mut targets, &buffer, &src);
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let want = colour::convert(&rgba, SOURCE.0, RECT, Chroma::Swscale, format);
        let bytes_per_sample = if format == Format::Nv12 { 1 } else { 2 };
        assert_same(
            &format!("{format:?} pool buffer"),
            &read_planes(&buffer, bytes_per_sample),
            &want,
        );
    }
}

#[test]
fn recycled_pool_buffers_convert_each_new_frame() {
    let gpu = Gpu::new().unwrap();
    let converter = Converter::new(&gpu, Format::Nv12);
    let mut targets = PoolTargets::default();
    let pool = pool_for(Format::Nv12);
    let frames: Vec<_> = (0..4).map(|seed| noise(100 + seed)).collect();
    let views: Vec<_> = frames.iter().map(|f| bgra_texture(&gpu, f)).collect();
    let mut seen = HashSet::new();
    for round in 0..5 {
        let held: Vec<_> = (0..6).map(|_| pool.acquire().unwrap()).collect();
        for (i, buffer) in held.iter().enumerate() {
            seen.insert(buffer.surface_id());
            convert_into(
                &gpu,
                &converter,
                &mut targets,
                buffer,
                &views[(round + i) % 4],
            );
        }
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        for (i, buffer) in held.iter().enumerate() {
            let want = colour::convert(
                &frames[(round + i) % 4],
                SOURCE.0,
                RECT,
                Chroma::Swscale,
                Format::Nv12,
            );
            assert_same(
                &format!("round {round} buffer {i}"),
                &read_planes(buffer, 1),
                &want,
            );
        }
    }
    assert!(seen.len() < 5 * 6, "the pool never recycled a buffer");
}
