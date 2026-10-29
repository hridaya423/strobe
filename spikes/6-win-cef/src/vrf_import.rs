use std::time::Instant;

use vrf::colour::{self, Chroma, Format, Rect};
use vrf::gpu::Gpu;
use vrf::gpu::convert::{Converter, Target};
use vrf::gpu::d3d12::SharedHandle;

pub struct Phases {
    pub open_us: f64,
    pub copy_us: f64,
    pub convert_us: f64,
    pub release_us: f64,
}

pub struct Checked {
    pub phases: Phases,
    pub centre: [u8; 4],
    pub planes_ok: bool,
}

struct Planes {
    y: wgpu::Texture,
    cbcr: wgpu::Texture,
    target: Target,
    owned: wgpu::Texture,
    owned_view: wgpu::TextureView,
}

pub struct VrfImport {
    gpu: Gpu,
    converter: Converter,
    direct: bool,
    planes: std::cell::RefCell<Option<Planes>>,
}

fn us(a: Instant, b: Instant) -> f64 {
    (b - a).as_secs_f64() * 1e6
}

impl VrfImport {
    pub fn new(direct: bool) -> Self {
        let gpu = Gpu::new().unwrap();
        println!(
            "vrf adapter: {} luid {} direct={direct}",
            gpu.adapter.get_info().name,
            gpu.luid()
        );
        let converter = Converter::new(&gpu, Format::Nv12);
        Self {
            gpu,
            converter,
            direct,
            planes: Default::default(),
        }
    }

    fn texture(
        &self,
        format: wgpu::TextureFormat,
        w: u32,
        h: u32,
        usage: wgpu::TextureUsages,
    ) -> wgpu::Texture {
        self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    }

    fn wait(&self) {
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
    }

    pub fn process(
        &self,
        handle: SharedHandle,
        delay: std::time::Duration,
    ) -> Result<Checked, String> {
        std::thread::sleep(delay);
        let usage = if self.direct {
            wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING
        } else {
            wgpu::TextureUsages::COPY_SRC
        };
        let t0 = Instant::now();
        let imported = self
            .gpu
            .import_shared_handle(&handle, usage)
            .map_err(|e| format!("{e:#}"))?;
        let t1 = Instant::now();
        let (w, h) = (imported.width(), imported.height());
        let mut planes = self.planes.borrow_mut();
        let planes = planes.get_or_insert_with(|| {
            let (yf, cf) = Converter::plane_formats(Format::Nv12);
            let u = wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC;
            let y = self.texture(yf, w, h, u);
            let cbcr = self.texture(cf, w / 2, h / 2, u);
            let target = self.converter.target(
                &y.create_view(&Default::default()),
                &cbcr.create_view(&Default::default()),
            );
            let owned = self.texture(
                imported.format(),
                w,
                h,
                wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            let owned_view = owned.create_view(&Default::default());
            Planes {
                y,
                cbcr,
                target,
                owned,
                owned_view,
            }
        });
        if !self.direct {
            let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_texture(
                imported.as_image_copy(),
                planes.owned.as_image_copy(),
                imported.size(),
            );
            self.gpu.queue.submit([encoder.finish()]);
            self.wait();
        }
        let t2 = Instant::now();
        let source = if self.direct {
            self.converter
                .source(&imported.create_view(&Default::default()))
        } else {
            self.converter.source(&planes.owned_view)
        };
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            self.converter.encode(
                &mut pass,
                &source,
                &planes.target,
                Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                },
                Chroma::Swscale,
            );
        }
        self.gpu.queue.submit([encoder.finish()]);
        self.wait();
        let t3 = Instant::now();

        let centre = self.centre(&imported);
        let y = self
            .gpu
            .read_texture(&planes.y)
            .map_err(|e| e.to_string())?;
        let cbcr = self
            .gpu
            .read_texture(&planes.cbcr)
            .map_err(|e| e.to_string())?;
        let block = [centre[2], centre[1], centre[0], 255].repeat(4);
        let want = colour::convert(
            &block,
            2,
            Rect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            Chroma::Swscale,
            Format::Nv12,
        );
        let got_y = y[(h / 2 * w + w / 2) as usize];
        let ci = ((h / 4 * (w / 2) + w / 4) * 2) as usize;
        let planes_ok = got_y == want[0] && cbcr[ci] == want[4] && cbcr[ci + 1] == want[5];

        let t4 = Instant::now();
        drop(source);
        drop(imported);
        self.wait();
        let t5 = Instant::now();
        let _ = t4;
        Ok(Checked {
            phases: Phases {
                open_us: us(t0, t1),
                copy_us: us(t1, t2),
                convert_us: us(t2, t3),
                release_us: us(t4, t5),
            },
            centre,
            planes_ok,
        })
    }

    fn centre(&self, texture: &wgpu::Texture) -> [u8; 4] {
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: texture.width() / 2,
                    y: texture.height() / 2,
                    z: 0,
                },
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
        self.gpu.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |r| r.unwrap());
        self.wait();
        let m = buffer.get_mapped_range(..).unwrap();
        [m[0], m[1], m[2], m[3]]
    }
}
