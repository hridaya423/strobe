use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use vrf::colour::{Chroma, Format, Rect};
use vrf::gpu::Gpu;
use vrf::gpu::convert::Converter;
use vrf::gpu::metal::PoolTargets;
use vrf::mux::{Container, Muxer};
use vrf::sched::{self, Consumer, Submission};
use vrf::sink::Sink;
use vrf_encode::turbo::{TurboEncoder, TurboOptions};
use vrf_encode::videotoolbox::VideoToolboxEncoder;
use vrf_encode::videotoolbox::pool::PoolBuffer;
use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Rational};

const FRAME_RATE: Rational = Rational::new(30, 1);
const SOURCES: usize = 4;
const WINDOW: usize = 32;

struct Args {
    frames: u64,
    codec: Codec,
    out: Option<PathBuf>,
    fresh_source: bool,
    width: u32,
    height: u32,
    turbo: Option<TurboOptions>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        frames: 600,
        codec: Codec::H264,
        out: None,
        fresh_source: false,
        width: 1920,
        height: 1080,
        turbo: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--frames" => args.frames = it.next().context("--frames needs a value")?.parse()?,
            "--codec" => {
                args.codec = match it.next().context("--codec needs a value")?.as_str() {
                    "h264" => Codec::H264,
                    "hevc" => Codec::Hevc,
                    "prores" => Codec::ProRes422,
                    other => bail!("unknown codec {other}"),
                }
            }
            "--out" => args.out = Some(it.next().context("--out needs a value")?.into()),
            "--fresh-source" => args.fresh_source = true,
            "--size" => {
                (args.width, args.height) =
                    match it.next().context("--size needs a value")?.as_str() {
                        "1080p" => (1920, 1080),
                        "4k" => (3840, 2160),
                        other => bail!("unknown size {other}"),
                    }
            }
            "--turbo" => args.turbo = Some(TurboOptions::for_frame(args.width, args.height)),
            "--chunk" => {
                let chunk = it.next().context("--chunk needs a value")?.parse()?;
                args.turbo
                    .as_mut()
                    .context("--chunk follows --turbo")?
                    .chunk_frames = chunk;
            }
            "--x264-threads" => {
                let threads = it.next().context("--x264-threads needs a value")?.parse()?;
                args.turbo
                    .as_mut()
                    .context("--x264-threads follows --turbo")?
                    .x264_threads = threads;
            }
            "--no-cpu-lane" => {
                args.turbo
                    .as_mut()
                    .context("--no-cpu-lane follows --turbo")?
                    .cpu_lane = false
            }
            "--vt-sessions" => {
                let sessions = it.next().context("--vt-sessions needs a value")?.parse()?;
                args.turbo
                    .as_mut()
                    .context("--vt-sessions follows --turbo")?
                    .vt_sessions = Some(sessions);
            }
            other => bail!("unknown flag {other}"),
        }
    }
    Ok(args)
}

fn cpu_seconds() -> (f64, f64) {
    let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    (seconds(usage.ru_utime), seconds(usage.ru_stime))
}

#[derive(Clone)]
struct Finished {
    buffer: PoolBuffer,
    submission: wgpu::SubmissionIndex,
}

#[derive(Default)]
struct SubmitTimes {
    gpu_wait: Duration,
    encode: Duration,
    mux: Duration,
}

fn submit_loop(
    encoder: &mut impl Encoder,
    mut consumer: Consumer<Finished>,
    device: wgpu::Device,
    mut sink: Sink,
) -> Result<(Muxer, SubmitTimes)> {
    let mut times = SubmitTimes::default();
    while let Some((index, frame)) = consumer.recv()? {
        let started = Instant::now();
        device.poll(wgpu::PollType::Wait {
            submission_index: Some(frame.submission),
            timeout: None,
        })?;
        times.gpu_wait += started.elapsed();

        let started = Instant::now();
        let pts = Rational::new(index as i64 * FRAME_RATE.den, FRAME_RATE.num);
        encoder.submit(EncoderFrame::PixelBuffer(frame.buffer), pts)?;
        times.encode += started.elapsed();

        let started = Instant::now();
        let packets = encoder.drain()?;
        sink.write(encoder, packets)?;
        times.mux += started.elapsed();
    }
    let started = Instant::now();
    let packets = encoder.flush()?;
    sink.write(encoder, packets)?;
    times.mux += started.elapsed();
    Ok((sink.into_muxer(), times))
}

