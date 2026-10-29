use crate::{convert::Nv12Converter, harness::Harness, page_url};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const LOAD_SHADER: &str = "
const ITERATIONS: u32 = %ITER%u;
@group(0) @binding(0) var<storage, read_write> data: array<f32>;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    var x = f32(id.x) * 0.001;
    for (var i = 0u; i < ITERATIONS; i++) {
        x = sin(x) * 1.0001 + cos(x * 1.3);
    }
    data[id.x] = x;
}";

pub struct GpuLoader {
    stop: Arc<AtomicBool>,
    submits: Arc<AtomicU64>,
    started: Instant,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl GpuLoader {
    pub fn start(iterations: u32) -> GpuLoader {
        let stop = Arc::new(AtomicBool::new(false));
        let submits = Arc::new(AtomicU64::new(0));
        let thread = {
            let (stop, submits) = (stop.clone(), submits.clone());
            std::thread::spawn(move || {
                let gpu = crate::gpu::Gpu::new();
                let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: None,
                    source: wgpu::ShaderSource::Wgsl(LOAD_SHADER.replace("%ITER%", &iterations.to_string()).into()),
                });
                let pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: None,
                    layout: None,
                    module: &module,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                });
                let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 4 << 20,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                });
                let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
                });
                while !stop.load(Ordering::Relaxed) {
                    let mut encoder = gpu.device.create_command_encoder(&Default::default());
                    {
                        let mut pass = encoder.begin_compute_pass(&Default::default());
                        pass.set_pipeline(&pipeline);
                        pass.set_bind_group(0, &bind_group, &[]);
                        pass.dispatch_workgroups(4096, 1, 1);
                    }
                    gpu.queue.submit([encoder.finish()]);
                    gpu.wait();
                    submits.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        GpuLoader { stop, submits, started: Instant::now(), thread: Some(thread) }
    }

    pub fn finish(mut self) -> f64 {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
        self.started.elapsed().as_secs_f64() * 1000.0 / self.submits.load(Ordering::Relaxed).max(1) as f64
    }
}

unsafe extern "C" {
    fn IOSurfaceGetID(surface: *const std::ffi::c_void) -> u32;
}

#[derive(Clone, Copy)]
struct Sample {
    path: usize,
    import: f64,
    submit: f64,
    wait: f64,
    total: f64,
}

fn summarise(label: &str, samples: &[Sample]) {
    let column = |f: fn(&Sample) -> f64| {
        let mut v: Vec<f64> = samples.iter().map(f).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (v[v.len() / 2], v[(v.len() - 1) * 95 / 100], *v.last().unwrap())
    };
    let (t50, t95, tmax) = column(|s| s.total);
    let (i50, _, _) = column(|s| s.import);
    let (s50, _, _) = column(|s| s.submit);
    let (w50, _, _) = column(|s| s.wait);
    println!(
        "  {label}: n={} total_ms p50={t50:.3} p95={t95:.3} max={tmax:.3} | p50 parts: import={i50:.3} encode+submit={s50:.3} gpu_wait={w50:.3}",
        samples.len()
    );
}

