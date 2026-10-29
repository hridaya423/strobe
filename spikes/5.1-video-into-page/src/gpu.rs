use std::collections::HashMap;

use anyhow::Result;
use objc2_io_surface::IOSurfaceRef;
use vrf::gpu::Gpu;
use wgpu::util::DeviceExt as _;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Params {
    pub width: u32,
    pub height: u32,
    pub kr: f32,
    pub kb: f32,
    pub limited: u32,
    pub layout: u32,
    pub chroma_mode: u32,
    pub pad: u32,
}

#[derive(Clone, Copy, Debug)]
pub enum Matrix {
    Bt709,
    Bt601,
}

impl Matrix {
    pub fn coefficients(self) -> (f32, f32) {
        match self {
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt601 => (0.299, 0.114),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bgra,
    Rgba,
    Bmp,
    Composite,
}

pub struct Converter {
    pub gpu: Gpu,
    layout: wgpu::BindGroupLayout,
    convert: wgpu::ComputePipeline,
    convert_bmp: wgpu::ComputePipeline,
    composite: wgpu::ComputePipeline,
    pub output: wgpu::Buffer,
    pub readback: wgpu::Buffer,
    video: HashMap<usize, (wgpu::TextureView, wgpu::TextureView)>,
    pages: HashMap<u32, wgpu::TextureView>,
    empty: wgpu::TextureView,
    pub size: u64,
}

impl Converter {
    pub fn new(gpu: Gpu, width: u32, height: u32) -> Self {
        let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let texture = |sample_type| wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let layout = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                entry(0, wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                }),
                entry(1, texture(wgpu::TextureSampleType::Uint)),
                entry(2, texture(wgpu::TextureSampleType::Uint)),
                entry(3, wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                }),
                entry(4, texture(wgpu::TextureSampleType::Float { filterable: false })),
            ],
        });
        let pipeline_layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry_point| {
            gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let convert = pipeline("convert");
        let convert_bmp = pipeline("convert_bmp");
        let composite = pipeline("composite");
        let size = u64::from(width) * u64::from(height) * 4;
        let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let empty = gpu
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        Converter {
            gpu,
            layout,
            convert,
            convert_bmp,
            composite,
            output,
            readback,
            video: HashMap::new(),
            pages: HashMap::new(),
            empty,
            size,
        }
    }

    pub fn video_views(&mut self, key: usize, surface: &IOSurfaceRef) -> Result<()> {
        if !self.video.contains_key(&key) {
            let luma = self.gpu.import_iosurface(surface, 0, wgpu::TextureFormat::R8Uint)?;
            let chroma = self.gpu.import_iosurface(surface, 1, wgpu::TextureFormat::Rg8Uint)?;
            self.video.insert(
                key,
                (luma.create_view(&Default::default()), chroma.create_view(&Default::default())),
            );
        }
        Ok(())
    }

    pub fn page_view(&mut self, surface: &IOSurfaceRef) -> Result<()> {
        let id = surface.id();
        if !self.pages.contains_key(&id) {
            let texture = self.gpu.import_iosurface(surface, 0, wgpu::TextureFormat::Bgra8Unorm)?;
            self.pages.insert(id, texture.create_view(&Default::default()));
        }
        Ok(())
    }

    /// Runs the shader for `kind` on video frame `key` (and the page surface for `Composite`),
    /// and waits for it. The result is in `output`.
    pub fn run(
        &self,
        kind: Kind,
        key: usize,
        page: Option<u32>,
        params: Params,
    ) -> Result<()> {
        let (luma, chroma) = &self.video[&key];
        let page = page.map_or(&self.empty, |id| &self.pages[&id]);
        let uniform = self
            .gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(luma) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(chroma) },
                wgpu::BindGroupEntry { binding: 3, resource: self.output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(page) },
            ],
        });
        let (pipeline, x_div) = match kind {
            Kind::Bgra | Kind::Rgba => (&self.convert, 1),
            Kind::Bmp => (&self.convert_bmp, 4),
            Kind::Composite => (&self.composite, 1),
        };
        let mut commands = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = commands.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(params.width.div_ceil(16 * x_div), params.height.div_ceil(16), 1);
        }
        let submission = self.gpu.queue.submit([commands.finish()]);
        self.gpu.device.poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })?;
        Ok(())
    }

    /// Copies `output` to the mappable buffer and maps it. Call [`Converter::unmap`] when done.
    pub fn map(&self, bytes: u64) -> Result<*const u8> {
        let mut commands = self.gpu.device.create_command_encoder(&Default::default());
        commands.copy_buffer_to_buffer(&self.output, 0, &self.readback, 0, bytes.next_multiple_of(4));
        let submission = self.gpu.queue.submit([commands.finish()]);
        self.readback.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        self.gpu.device.poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })?;
        let ptr = self.readback.get_mapped_range(..)?.as_ptr();
        Ok(ptr)
    }

    pub fn unmap(&self) {
        self.readback.unmap();
    }
}
