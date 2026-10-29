use cef::AcceleratedPaintInfo;
use objc2_io_surface::IOSurfaceRef;
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTextureDescriptor, MTLTextureType,
    MTLTextureUsage,
};
use std::ptr::NonNull;

static CPU_READBACKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn cpu_readbacks() -> u64 {
    CPU_READBACKS.load(std::sync::atomic::Ordering::Relaxed)
}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

pub struct Frame {
    pub texture: wgpu::Texture,
    pub width: u32,
    pub height: u32,
    pub swap_rb: bool,
}

impl Gpu {
    pub fn new() -> Gpu {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        Gpu { device, queue }
    }

    pub fn wait(&self) {
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
    }

    pub fn import_frame(&self, info: &AcceleratedPaintInfo) -> Frame {
        let raw: cef::sys::cef_color_type_t = *info.format.as_ref();
        let (format, swap_rb) = match raw {
            cef::sys::cef_color_type_t::CEF_COLOR_TYPE_BGRA_8888 => {
                (wgpu::TextureFormat::Bgra8Unorm, true)
            }
            cef::sys::cef_color_type_t::CEF_COLOR_TYPE_RGBA_8888 => {
                (wgpu::TextureFormat::Rgba8Unorm, false)
            }
            other => panic!("unsupported CEF colour type {other:?}"),
        };
        let width = info.extra.coded_size.width as u32;
        let height = info.extra.coded_size.height as u32;
        let texture = unsafe {
            self.import_plane(
                info.shared_texture_io_surface,
                0,
                width,
                height,
                format,
                wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
                MTLTextureUsage::ShaderRead,
                wgpu::TextureUses::RESOURCE,
            )
        };
        Frame {
            texture,
            width,
            height,
            swap_rb,
        }
    }

    pub unsafe fn import_plane(
        &self,
        surface: *mut std::ffi::c_void,
        plane: usize,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
        mtl_usage: MTLTextureUsage,
        uses: wgpu::TextureUses,
    ) -> wgpu::Texture {
        let surface = NonNull::new(surface.cast::<IOSurfaceRef>()).expect("null IOSurface");
        let desc = wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        };
        let mtl_desc = MTLTextureDescriptor::new();
        unsafe {
            mtl_desc.setWidth(width as _);
            mtl_desc.setHeight(height as _);
            mtl_desc.setTextureType(MTLTextureType::Type2D);
            mtl_desc.setPixelFormat(match format {
                wgpu::TextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
                wgpu::TextureFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
                wgpu::TextureFormat::R8Unorm => MTLPixelFormat::R8Unorm,
                wgpu::TextureFormat::Rg8Unorm => MTLPixelFormat::RG8Unorm,
                other => panic!("unsupported import format {other:?}"),
            });
            mtl_desc.setUsage(mtl_usage);
            mtl_desc.setStorageMode(MTLStorageMode::Shared);
        }
        let hal_texture = unsafe {
            let guard = self
                .device
                .as_hal::<wgpu::wgc::api::Metal>()
                .expect("wgpu is not on Metal");
            let raw = guard
                .raw_device()
                .newTextureWithDescriptor_iosurface_plane(&mtl_desc, surface.as_ref(), plane)
                .expect("newTextureWithDescriptor:iosurface:plane: failed");
            <wgpu::wgc::api::Metal as wgpu::hal::Api>::Device::texture_from_raw(
                raw,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
                None,
            )
        };
        unsafe {
            self.device
                .create_texture_from_hal::<wgpu::wgc::api::Metal>(hal_texture, &desc, uses)
        }
    }

    pub fn read_points(&self, frame: &Frame, origin: (u32, u32), points: &[(u32, u32)]) -> Vec<[u8; 4]> {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (points.len() * 256) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        for (i, (x, y)) in points.iter().enumerate() {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &frame.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: origin.0 + x,
                        y: origin.1 + y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: (i * 256) as u64,
                        bytes_per_row: None,
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.queue.submit([encoder.finish()]);
        let bytes = self.map_all(&buffer);
        bytes
            .chunks_exact(256)
            .map(|p| &p[..4])
            .map(|p| {
                if frame.swap_rb {
                    [p[2], p[1], p[0], p[3]]
                } else {
                    [p[0], p[1], p[2], p[3]]
                }
            })
            .collect()
    }

    pub fn read_frame(&self, frame: &Frame, origin: (u32, u32), size: (u32, u32)) -> Vec<u8> {
        let row = size.0 * 4;
        let padded = row.div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (padded * size.1) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &frame.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: origin.0,
                    y: origin.1,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(size.1),
                },
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let bytes = self.map_all(&buffer);
        let mut out = Vec::with_capacity((row * size.1) as usize);
        for r in 0..size.1 as usize {
            out.extend_from_slice(&bytes[r * padded as usize..r * padded as usize + row as usize]);
        }
        out
    }

    fn map_all(&self, buffer: &wgpu::Buffer) -> Vec<u8> {
        CPU_READBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        self.wait();
        let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
        buffer.unmap();
        data
    }
}

pub fn save_png(path: &std::path::Path, bgra: &[u8], swap_rb: bool, width: u32, height: u32) {
    let mut rgba = bgra.to_vec();
    if swap_rb {
        rgba.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.write_header().unwrap().write_image_data(&rgba).unwrap();
}