pub fn run(options: &HashMap<String, String>) {
    let n: usize = options.get("n").map_or(300, |v| v.parse().unwrap());
    let size = options.get("size").map_or("1080", String::as_str);
    let (width, height) = if size == "4k" { (3840u32, 2160u32) } else { (1920, 1080) };
    let loader = match options.get("load").map(String::as_str) {
        Some("gpu") => Some(GpuLoader::start(4000)),
        Some("gpu-light") => Some(GpuLoader::start(300)),
        _ => None,
    };

    let h = Harness::open(&page_url("s13.html"), width as i32, height as i32);
    let gpu = &h.shared.gpu;
    let converter = Nv12Converter::new(gpu);
    let texture = |w, h, format, usage| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let y_plane = texture(width, height, wgpu::TextureFormat::R8Unorm, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let c_plane = texture(width / 2, height / 2, wgpu::TextureFormat::Rg8Unorm, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let owned = texture(
        width,
        height,
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
    );

    let samples: Rc<RefCell<Vec<Sample>>> = Rc::default();
    let frame_number = Rc::new(std::cell::Cell::new(0u64));
    let cache: Rc<RefCell<HashMap<u32, wgpu::Texture>>> = Rc::default();
    let distinct_surfaces = Rc::new(std::cell::Cell::new(0usize));
    {
        let (samples, shared, frame_number, cache, distinct) = (samples.clone(), h.shared.clone(), frame_number.clone(), cache.clone(), distinct_surfaces.clone());
        let converter = Rc::new(converter);
        h.set_paint(move |info| {
            let t0 = Instant::now();
            let gpu = &shared.gpu;
            let path = (frame_number.get() % 4) as usize;
            frame_number.set(frame_number.get() + 1);
            let origin = (info.extra.visible_rect.x as u32, info.extra.visible_rect.y as u32);
            let (t1, t2, t3);
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            match path {
                0 | 1 => {
                    let frame = gpu.import_frame(info);
                    t1 = Instant::now();
                    if path == 1 {
                        converter.encode(gpu, &mut encoder, &frame.texture, origin, &y_plane, &c_plane);
                    } else {
                        encoder.copy_texture_to_texture(
                            wgpu::TexelCopyTextureInfo { texture: &frame.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                            wgpu::TexelCopyTextureInfo { texture: &owned, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                            wgpu::Extent3d { width: frame.width, height: frame.height, depth_or_array_layers: 1 },
                        );
                        converter.encode(gpu, &mut encoder, &owned, origin, &y_plane, &c_plane);
                    }
                    gpu.queue.submit([encoder.finish()]);
                }
                2 => {
                    let id = unsafe { IOSurfaceGetID(info.shared_texture_io_surface) };
                    let mut cache = cache.borrow_mut();
                    if !cache.contains_key(&id) {
                        cache.insert(id, gpu.import_frame(info).texture);
                        distinct.set(cache.len());
                    }
                    t1 = Instant::now();
                    converter.encode(gpu, &mut encoder, &cache[&id], origin, &y_plane, &c_plane);
                    gpu.queue.submit([encoder.finish()]);
                }
                _ => {
                    t1 = Instant::now();
                    gpu.queue.submit([encoder.finish()]);
                }
            }
            t2 = Instant::now();
            gpu.wait();
            t3 = Instant::now();
            let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1000.0;
            samples.borrow_mut().push(Sample { path, import: ms(t0, t1), submit: ms(t1, t2), wait: ms(t2, t3), total: ms(t0, t3) });
        });
    }

    for i in 0..10 {
        h.eval(&format!("seek({i})"));
        h.begin_frame();
        h.pump_for(Duration::from_millis(100));
    }
    samples.borrow_mut().clear();
    frame_number.set(0);
    let mut dropped = 0;
    for i in 0..n {
        h.eval(&format!("seek({})", i + 100));
        let before = samples.borrow().len();
        h.begin_frame();
        if !h.pump_until(Duration::from_millis(500), || samples.borrow().len() > before) {
            dropped += 1;
        }
        h.pump_for(Duration::from_millis(5));
    }
    let loader_ms = loader.map(GpuLoader::finish);
    let samples = samples.borrow();
    println!(
        "spike=1.3 size={width}x{height} gpu_load={} frames={n} dropped={dropped} raster={} load={}",
        match loader_ms {
            Some(ms) => format!("synthetic compute loop ({ms:.1} ms per dispatch)"),
            None => "idle".into(),
        },
        crate::harness::raster_mode(),
        crate::load_average()
    );
    let by_path = |k: usize| samples.iter().copied().filter(|s| s.path == k).collect::<Vec<_>>();
    summarise("A import + GPU copy + convert + wait", &by_path(0));
    summarise("B import + fused convert (no copy) + wait", &by_path(1));
    summarise("C cached import (by IOSurface ID) + fused convert + wait", &by_path(2));
    summarise("D floor: empty submit + wait, no import", &by_path(3));
    println!("  distinct IOSurface IDs seen: {}", distinct_surfaces.get());
    h.close();
}
