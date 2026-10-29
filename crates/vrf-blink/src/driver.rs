use std::collections::HashMap;
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use vrf::colour::{Chroma, Format};
use vrf::gpu::Gpu;
use vrf::gpu::convert::{Converter, Source};
use vrf::gpu::metal::PoolTargets;
use vrf::mux::{Container, Muxer};
use vrf::sched::{self, Consumer, Producer, Submission};
use vrf::sink::Sink;
use vrf_encode::videotoolbox::VideoToolboxEncoder;
use vrf_encode::videotoolbox::pool::{PixelPool, PoolBuffer};
use vrf_encode::{Codec, Encoder, EncoderConfig, EncoderFrame, Rational};

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
}

impl Config {
    pub fn new(width: u32, height: u32, frame_rate: Rational, codec: Codec, frames: u64) -> Self {
        Config {
            width,
            height,
            frame_rate,
            codec,
            frames,
            watchdog: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Default)]
pub struct Stats {
    pub elapsed: Duration,
    pub finish: Duration,
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
    Ok((sink.into_muxer(), encode))
}

/// Takes one begin frame at a time from a view and checks the delivery against CEF's capture
/// counter. Replacing the view it captures from is allowed: the counter restarts with the new
/// view.
#[derive(Default)]
pub struct Capture {
    view: Option<u64>,
    last_counter: Option<u64>,
    pub retries: u64,
    pub begin_to_paint: Vec<Duration>,
}

impl Capture {
    /// Sends begin frames until a delivery arrives, and returns `None` when none arrives within
    /// `watchdog` of a begin frame on a view that has already delivered once, which means the
    /// page did not change.
    pub fn next(&mut self, view: &View, watchdog: Duration) -> Result<Option<Paint>> {
        if self.view != Some(view.serial()) {
            self.view = Some(view.serial());
            self.last_counter = None;
        }
        for _ in 0..2 {
            let sent = view.begin_frame();
            let Some(paint) = view.recv_paint(watchdog) else {
                if self.last_counter.is_some() {
                    return Ok(None);
                }
                self.retries += 1;
                view.invalidate();
                continue;
            };
            ensure!(
                paint.at >= sent,
                "a delivery from before this begin frame arrived; the watchdog of {watchdog:?} is too short"
            );
            self.begin_to_paint.push(paint.at - sent);
            let gap = self
                .last_counter
                .is_some_and(|last| paint.counter != last + 1);
            self.last_counter = Some(paint.counter);
            if !gap {
                return Ok(Some(paint));
            }
            self.retries += 1;
            view.invalidate();
        }
        bail!("no clean delivery in two attempts")
    }
}

/// Converts painted surfaces into encoder buffers and feeds them, in frame order, to an encoder
/// and muxer on a submit thread.
pub struct Pipeline {
    converting: Converting,
    producer: Producer<PoolBuffer>,
    submit: JoinHandle<Result<(Muxer, Vec<Duration>)>>,
    width: u32,
    height: u32,
    pub repeats: u64,
    pub convert: Vec<Duration>,
}

impl Pipeline {
    pub fn new(config: &Config, out: &Path) -> Result<Pipeline> {
        let gpu = Gpu::new()?;
        let converter = Converter::new(&gpu, Format::Nv12);
        let encoder = VideoToolboxEncoder::new(EncoderConfig::new(
            config.codec,
            config.width,
            config.height,
            config.frame_rate,
        ))?;
        let converting = Converting {
            gpu,
            converter,
            targets: PoolTargets::default(),
            imports: HashMap::new(),
            pool: encoder.pool().clone(),
        };
        let (producer, consumer) = sched::ordered::<PoolBuffer>(WINDOW);
        let container = match config.codec {
            Codec::ProRes422 => Container::Mov,
            Codec::H264 | Codec::Hevc => Container::Mp4,
        };
        let sink = Sink::new(
            Muxer::create(out, container, config.frame_rate.num as u32)?,
            config.frame_rate.den as u32,
        );
        let frame_rate = config.frame_rate;
        let submit = thread::spawn(move || submit_loop(encoder, consumer, sink, frame_rate));
        Ok(Pipeline {
            converting,
            producer,
            submit,
            width: config.width,
            height: config.height,
            repeats: 0,
            convert: Vec::new(),
        })
    }

    /// Imports `paint`, converts it, and queues it as frame `index`.
    pub fn frame(&mut self, index: u64, paint: &Paint) -> Result<()> {
        ensure!(
            (paint.visible.width, paint.visible.height) == (self.width, self.height),
            "frame {index} is {}x{}, expected {}x{}",
            paint.visible.width,
            paint.visible.height,
            self.width,
            self.height
        );
        let step = Instant::now();
        let buffer = self.converting.convert(paint)?;
        self.convert.push(step.elapsed());
        self.producer.submit(index, Submission::Frame(buffer))
    }

    /// Queues frame `index` as a copy of the one before it.
    pub fn repeat(&mut self, index: u64) -> Result<()> {
        self.repeats += 1;
        self.producer.submit(index, Submission::Repeat)
    }

    /// Waits for every queued frame to be encoded and returns the muxer, still open, and the
    /// time each encoder submit took.
    pub fn finish(self) -> Result<(Muxer, Vec<Duration>)> {
        drop(self.producer);
        self.submit.join().expect("submit thread panicked")
    }
}

/// Renders `config.frames` frames of the page in `view` to a file at `out`. Frame `i` runs
/// `seek(view, i)`, which must leave the page showing that frame and may replace the view, sends
/// one begin frame, and converts the delivered IOSurface into the encoder's pixel buffer. A frame
/// with no delivery within `config.watchdog` repeats the previous one.
pub fn render(
    view: &mut View,
    config: &Config,
    mut seek: impl FnMut(&mut View, u64) -> Result<()>,
    out: &Path,
) -> Result<Stats> {
    let mut pipeline = Pipeline::new(config, out)?;
    let mut stats = Stats::default();
    let mut capture = Capture::default();
    let started = Instant::now();
    for index in 0..config.frames {
        let step = Instant::now();
        seek(view, index).with_context(|| format!("seek to frame {index}"))?;
        stats.seek.push(step.elapsed());

        match capture.next(view, config.watchdog)? {
            Some(paint) => pipeline.frame(index, &paint)?,
            None => pipeline.repeat(index)?,
        }
    }
    stats.repeats = pipeline.repeats;
    stats.convert = std::mem::take(&mut pipeline.convert);
    let (muxer, encode) = pipeline.finish()?;
    stats.elapsed = started.elapsed();
    stats.encode = encode;
    view.check_project()?;
    let step = Instant::now();
    muxer.finish()?;
    stats.finish = step.elapsed();
    stats.retries = capture.retries;
    stats.begin_to_paint = capture.begin_to_paint;
    Ok(stats)
}
