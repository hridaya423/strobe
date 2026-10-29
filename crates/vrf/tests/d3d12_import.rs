#![cfg(windows)]

mod gpu_support;

use std::time::Duration;

use gpu_support::{D3d11, SharedTexture};
use vrf::gpu::Gpu;
use vrf::gpu::d3d12::SharedHandle;

fn duplicate(shared: &SharedTexture) -> SharedHandle {
    unsafe { SharedHandle::duplicate(shared.handle()) }.unwrap()
}

fn owned_like(gpu: &Gpu, source: &wgpu::Texture) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("owned"),
        size: source.size(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: source.format(),
        usage: wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn copy_into(gpu: &Gpu, source: &wgpu::Texture, owned: &wgpu::Texture) {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_texture(source.as_image_copy(), owned.as_image_copy(), source.size());
    gpu.queue.submit([encoder.finish()]);
}

fn read_pixel(gpu: &Gpu, texture: &wgpu::Texture, x: u32, y: u32) -> [u8; 4] {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    buffer.map_async(wgpu::MapMode::Read, .., |r| r.unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let mapped = buffer.get_mapped_range(..).unwrap();
    [mapped[0], mapped[1], mapped[2], mapped[3]]
}

fn colour_of(frame: u32) -> [u8; 4] {
    [
        (255 - (frame & 255)) as u8,
        (frame >> 8) as u8,
        (frame & 255) as u8,
        255,
    ]
}

fn imported_copy(gpu: &Gpu, handle: &SharedHandle) -> wgpu::Texture {
    let source = gpu
        .import_shared_handle(handle, wgpu::TextureUsages::COPY_SRC)
        .unwrap();
    let owned = owned_like(gpu, &source);
    copy_into(gpu, &source, &owned);
    owned
}

#[test]
fn shared_handle_round_trips_through_a_copy() {
    let gpu = Gpu::new().unwrap();
    let d3d11 = D3d11::new(gpu.luid());
    let (w, h) = (64usize, 64usize);
    let pattern: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 + i / 256) as u8).collect();
    let shared = d3d11.shared_texture(w as u32, h as u32);
    d3d11.write(&shared, &pattern);

    let owned = imported_copy(&gpu, &duplicate(&shared));

    assert_eq!(gpu.read_texture(&owned).unwrap(), pattern);
}

#[test]
fn reimporting_the_handle_sees_later_writes() {
    let gpu = Gpu::new().unwrap();
    let d3d11 = D3d11::new(gpu.luid());
    let (w, h) = (64usize, 64usize);
    let shared = d3d11.shared_texture(w as u32, h as u32);
    let handle = duplicate(&shared);
    for round in 0..3u8 {
        let pattern: Vec<u8> = (0..w * h * 4)
            .map(|i| (i as u8).wrapping_mul(round + 1))
            .collect();
        d3d11.write(&shared, &pattern);
        let owned = imported_copy(&gpu, &handle);
        assert_eq!(gpu.read_texture(&owned).unwrap(), pattern, "round {round}");
    }
}

#[test]
fn the_duplicate_outlives_the_original_handle() {
    let gpu = Gpu::new().unwrap();
    let d3d11 = D3d11::new(gpu.luid());
    let shared = d3d11.shared_texture(32, 32);
    d3d11.clear(&shared, [10, 20, 30, 255]);
    let handle = duplicate(&shared);
    drop(shared);
    let owned = imported_copy(&gpu, &handle);
    assert_eq!(read_pixel(&gpu, &owned, 5, 5), [10, 20, 30, 255]);
}

#[test]
fn a_gpu_opened_by_luid_imports_handles_from_that_adapter() {
    let adapters = Gpu::adapters();
    assert!(!adapters.is_empty());
    for (name, luid) in adapters {
        let gpu = Gpu::with_luid(luid).unwrap();
        assert_eq!(gpu.luid(), luid, "{name}");
        let d3d11 = D3d11::new(luid);
        let shared = d3d11.shared_texture(32, 32);
        d3d11.clear(&shared, [1, 2, 3, 255]);
        let owned = imported_copy(&gpu, &duplicate(&shared));
        assert_eq!(read_pixel(&gpu, &owned, 0, 0), [1, 2, 3, 255], "{name}");
    }
}

#[test]
fn a_handle_from_another_adapter_does_not_open() {
    let adapters = Gpu::adapters();
    if adapters.len() < 2 {
        return;
    }
    let gpu = Gpu::with_luid(adapters[0].1).unwrap();
    let d3d11 = D3d11::new(adapters[1].1);
    let shared = d3d11.shared_texture(32, 32);
    let result = gpu.import_shared_handle(&duplicate(&shared), wgpu::TextureUsages::COPY_SRC);
    assert!(result.is_err());
}

fn deferred_frames(frames: u32, delay: impl Fn(u32) -> Duration) {
    let gpu = Gpu::new().unwrap();
    let d3d11 = D3d11::new(gpu.luid());
    let (w, h) = (1920, 1080);
    let pool: Vec<_> = (0..3).map(|_| d3d11.shared_texture(w, h)).collect();
    let mut owned = None;
    for frame in 0..frames {
        let shared = &pool[frame as usize % pool.len()];
        d3d11.clear(shared, colour_of(frame));
        let handle = duplicate(shared);
        std::thread::sleep(delay(frame));
        let source = gpu
            .import_shared_handle(&handle, wgpu::TextureUsages::COPY_SRC)
            .unwrap();
        let target = owned.get_or_insert_with(|| owned_like(&gpu, &source));
        copy_into(&gpu, &source, target);
        drop(source);
        for (x, y) in [
            (0, 0),
            (w - 1, 0),
            (0, h - 1),
            (w - 1, h - 1),
            (w / 2, h / 2),
        ] {
            assert_eq!(
                read_pixel(&gpu, target, x, y),
                colour_of(frame),
                "frame {frame} at {x},{y}"
            );
        }
    }
}

#[test]
fn deferred_import_is_exact_over_5000_frames_with_an_injected_delay() {
    deferred_frames(5000, |frame| {
        Duration::from_micros(u64::from(frame % 7) * 300)
    });
}
