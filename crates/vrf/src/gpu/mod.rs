pub mod convert;
mod metal;

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::MTLDevice;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: wgpu::Adapter,
    pub mtl_device: Retained<ProtocolObject<dyn MTLDevice>>,
}

impl Gpu {
    pub fn new() -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .context("no Metal adapter")?;
        let features = wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES
            | (wgpu::Features::TIMESTAMP_QUERY & adapter.features());
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("vrf"),
                required_features: features,
                required_limits: adapter.limits(),
                ..Default::default()
            }))?;
        let mtl_device = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .context("device is not Metal")?
            .raw_device()
            .clone();
        Ok(Self {
            device,
            queue,
            adapter,
            mtl_device,
        })
    }

    pub fn read_texture(&self, texture: &wgpu::Texture) -> Result<Vec<u8>> {
        let row_bytes = texture.width() * texture.format().block_copy_size(None).unwrap();
        let stride = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(stride * texture.height()),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(texture.height()),
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |r| r.unwrap());
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let mapped = buffer.get_mapped_range(..)?;
        Ok(mapped
            .chunks(stride as usize)
            .flat_map(|row| &row[..row_bytes as usize])
            .copied()
            .collect())
    }
}
