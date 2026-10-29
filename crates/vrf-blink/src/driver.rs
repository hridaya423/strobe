use std::collections::HashMap;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use vrf::colour::{Chroma, Format};
use vrf::gpu::Gpu;
use vrf::gpu::convert::{Converter, Source};
use vrf::gpu::metal::PoolTargets;
use vrf::mux::{Container, Muxer, Sample, StreamConfig};
use vrf::sched::{self, Consumer, Submission};
use vrf_encode::videotoolbox::VideoToolboxEncoder;
use vrf_encode::videotoolbox::pool::{PixelPool, PoolBuffer};
use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Rational};

use crate::{Paint, View};

const WINDOW: usize = 32;
const MAX_IMPORTS: usize = 16;

#[derive(Debug, Clone)]
pub struct Config {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub codec: Codec,
    pub frames: u64,
    pub watchdog: Duration,
    pub import_delay: Duration,
}

impl Config {
    pub fn new(width: u32, height: u32, frame_rate: Rational, codec: Codec, frames: u64) -> Self {
        Config {
            width,
            height,
            frame_rate,
            codec,
            frames,
            watchdog: Duration::from_millis(100),
            import_delay: Duration::ZERO,
        }
    }
}

#[derive(Debug, Default)]
pub struct Stats {
    pub elapsed: Duration,
    pub repeats: u64,
    pub retries: u64,
    pub seek: Vec<Duration>,
    pub begin_to_paint: Vec<Duration>,
    pub convert: Vec<Duration>,
    pub encode: Vec<Duration>,
}

#[derive(Debug, Clone, Copy)]
pub struct Summary {
    pub mean: Duration,
    pub p50: Duration,
    pub p99: Duration,
}

impl Summary {
    pub fn of(samples: &[Duration]) -> Summary {
        let mut sorted = samples.to_vec();
        sorted.sort();
        let at = |p: f64| {
            sorted
                .get(((sorted.len().saturating_sub(1)) as f64 * p) as usize)
                .copied()
                .unwrap_or_default()
        };
        Summary {
            mean: sorted.iter().sum::<Duration>() / sorted.len().max(1) as u32,
            p50: at(0.5),
            p99: at(0.99),
        }
    }
}

struct Converting {
    gpu: Gpu,
    converter: Converter,
    targets: PoolTargets,
    imports: HashMap<u32, Source>,
    pool: PixelPool,
}

impl Converting {
    fn convert(&mut self, paint: &Paint) -> Result<PoolBuffer> {
        let id = paint.surface.id();
        if !self.imports.contains_key(&id) {
            if self.imports.len() >= MAX_IMPORTS {
                self.imports.clear();
            }
            let texture = self.gpu.import_iosurface(&paint.surface, 0, paint.format)?;
            let source = self
                .converter
                .source(&texture.create_view(&Default::default()));
            self.imports.insert(id, source);
        }
        let buffer = self.pool.acquire()?;
        let target = self.targets.get(&self.gpu, &self.converter, &buffer)?;
        let mut commands = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = commands.begin_compute_pass(&Default::default());
            self.converter.encode(
                &mut pass,
                &self.imports[&id],
                target,
                paint.visible,
                Chroma::Swscale,
            );
        }
        let submission = self.gpu.queue.submit([commands.finish()]);
        self.gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })?;
        Ok(buffer)
    }
}

struct Sink {
    muxer: Muxer,
    started: bool,
    duration: u32,
}

impl Sink {
    fn write(&mut self, encoder: &impl Encoder, packets: Vec<Packet>) -> Result<()> {
        if packets.is_empty() {
            return Ok(());
        }
        let info = encoder.stream_info()?;
        if !self.started {
            self.muxer.start_chunk(StreamConfig::try_from(&info)?)?;
            self.started = true;
        }
        for packet in packets {
            self.muxer
                .write(&Sample::from_packet(packet, info.timebase, self.duration)?)?;
        }
        Ok(())
    }
}

