use super::Gpu;
use crate::colour::{Chroma, Format, Rect};

const IMMEDIATE_BYTES: usize = 24;

/// One compute dispatch that reads an RGBA8 or BGRA8 texture, crops it to a
/// rect at any offset, and writes the Y and CbCr planes of NV12 or P010.
///
/// The source and the destination planes are bound separately. A [`Target`]
/// lives as long as its planes do and a [`Source`] is cheap enough to make for
/// every frame, so a frame costs one small bind group and no buffer.
pub struct Converter {
    device: wgpu::Device,
    pipeline: wgpu::ComputePipeline,
    source_layout: wgpu::BindGroupLayout,
    target_layout: wgpu::BindGroupLayout,
    format: Format,
}

pub struct Source(wgpu::BindGroup);

pub struct Target(wgpu::BindGroup);

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
            Format::Nv12 => (0, 1, "convert8"),
            Format::P010 => (2, 3, "convert16"),
        };
        let source_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("convert source"),
            entries: &[entry(
                0,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            )],
        });
        let target_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("convert target"),
            entries: &[
                entry(y_binding, storage(y_format)),
                entry(cbcr_binding, storage(cbcr_format)),
            ],
        });
        let module = device.create_shader_module(wgpu::include_wgsl!("convert.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("convert"),
            bind_group_layouts: &[Some(&source_layout), Some(&target_layout)],
            immediate_size: IMMEDIATE_BYTES as u32,
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
            source_layout,
            target_layout,
            format,
        }
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Texture formats of the Y and CbCr planes: `R8Uint` and `Rg8Uint` for
    /// NV12, `R16Uint` and `Rg16Uint` for P010.
    pub fn plane_formats(format: Format) -> (wgpu::TextureFormat, wgpu::TextureFormat) {
        match format {
            Format::Nv12 => (wgpu::TextureFormat::R8Uint, wgpu::TextureFormat::Rg8Uint),
            Format::P010 => (wgpu::TextureFormat::R16Uint, wgpu::TextureFormat::Rg16Uint),
        }
    }

    pub fn source(&self, view: &wgpu::TextureView) -> Source {
        Source(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("convert source"),
            layout: &self.source_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            }],
        }))
    }

    pub fn target(&self, y: &wgpu::TextureView, cbcr: &wgpu::TextureView) -> Target {
        let (y_binding, cbcr_binding) = match self.format {
            Format::Nv12 => (0, 1),
            Format::P010 => (2, 3),
        };
        Target(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("convert target"),
            layout: &self.target_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: y_binding,
                    resource: wgpu::BindingResource::TextureView(y),
                },
                wgpu::BindGroupEntry {
                    binding: cbcr_binding,
                    resource: wgpu::BindingResource::TextureView(cbcr),
                },
            ],
        }))
    }

    /// Records the conversion of `rect` of `src` into `target`. The Y plane must
    /// be at least `rect.width` by `rect.height` and the CbCr plane half of that,
    /// and `rect` must have even dimensions and lie inside the source.
    pub fn encode(
        &self,
        pass: &mut wgpu::ComputePass<'_>,
        src: &Source,
        target: &Target,
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
        ];
        let mut bytes = [0u8; IMMEDIATE_BYTES];
        for (chunk, value) in bytes.chunks_exact_mut(4).zip(params) {
            chunk.copy_from_slice(&value.to_ne_bytes());
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &src.0, &[]);
        pass.set_bind_group(1, &target.0, &[]);
        pass.set_immediates(0, &bytes);
        pass.dispatch_workgroups(
            (rect.width / 2).div_ceil(16),
            (rect.height / 2).div_ceil(16),
            1,
        );
    }
}
