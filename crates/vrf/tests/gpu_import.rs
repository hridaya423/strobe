#![cfg(target_os = "macos")]

#[allow(dead_code)]
mod gpu_support;

use gpu_support::{BGRA, Surface};
use vrf::gpu::Gpu;

#[test]
fn bgra_iosurface_round_trips_without_copy() {
    let gpu = Gpu::new().unwrap();
    let (w, h) = (64usize, 64usize);
    let pattern: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 + i / 256) as u8).collect();
    let surface = Surface::new(w, h, BGRA);
    surface.write_plane(0, w * 4, h, &pattern);

    let texture = gpu
        .import_iosurface(&surface.surface, 0, wgpu::TextureFormat::Bgra8Unorm)
        .unwrap();

    let readback = gpu.read_texture(&texture).unwrap();
    assert_eq!(readback, pattern);
}

#[test]
fn imported_texture_sees_later_cpu_writes() {
    let gpu = Gpu::new().unwrap();
    let (w, h) = (64usize, 64usize);
    let surface = Surface::new(w, h, BGRA);
    surface.write_plane(0, w * 4, h, &vec![0u8; w * h * 4]);
    let texture = gpu
        .import_iosurface(&surface.surface, 0, wgpu::TextureFormat::Bgra8Unorm)
        .unwrap();

    let second: Vec<u8> = (0..w * h * 4).map(|i| (255 - i % 251) as u8).collect();
    surface.write_plane(0, w * 4, h, &second);

    assert_eq!(gpu.read_texture(&texture).unwrap(), second);
}

#[test]
fn nv12_planes_import_as_r8_and_rg8() {
    let gpu = Gpu::new().unwrap();
    let (w, h) = (64usize, 48usize);
    let surface = Surface::new(w, h, gpu_support::NV12);
    let y: Vec<u8> = (0..w * h).map(|i| (i * 3) as u8).collect();
    let cbcr: Vec<u8> = (0..w * h / 2).map(|i| (i * 5 + 1) as u8).collect();
    surface.write_plane(0, w, h, &y);
    surface.write_plane(1, w, h / 2, &cbcr);

    let y_tex = gpu
        .import_iosurface(&surface.surface, 0, wgpu::TextureFormat::R8Uint)
        .unwrap();
    let c_tex = gpu
        .import_iosurface(&surface.surface, 1, wgpu::TextureFormat::Rg8Uint)
        .unwrap();

    assert_eq!((y_tex.width(), y_tex.height()), (64, 48));
    assert_eq!((c_tex.width(), c_tex.height()), (32, 24));
    assert_eq!(gpu.read_texture(&y_tex).unwrap(), y);
    assert_eq!(gpu.read_texture(&c_tex).unwrap(), cbcr);
}
