use wgpu::util::DeviceExt;

use super::Gpu;
use crate::colour::{Chroma, Format, Rect};

/// One compute dispatch that reads an RGBA8 or BGRA8 texture, crops it to a
/// rect at any offset, and writes the Y and CbCr planes of NV12 or P010.
pub struct Converter {
    device: wgpu::Device,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    format: Format,
}

fn entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}

fn storage(format: wgpu::TextureFormat) -> wgpu::BindingType {
    wgpu::BindingType::StorageTexture {
        access: wgpu::StorageTextureAccess::WriteOnly,
        format,
        view_dimension: wgpu::TextureViewDimension::D2,
    }
}

impl Converter {
    pub fn new(gpu: &Gpu, format: Format) -> Self {
        let device = gpu.device.clone();
        let (y_format, cbcr_format) = Self::plane_formats(format);
        let (y_binding, cbcr_binding, entry_point) = match format {
            Format::Nv12 => (2, 3, "convert8"),
            Format::P010 => (4, 5, "convert16"),
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("convert"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(y_binding, storage(y_format)),
                entry(cbcr_binding, storage(cbcr_format)),
            ],
        });
        let module = device.create_shader_module(wgpu::include_wgsl!("convert.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("convert"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("convert"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            device,
            pipeline,
            layout,
            format,
        }
    }

    /// Texture formats of the Y and CbCr planes: `R8Uint` and `Rg8Uint` for
    /// NV12, `R16Uint` and `Rg16Uint` for P010.
    pub fn plane_formats(format: Format) -> (wgpu::TextureFormat, wgpu::TextureFormat) {
        match format {
            Format::Nv12 => (wgpu::TextureFormat::R8Uint, wgpu::TextureFormat::Rg8Uint),
            Format::P010 => (wgpu::TextureFormat::R16Uint, wgpu::TextureFormat::Rg16Uint),
        }
    }

    /// Records the conversion of `rect` of `src` into `y` and `cbcr`. `y` must
    /// be at least `rect.width` by `rect.height` and `cbcr` half of that, and
    /// `rect` must have even dimensions and lie inside `src`.
    pub fn encode(
        &self,
        pass: &mut wgpu::ComputePass<'_>,
        src: &wgpu::TextureView,
        y: &wgpu::TextureView,
        cbcr: &wgpu::TextureView,
        rect: Rect,
        chroma: Chroma,
    ) {
        assert!(
            rect.width.is_multiple_of(2) && rect.height.is_multiple_of(2),
            "NV12 and P010 need even dimensions"
        );
        let params = [
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            u32::from(chroma == Chroma::Spec),
            self.format.bits(),
            0,
            0,
        ];
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("convert params"),
                contents: &params
                    .iter()
                    .flat_map(|v| v.to_ne_bytes())
                    .collect::<Vec<u8>>(),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let (y_binding, cbcr_binding) = match self.format {
            Format::Nv12 => (2, 3),
            Format::P010 => (4, 5),
        };
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("convert"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: y_binding,
                    resource: wgpu::BindingResource::TextureView(y),
                },
                wgpu::BindGroupEntry {
                    binding: cbcr_binding,
                    resource: wgpu::BindingResource::TextureView(cbcr),
                },
            ],
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            (rect.width / 2).div_ceil(16),
            (rect.height / 2).div_ceil(16),
            1,
        );
    }
}