fn submit_loop(
    mut encoder: VideoToolboxEncoder,
    mut consumer: Consumer<PoolBuffer>,
    mut sink: Sink,
    frame_rate: Rational,
) -> Result<(Muxer, Vec<Duration>)> {
    let mut encode = Vec::new();
    while let Some((index, buffer)) = consumer.recv()? {
        let started = Instant::now();
        let pts = Rational::new(index as i64 * frame_rate.den, frame_rate.num);
        encoder.submit(EncoderFrame::PixelBuffer(buffer), pts)?;
        encode.push(started.elapsed());
        let packets = encoder.drain()?;
        sink.write(&encoder, packets)?;
    }
    let packets = encoder.flush()?;
    sink.write(&encoder, packets)?;
    Ok((sink.muxer, encode))
}

#[derive(Default)]
struct Capture {
    last_counter: Option<u64>,
}

impl Capture {
    fn next(
        &mut self,
        view: &View,
        watchdog: Duration,
        stats: &mut Stats,
    ) -> Result<Option<Paint>> {
        for _ in 0..2 {
            let sent = view.begin_frame();
            let Some(paint) = view.recv_paint(watchdog) else {
                if self.last_counter.is_some() {
                    return Ok(None);
                }
                stats.retries += 1;
                view.invalidate();
                continue;
            };
            ensure!(
                paint.at >= sent,
                "a delivery from before this begin frame arrived; the watchdog of {watchdog:?} is too short"
            );
            stats.begin_to_paint.push(paint.at - sent);
            let gap = self
                .last_counter
                .is_some_and(|last| paint.counter != last + 1);
            self.last_counter = Some(paint.counter);
            if !gap {
                return Ok(Some(paint));
            }
            stats.retries += 1;
            view.invalidate();
        }
        bail!("no clean delivery in two attempts")
    }
}

/// Renders `config.frames` frames of the page in `view` to an MP4 at `out`. Frame `i` runs
/// `seek(i)` in the page, sends one begin frame, and converts the delivered IOSurface into the
/// encoder's pixel buffer. A frame with no delivery within `config.watchdog` repeats the previous
/// one.
pub fn render(
    view: &mut View,
    config: &Config,
    seek: impl Fn(u64) -> String,
    out: &Path,
) -> Result<Stats> {
    let gpu = Gpu::new()?;
    let converter = Converter::new(&gpu, Format::Nv12);
    let encoder = VideoToolboxEncoder::new(EncoderConfig::new(
        config.codec,
        config.width,
        config.height,
        config.frame_rate,
    ))?;
    let mut converting = Converting {
        gpu,
        converter,
        targets: PoolTargets::default(),
        imports: HashMap::new(),
        pool: encoder.pool().clone(),
    };
    let (producer, consumer) = sched::ordered::<PoolBuffer>(WINDOW);
    let sink = Sink {
        muxer: Muxer::create(out, Container::Mp4, config.frame_rate.num as u32)?,
        started: false,
        duration: config.frame_rate.den as u32,
    };
    let frame_rate = config.frame_rate;
    let submit = thread::spawn(move || submit_loop(encoder, consumer, sink, frame_rate));

    let mut stats = Stats::default();
    let mut capture = Capture::default();
    let started = Instant::now();
    for index in 0..config.frames {
        let step = Instant::now();
        view.evaluate(&seek(index))
            .with_context(|| format!("seek to frame {index}"))?;
        stats.seek.push(step.elapsed());

        let submission = match capture.next(view, config.watchdog, &mut stats)? {
            Some(paint) => {
                ensure!(
                    (paint.visible.width, paint.visible.height) == (config.width, config.height),
                    "frame {index} is {}x{}, expected {}x{}",
                    paint.visible.width,
                    paint.visible.height,
                    config.width,
                    config.height
                );
                if !config.import_delay.is_zero() {
                    thread::sleep(config.import_delay);
                }
                let step = Instant::now();
                let buffer = converting.convert(&paint)?;
                stats.convert.push(step.elapsed());
                Submission::Frame(buffer)
            }
            None => {
                stats.repeats += 1;
                Submission::Repeat
            }
        };
        producer.submit(index, submission)?;
    }
    drop(producer);
    let (muxer, encode) = submit.join().expect("submit thread panicked")?;
    stats.elapsed = started.elapsed();
    stats.encode = encode;
    muxer.finish()?;
    Ok(stats)
}