pub fn main() -> Result<()> {
    let args = parse_args()?;
    let (width, height) = (args.width, args.height);
    let gpu = Gpu::new()?;
    let converter = Converter::new(&gpu, Format::Nv12);
    let mut config = EncoderConfig::new(args.codec, width, height, FRAME_RATE);
    config.keyframe_interval = 240;

    let render = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pattern"),
            layout: Some(
                &gpu.device
                    .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: None,
                        bind_group_layouts: &[Some(&gpu.device.create_bind_group_layout(
                            &wgpu::BindGroupLayoutDescriptor {
                                label: None,
                                entries: &[wgpu::BindGroupLayoutEntry {
                                    binding: 0,
                                    visibility: wgpu::ShaderStages::COMPUTE,
                                    ty: wgpu::BindingType::StorageTexture {
                                        access: wgpu::StorageTextureAccess::WriteOnly,
                                        format: wgpu::TextureFormat::Bgra8Unorm,
                                        view_dimension: wgpu::TextureViewDimension::D2,
                                    },
                                    count: None,
                                }],
                            },
                        ))],
                        immediate_size: 4,
                    }),
            ),
            module: &gpu
                .device
                .create_shader_module(wgpu::include_wgsl!("spine_bench.wgsl")),
            entry_point: Some("render"),
            compilation_options: Default::default(),
            cache: None,
        });
    let textures: Vec<_> = (0..SOURCES)
        .map(|_| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("source"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        })
        .collect();
    let views: Vec<_> = textures
        .iter()
        .map(|t| t.create_view(&Default::default()))
        .collect();
    let render_groups: Vec<_> = views
        .iter()
        .map(|view| {
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &render.get_bind_group_layout(0),
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                }],
            })
        })
        .collect();
    let sources: Vec<_> = views.iter().map(|view| converter.source(view)).collect();
    let mut targets = PoolTargets::default();
    let rect = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };

    let out = args
        .out
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("spine_bench.mp4"));
    let container = match args.codec {
        Codec::ProRes422 => Container::Mov,
        _ => Container::Mp4,
    };
    let muxer = Muxer::create(&out, container, FRAME_RATE.num as u32)?;
    let sink = Sink::new(muxer, FRAME_RATE.den as u32);
    let (producer, consumer) = sched::ordered::<Finished>(WINDOW);
    let submit_device = gpu.device.clone();
    let (pool, submit) = match args.turbo {
        Some(options) => {
            let mut encoder = TurboEncoder::with_options(config, options)?;
            let pool = encoder.pool().clone();
            let submit = std::thread::spawn(move || {
                let (muxer, times) = submit_loop(&mut encoder, consumer, submit_device, sink)?;
                Ok::<_, anyhow::Error>((muxer, times, format!("{:?}", encoder.lane_stats())))
            });
            (pool, submit)
        }
        None => {
            let mut encoder = VideoToolboxEncoder::new(config)?;
            let pool = encoder.pool().clone();
            let submit = std::thread::spawn(move || {
                let (muxer, times) = submit_loop(&mut encoder, consumer, submit_device, sink)?;
                Ok::<_, anyhow::Error>((muxer, times, String::new()))
            });
            (pool, submit)
        }
    };

    let (mut acquire, mut record, mut reorder) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    let (user0, system0) = cpu_seconds();
    let start = Instant::now();
    for index in 0..args.frames {
        let started = Instant::now();
        let buffer = pool.acquire()?;
        acquire += started.elapsed();

        let started = Instant::now();
        let slot = index as usize % SOURCES;
        let fresh;
        let source = if args.fresh_source {
            fresh = converter.source(&views[slot]);
            &fresh
        } else {
            &sources[slot]
        };
        let target = targets.get(&gpu, &converter, &buffer)?;
        let mut commands = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = commands.begin_compute_pass(&Default::default());
            pass.set_pipeline(&render);
            pass.set_bind_group(0, &render_groups[slot], &[]);
            pass.set_immediates(0, &(index as u32).to_ne_bytes());
            pass.dispatch_workgroups(width / 8, height / 8, 1);
        }
        {
            let mut pass = commands.begin_compute_pass(&Default::default());
            converter.encode(&mut pass, source, target, rect, Chroma::Swscale);
        }
        let submission = gpu.queue.submit([commands.finish()]);
        record += started.elapsed();

        let started = Instant::now();
        producer.submit(index, Submission::Frame(Finished { buffer, submission }))?;
        reorder += started.elapsed();
    }
    drop(producer);
    let (muxer, times, lanes) = submit.join().expect("submit thread panicked")?;
    let elapsed = start.elapsed();
    let (user1, system1) = cpu_seconds();

    let started = Instant::now();
    muxer.finish()?;
    let faststart = started.elapsed();
    let size = std::fs::metadata(&out)?.len();
    if args.out.is_none() {
        std::fs::remove_file(&out)?;
    }

    let frames = args.frames as f64;
    let per_frame = |d: Duration| d.as_secs_f64() * 1000.0 / frames;
    let cpu = (user1 - user0) + (system1 - system0);
    println!(
        "spine_bench {:?}{} {width}x{height}: {} frames in {:.3} s = {:.1} fps",
        args.codec,
        if args.turbo.is_some() { " turbo" } else { "" },
        args.frames,
        elapsed.as_secs_f64(),
        frames / elapsed.as_secs_f64()
    );
    println!(
        "cpu: {:.3} ms/frame (user {:.3}, system {:.3})",
        cpu * 1000.0 / frames,
        (user1 - user0) * 1000.0 / frames,
        (system1 - system0) * 1000.0 / frames
    );
    println!(
        "ms/frame: producer blocked on pool {:.3}, record+submit {:.3}, blocked on reorder window {:.3}; submit thread gpu wait {:.3}, VT submit {:.3}, drain+mux {:.3}",
        per_frame(acquire),
        per_frame(record),
        per_frame(reorder),
        per_frame(times.gpu_wait),
        per_frame(times.encode),
        per_frame(times.mux)
    );
    if !lanes.is_empty() {
        println!("lanes: {lanes}");
    }
    println!(
        "faststart: {:.1} ms for {:.1} MB",
        faststart.as_secs_f64() * 1000.0,
        size as f64 / 1e6
    );
    Ok(())
}
