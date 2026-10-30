use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use vrf::colour::{Chroma, Format};
use vrf::gpu::Gpu;
use vrf::gpu::convert::{Converter, Source};
use vrf::gpu::metal::PoolTargets;
use vrf::mux::{Container, Muxer};
use vrf::sched::{self, Consumer, Producer, Submission};
use vrf::sink::Sink;
use vrf_encode::turbo::{TurboEncoder, TurboOptions};
use vrf_encode::videotoolbox::pool::{PixelFormat, PixelPool, PoolBuffer};
use vrf_encode::videotoolbox::{self, VideoToolboxEncoder};
use vrf_encode::x264;
use vrf_encode::{Codec, ColourSpace, Encoder, EncoderConfig, EncoderFrame, Rational};

use crate::skip::Skip;
use crate::{Paint, View};

const WINDOW: usize = 32;
const MAX_IMPORTS: usize = 16;
const POOL_MARGIN: usize = 32;
const MEMORY_BUDGET: u64 = 3 << 29;
const MAX_MEMORY_BUDGET: u64 = 3 << 30;
const MIN_RUN: u64 = 80;
const LEAD_IN_RUNS: u64 = 10;
const MIN_RUN_LEAD_INS: u64 = 4;
const TICKING_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct Config {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub codec: Codec,
    pub frames: u64,
    pub watchdog: Duration,
    pub window: usize,
    pub turbo: bool,
    pub skip: Option<Skip>,
    pub colour: ColourSpace,
}

impl Config {
    fn turbo_options(&self) -> Option<TurboOptions> {
        self.turbo
            .then(|| TurboOptions::for_frame(self.width, self.height))
    }

    pub fn new(width: u32, height: u32, frame_rate: Rational, codec: Codec, frames: u64) -> Self {
        Config {
            width,
            height,
            frame_rate,
            codec,
            frames,
            watchdog: Duration::from_millis(500),
            window: WINDOW,
            turbo: codec == Codec::H264 && x264::available().is_ok(),
            skip: None,
            colour: ColourSpace::BT709,
        }
    }

    fn encoder_config(&self) -> EncoderConfig {
        let mut config = EncoderConfig::new(self.codec, self.width, self.height, self.frame_rate);
        config.colour = self.colour;
        config
    }

    fn chroma(&self) -> Chroma {
        if self.colour == ColourSpace::BT709 {
            Chroma::Swscale
        } else {
            Chroma::Exact(self.colour)
        }
    }
}

#[derive(Debug, Default)]
pub struct Stats {
    pub elapsed: Duration,
    pub pipeline: Duration,
    pub finish: Duration,
    pub repeats: u64,
    pub skipped: u64,
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

const WARM_POOLS: usize = 2;

type PoolKey = (u32, u32, PixelFormat, usize, ColourSpace);

struct PoolSlot {
    key: PoolKey,
    pool: PixelPool,
    targets: PoolTargets,
}

/// The GPU device, the converter and the encoder pixel pools, kept from one render to the next.
/// Creating them costs more than a short render takes.
pub struct Warm {
    gpu: Arc<Gpu>,
    converter: Arc<Converter>,
    pools: Vec<PoolSlot>,
}

// Metal devices and CoreVideo pools are thread-safe; a `Warm` is only ever used by one thread
// at a time.
unsafe impl Send for Warm {}

impl Warm {
    pub fn new() -> Result<Warm> {
        let gpu = Gpu::new()?;
        let converter = Converter::new(&gpu, Format::Nv12);
        Ok(Warm {
            gpu: Arc::new(gpu),
            converter: Arc::new(converter),
            pools: Vec::new(),
        })
    }

    /// Allocates the pixel pool for frames of `width` x `height` if no render has yet.
    pub fn prepare(&mut self, width: u32, height: u32, colour: ColourSpace) -> Result<()> {
        let mut config = Config::new(width, height, Rational::new(30, 1), Codec::H264, 0);
        config.colour = colour;
        let key = pool_key(&config, &config.encoder_config(), config.turbo_options())?;
        self.slot(key).map(|_| ())
    }

    fn slot(&mut self, key: PoolKey) -> Result<&mut PoolSlot> {
        let at = match self.pools.iter().position(|slot| slot.key == key) {
            Some(at) => at,
            None => {
                if self.pools.len() >= WARM_POOLS {
                    self.pools.remove(0);
                }
                let (width, height, format, size, colour) = key;
                self.pools.push(PoolSlot {
                    key,
                    pool: PixelPool::with_colour(
                        width as usize,
                        height as usize,
                        format,
                        size,
                        colour,
                    )?,
                    targets: PoolTargets::default(),
                });
                self.pools.len() - 1
            }
        };
        Ok(&mut self.pools[at])
    }
}

struct Converting {
    gpu: Arc<Gpu>,
    converter: Arc<Converter>,
    targets: PoolTargets,
    imports: HashMap<u32, Source>,
    pool: PixelPool,
    chroma: Chroma,
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
                self.chroma,
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

fn pool_key(
    config: &Config,
    encoder_config: &EncoderConfig,
    turbo: Option<TurboOptions>,
) -> Result<PoolKey> {
    let lane_buffers = match &turbo {
        Some(options) => TurboEncoder::lane_buffers(encoder_config, options)?,
        None => 0,
    };
    Ok((
        config.width,
        config.height,
        videotoolbox::pixel_format(encoder_config),
        config.window + POOL_MARGIN + lane_buffers,
        config.colour,
    ))
}

fn submit_loop(
    mut encoder: impl Encoder,
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
    /// page did not change. A ticking view always delivers, so there a missing delivery within
    /// 30 s is an error.
    pub fn next(&mut self, view: &View, watchdog: Duration) -> Result<Option<Paint>> {
        if self.view != Some(view.serial()) {
            self.view = Some(view.serial());
            self.last_counter = None;
        }
        let wait = if view.ticking() {
            TICKING_TIMEOUT
        } else {
            watchdog
        };
        for _ in 0..2 {
            let sent = view.begin_frame();
            let mut delivery = view.recv_paint(wait);
            while let Some(stale) = delivery.as_ref().filter(|paint| paint.at < sent) {
                self.retries += 1;
                self.last_counter = Some(stale.counter);
                delivery = view.recv_paint(wait);
            }
            let Some(paint) = delivery else {
                ensure!(
                    !view.ticking(),
                    "no delivery within {wait:?} of a begin frame"
                );
                if self.last_counter.is_some() {
                    return Ok(None);
                }
                self.retries += 1;
                view.invalidate();
                continue;
            };
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

/// What a worker thread needs to build its own [`Lane`]. The GPU device is created on the
/// thread that calls [`LaneSpec::open`].
pub struct LaneSpec {
    producer: Producer<PoolBuffer>,
    pool: PixelPool,
    width: u32,
    height: u32,
    chroma: Chroma,
}

impl LaneSpec {
    pub fn open(self) -> Result<Lane> {
        let gpu = Gpu::new()?;
        let converter = Converter::new(&gpu, Format::Nv12);
        Ok(Lane {
            converting: Converting {
                gpu: Arc::new(gpu),
                converter: Arc::new(converter),
                targets: PoolTargets::default(),
                imports: HashMap::new(),
                pool: self.pool,
                chroma: self.chroma,
            },
            producer: self.producer,
            width: self.width,
            height: self.height,
            repeats: 0,
            skipped: 0,
            convert: Vec::new(),
        })
    }
}

/// One producer of a [`Pipeline`]: converts painted surfaces into encoder buffers and queues them
/// by frame index. Each view thread has its own lane.
pub struct Lane {
    converting: Converting,
    producer: Producer<PoolBuffer>,
    width: u32,
    height: u32,
    pub repeats: u64,
    pub skipped: u64,
    pub convert: Vec<Duration>,
}

impl Lane {
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

    /// Queues frame `index` as a copy of the one before it, because the page proved it did not
    /// change and no begin frame was sent.
    pub fn skip(&mut self, index: u64) -> Result<()> {
        self.skipped += 1;
        self.producer.submit(index, Submission::Repeat)
    }

    pub fn spec(&self) -> LaneSpec {
        LaneSpec {
            producer: self.producer.clone(),
            pool: self.converting.pool.clone(),
            width: self.width,
            height: self.height,
            chroma: self.converting.chroma,
        }
    }
}

/// Feeds frames, in frame order, to an encoder and muxer on a submit thread. Frames come in
/// through lanes; the pipeline is itself the first lane, and uses the GPU state of `warm`.
pub struct Pipeline<'w> {
    lane: Lane,
    submit: JoinHandle<Result<(Muxer, Vec<Duration>)>>,
    warm: &'w mut Warm,
    key: PoolKey,
}

impl Deref for Pipeline<'_> {
    type Target = Lane;

    fn deref(&self) -> &Lane {
        &self.lane
    }
}

impl DerefMut for Pipeline<'_> {
    fn deref_mut(&mut self) -> &mut Lane {
        &mut self.lane
    }
}

impl<'w> Pipeline<'w> {
    pub fn new(config: &Config, out: &Path, warm: &'w mut Warm) -> Result<Pipeline<'w>> {
        let encoder_config = config.encoder_config();
        let turbo = config.turbo_options();
        let key = pool_key(config, &encoder_config, turbo)?;
        let gpu = warm.gpu.clone();
        let converter = warm.converter.clone();
        let slot = warm.slot(key)?;
        let pool = slot.pool.clone();
        let targets = std::mem::take(&mut slot.targets);
        let (producer, consumer) = sched::ordered::<PoolBuffer>(config.window);
        let container = match config.codec {
            Codec::ProRes422 => Container::Mov,
            Codec::H264 | Codec::Hevc | Codec::Av1 => Container::Mp4,
        };
        let sink = Sink::new(
            Muxer::create(out, container, config.frame_rate.num as u32)?,
            config.frame_rate.den as u32,
        );
        let lane = Lane {
            converting: Converting {
                gpu,
                converter,
                targets,
                imports: HashMap::new(),
                pool: pool.clone(),
                chroma: config.chroma(),
            },
            producer,
            width: config.width,
            height: config.height,
            repeats: 0,
            skipped: 0,
            convert: Vec::new(),
        };
        let frame_rate = config.frame_rate;
        let submit = match turbo {
            Some(options) => {
                let encoder = TurboEncoder::with_pool(encoder_config, options, pool)?;
                thread::spawn(move || submit_loop(encoder, consumer, sink, frame_rate))
            }
            None => {
                let encoder = VideoToolboxEncoder::with_pool(encoder_config, pool, None)?;
                thread::spawn(move || submit_loop(encoder, consumer, sink, frame_rate))
            }
        };
        Ok(Pipeline {
            lane,
            submit,
            warm,
            key,
        })
    }

    /// Waits for every queued frame to be encoded and returns the muxer, still open, and the
    /// time each encoder submit took. Every other lane must be dropped first.
    pub fn finish(self) -> Result<(Muxer, Vec<Duration>)> {
        let Pipeline {
            lane,
            submit,
            warm,
            key,
        } = self;
        let Lane {
            converting,
            producer,
            ..
        } = lane;
        drop(producer);
        let Converting { targets, .. } = converting;
        if let Some(slot) = warm.pools.iter_mut().find(|slot| slot.key == key) {
            slot.targets = targets;
        }
        submit.join().expect("submit thread panicked")
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
    let mut warm = Warm::new()?;
    let mut pipeline = Pipeline::new(config, out, &mut warm)?;
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

pub const LEAD_IN: u64 = 20;

#[cfg(target_os = "macos")]
fn physical_memory() -> Option<u64> {
    let mut bytes = 0u64;
    let mut size = std::mem::size_of::<u64>();
    let status = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&raw mut bytes).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0).then_some(bytes)
}

#[cfg(not(target_os = "macos"))]
fn physical_memory() -> Option<u64> {
    None
}

/// An eighth of the machine's memory for frames waiting on the encoder, between 1.5 and 3 GB.
fn memory_budget() -> u64 {
    physical_memory().map_or(MEMORY_BUDGET, |bytes| {
        (bytes / 8).clamp(MEMORY_BUDGET, MAX_MEMORY_BUDGET)
    })
}

/// How a render is split across views: frames are dealt in contiguous runs of `run`, and each
/// view seeks and paints `lead_in` frames before its run and discards them, because a page's
/// output can depend on the frames before it, and on their having been painted, not only seeked.
/// Views must be ticking, since a repeat of the frame before a run would repeat another view's
/// frame.
///
/// With `ranges` the render is cut into that many fixed ranges instead, each painted by a fresh
/// view from its first frame with no lead-in, which is how HyperFrames' workers render. The
/// compositor keeps raster state of `will-change` layers from the frame where each layer first
/// painted, so a range's pixels depend on where its view started; fixed ranges make every
/// output independent of the view count and equal to HyperFrames' with as many workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub views: usize,
    pub run: u64,
    pub lead_in: u64,
    #[serde(default = "enabled")]
    pub turbo: bool,
    #[serde(default)]
    pub ranges: Option<u64>,
}

fn enabled() -> bool {
    true
}

impl Plan {
    pub fn single() -> Plan {
        Plan {
            views: 1,
            run: u64::MAX,
            lead_in: LEAD_IN,
            turbo: true,
            ranges: None,
        }
    }

    pub fn new(views: usize, run: u64) -> Plan {
        Plan {
            views: views.max(1),
            run: run.max(1),
            lead_in: LEAD_IN,
            turbo: true,
            ranges: None,
        }
    }

    /// Cuts `frames` into `ranges` equal ranges, the last one shorter, each for a fresh view.
    pub fn ranged(self, ranges: u64, frames: u64) -> Plan {
        let ranges = ranges.clamp(1, frames.max(1));
        Plan {
            run: frames.div_ceil(ranges).max(1),
            lead_in: 0,
            ranges: Some(ranges),
            ..self
        }
    }

    /// Frames the reorder buffer holds when every view is inside its run: the views ahead of the
    /// next frame to encode hold up to `run` frames each.
    fn window(&self) -> usize {
        let ahead = (self.views as u64 - 1).saturating_mul(self.run) + self.views as u64;
        ahead.max(WINDOW as u64) as usize
    }

    /// The run that keeps the discarded lead-in near a tenth of the frames each view paints.
    pub fn auto_run(lead_in: u64) -> u64 {
        lead_in.saturating_mul(LEAD_IN_RUNS).max(MIN_RUN)
    }

    /// The smallest run a claim may take near the end of a render, so that no view idles while
    /// another finishes a long run.
    fn min_run(&self) -> u64 {
        self.run.min(self.lead_in.saturating_mul(MIN_RUN_LEAD_INS)).max(1)
    }

    /// Cuts a long `run` to an even share of `frames`, but not below `MIN_RUN`, so a short
    /// render still spreads over its views.
    fn sized(mut self, frames: u64) -> Plan {
        if self.ranges.is_none() && self.run > MIN_RUN {
            self.run = self
                .run
                .min(frames.div_ceil(self.views as u64).max(MIN_RUN));
        }
        self
    }

    /// Cuts `run` until the buffers the window needs fit in the memory budget.
    fn fit(mut self, frame_bytes: u64) -> Plan {
        let buffers = (memory_budget() / frame_bytes) as usize;
        let allowed = buffers.saturating_sub(POOL_MARGIN).max(WINDOW);
        if self.ranges.is_some() {
            let fits = 1 + (allowed as u64).saturating_sub(1) / (self.run + 1);
            self.views = self.views.min(fits as usize).max(1);
        } else if self.views > 1 {
            let views = self.views as u64;
            self.run = self
                .run
                .min(((allowed as u64).saturating_sub(views) / (views - 1)).max(1));
        }
        self
    }
}

/// Hands out contiguous runs of frames. A claim takes about `remaining / (2 * views)` frames, at
/// most `plan.run` (so long renders keep their long runs and little lead-in) and at least
/// `plan.min_run()` (so the last claims are short and every view finishes together). A plan with
/// ranges hands out exactly `plan.run` frames each time.
struct Claims {
    next: Mutex<u64>,
    count: u64,
    views: u64,
    max: u64,
    min: u64,
    fixed: bool,
}

impl Claims {
    fn new(plan: &Plan, count: u64) -> Claims {
        Claims {
            next: Mutex::new(0),
            count,
            fixed: plan.ranges.is_some(),
            views: plan.views as u64,
            max: plan.run,
            min: plan.min_run(),
        }
    }

    fn size(&self, remaining: u64) -> u64 {
        if self.fixed || self.views == 1 {
            return remaining.min(self.max);
        }
        (remaining / (2 * self.views))
            .clamp(self.min, self.max)
            .min(remaining)
    }

    fn claim(&self) -> Option<(u64, u64)> {
        let mut next = self.next.lock().unwrap();
        let from = *next;
        let remaining = self.count.saturating_sub(from);
        if remaining == 0 {
            return None;
        }
        let to = from + self.size(remaining);
        *next = to;
        Some((from, to))
    }
}

pub struct Runs<T> {
    pub plan: Plan,
    pub muxer: Muxer,
    pub frames: Vec<T>,
    pub stats: Stats,
    pub lead_in_frames: u64,
}

struct Shared<'a, T> {
    first: u64,
    plan: Plan,
    watchdog: Duration,
    skip: Option<&'a Skip>,
    claims: Claims,
    done: AtomicU64,
    on_frame: &'a (dyn Fn(u64, u64, &T) -> Result<()> + Sync),
    open: &'a (dyn Fn() -> Result<View> + Sync),
    seek: &'a (dyn Fn(&mut View, u64, bool) -> Result<(T, bool)> + Sync),
}

struct Worker<T> {
    frames: Vec<(u64, T)>,
    seek: Vec<Duration>,
    lead_in: u64,
    capture: Capture,
    repeats: u64,
    skipped: u64,
    convert: Vec<Duration>,
}

enum Job {
    Paint(Paint),
    Repeat,
    Skip,
}

struct Handoff {
    jobs: mpsc::Sender<(u64, Job)>,
    finished: mpsc::Receiver<Result<()>>,
    pending: usize,
}

impl Handoff {
    fn send(&mut self, index: u64, job: Job) -> Result<()> {
        self.jobs
            .send((index, job))
            .map_err(|_| anyhow!("the converter stopped"))?;
        self.pending += 1;
        Ok(())
    }

    fn wait(&mut self) -> Result<()> {
        while self.pending > 0 {
            self.finished
                .recv()
                .map_err(|_| anyhow!("the converter stopped"))??;
            self.pending -= 1;
        }
        Ok(())
    }
}

fn convert_frames(
    lane: &mut Lane,
    queued: mpsc::Receiver<(u64, Job)>,
    finished: mpsc::Sender<Result<()>>,
) {
    for (index, job) in queued {
        let result = match job {
            Job::Paint(paint) => lane.frame(index, &paint),
            Job::Repeat => lane.repeat(index),
            Job::Skip => lane.skip(index),
        };
        let failed = result.is_err();
        let _ = finished.send(result);
        if failed {
            break;
        }
    }
}

fn work<T>(lane: &mut Lane, shared: &Shared<T>) -> Result<Worker<T>> {
    let (jobs, queued) = mpsc::channel();
    let (acks, finished) = mpsc::channel();
    let mut worker = thread::scope(|scope| {
        scope.spawn(|| convert_frames(lane, queued, acks));
        drive(
            shared,
            Handoff {
                jobs,
                finished,
                pending: 0,
            },
        )
    })?;
    worker.repeats = lane.repeats;
    worker.skipped = lane.skipped;
    worker.convert = std::mem::take(&mut lane.convert);
    Ok(worker)
}

/// Seeks and captures the view's runs. A delivered surface stays valid until the next begin
/// frame, so its conversion runs on the lane's thread while the page seeks the next frame, and
/// is awaited just before that begin frame.
fn drive<T>(shared: &Shared<T>, mut handoff: Handoff) -> Result<Worker<T>> {
    let mut worker = Worker {
        frames: Vec::new(),
        seek: Vec::new(),
        lead_in: 0,
        capture: Capture::default(),
        repeats: 0,
        skipped: 0,
        convert: Vec::new(),
    };
    let mut view = (shared.open)()?;
    let mut last = None;
    loop {
        let Some((from, to)) = shared.claims.claim() else {
            break;
        };
        if shared.plan.ranges.is_some() && last.is_some() {
            handoff.wait()?;
            std::mem::replace(&mut view, (shared.open)()?).close()?;
            last = None;
        }
        let start = shared.first + from;
        let resume = start
            .saturating_sub(shared.plan.lead_in)
            .max(last.map_or(0, |last| last + 1));
        for before in resume..start {
            (shared.seek)(&mut view, before, false)
                .with_context(|| format!("seek to frame {before}"))?;
            handoff.wait()?;
            worker
                .capture
                .next(&view, shared.watchdog)
                .with_context(|| format!("capture frame {before}"))?;
            worker.lead_in += 1;
        }
        for index in from..to {
            let frame = shared.first + index;
            let step = Instant::now();
            let ask = shared.skip.is_some_and(|skip| skip.wants(frame));
            let (value, unchanged) = (shared.seek)(&mut view, frame, ask)
                .with_context(|| format!("seek to frame {frame}"))?;
            worker.seek.push(step.elapsed());
            if unchanged && index > from {
                handoff.send(index, Job::Skip)?;
            } else {
                handoff.wait()?;
                let paint = worker
                    .capture
                    .next(&view, shared.watchdog)
                    .with_context(|| format!("capture frame {frame}"))?;
                handoff.send(index, paint.map_or(Job::Repeat, Job::Paint))?;
            }
            let done = shared.done.fetch_add(1, Ordering::Relaxed) + 1;
            (shared.on_frame)(done, index, &value)?;
            worker.frames.push((index, value));
        }
        last = Some(shared.first + to - 1);
    }
    handoff.wait()?;
    view.check_project()?;
    view.close()?;
    Ok(worker)
}

/// Renders `config.frames` frames starting at `first` to a file at `out`, with `plan.views` views
/// each on its own thread. `open` makes a view and `seek` leaves it showing a frame and returns
/// what the adapter collects for it, and, when its last argument asks, whether the frame is
/// unchanged from the one before (see `skip::evaluate`); `seek` may replace the view. The first error any view hits
/// stops the others and is the one returned. `on_frame` gets the number of frames done, the
/// frame's index in the render and what `seek` returned for it, after each one, from whichever
/// view finished it. Lead-in frames are not reported.
pub fn render_runs<T: Send>(
    config: &Config,
    plan: Plan,
    first: u64,
    out: &Path,
    warm: &mut Warm,
    open: impl Fn() -> Result<View> + Sync,
    seek: impl Fn(&mut View, u64, bool) -> Result<(T, bool)> + Sync,
    on_frame: impl Fn(u64, u64, &T) -> Result<()> + Sync,
) -> Result<Runs<T>> {
    let started = Instant::now();
    let frame_bytes = u64::from(config.width) * u64::from(config.height) * 3 / 2;
    let plan = plan.fit(frame_bytes).sized(config.frames);
    let mut config = config.clone();
    config.window = config.window.max(plan.window());
    config.turbo &= plan.turbo;
    let step = Instant::now();
    let mut pipeline = Pipeline::new(&config, out, warm)?;
    let pipeline_s = step.elapsed();
    let views = (plan.views as u64)
        .min(config.frames.div_ceil(plan.run))
        .max(1) as usize;
    let specs: Vec<LaneSpec> = (1..views).map(|_| pipeline.spec()).collect();
    let aborter = pipeline.producer.clone();
    let shared = Shared {
        first,
        plan,
        watchdog: config.watchdog,
        skip: config.skip.as_ref(),
        claims: Claims::new(&Plan { views, ..plan }, config.frames),
        done: AtomicU64::new(0),
        on_frame: &on_frame,
        open: &open,
        seek: &seek,
    };
    let failure = Mutex::new(None);
    let guard = |result: Result<Worker<T>>| match result {
        Ok(worker) => Some(worker),
        Err(error) => {
            failure.lock().unwrap().get_or_insert(error);
            aborter.abort();
            None
        }
    };
    let workers: Vec<Option<Worker<T>>> = thread::scope(|scope| {
        let handles: Vec<_> = specs
            .into_iter()
            .map(|spec| {
                scope.spawn(|| guard(spec.open().and_then(|mut lane| work(&mut lane, &shared))))
            })
            .collect();
        let mut workers = vec![guard(work(&mut pipeline, &shared))];
        workers.extend(
            handles
                .into_iter()
                .map(|h| h.join().expect("view thread panicked")),
        );
        workers
    });
    drop(aborter);
    let finished = pipeline.finish();
    if let Some(error) = failure.into_inner().unwrap() {
        return Err(error);
    }
    let (muxer, encode) = finished?;
    let mut stats = Stats {
        elapsed: started.elapsed(),
        pipeline: pipeline_s,
        encode,
        ..Stats::default()
    };
    let mut frames = Vec::new();
    let mut lead_in_frames = 0;
    for worker in workers.into_iter().flatten() {
        frames.extend(worker.frames);
        lead_in_frames += worker.lead_in;
        stats.repeats += worker.repeats;
        stats.skipped += worker.skipped;
        stats.retries += worker.capture.retries;
        stats.seek.extend(worker.seek);
        stats.begin_to_paint.extend(worker.capture.begin_to_paint);
        stats.convert.extend(worker.convert);
    }
    frames.sort_by_key(|(index, _)| *index);
    ensure!(
        frames.len() as u64 == config.frames,
        "{} frames rendered, expected {}",
        frames.len(),
        config.frames
    );
    Ok(Runs {
        plan: Plan { views, ..plan },
        muxer,
        frames: frames.into_iter().map(|(_, value)| value).collect(),
        stats,
        lead_in_frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_run_keeps_the_lead_in_near_a_tenth() {
        assert_eq!(Plan::auto_run(20), 200);
        assert_eq!(Plan::auto_run(0), MIN_RUN);
    }

    fn sizes(plan: Plan, frames: u64) -> Vec<u64> {
        let claims = Claims::new(&plan, frames);
        let mut sizes = Vec::new();
        let mut at = 0;
        while let Some((from, to)) = claims.claim() {
            assert_eq!(from, at);
            sizes.push(to - from);
            at = to;
        }
        assert_eq!(at, frames);
        sizes
    }

    #[test]
    fn claims_shrink_towards_the_end_so_no_view_idles() {
        let plan = Plan::new(4, 200);
        let long = sizes(plan, 1692);
        assert_eq!(long[0], 200);
        assert!(long.windows(2).all(|pair| pair[0] >= pair[1]));
        let min = plan.min_run();
        assert_eq!(min, 80);
        assert!(long[..long.len() - 1].iter().all(|size| *size >= min));
        assert!(long.iter().rev().skip(1).take(2).all(|size| *size == min));
        assert!(long.iter().filter(|size| **size < 200).count() >= 3);
    }

    #[test]
    fn a_fixed_run_stays_fixed_and_a_short_render_is_one_claim_per_view() {
        assert_eq!(sizes(Plan::new(4, 7), 96), [vec![7; 13], vec![5]].concat());
        assert_eq!(sizes(Plan::single(), 300), vec![300]);
        assert_eq!(sizes(Plan::new(4, 200).sized(300), 300), vec![80, 80, 80, 60]);
    }

    #[test]
    fn ranges_are_fixed_whatever_the_view_count() {
        for views in [1, 2, 8] {
            let plan = Plan::new(views, 7).ranged(8, 1185);
            assert_eq!((plan.run, plan.lead_in), (149, 0));
            assert_eq!(sizes(plan, 1185), [vec![149; 7], vec![142]].concat());
            assert_eq!(plan.sized(1185), plan);
        }
        assert_eq!(Plan::single().ranged(8, 5).run, 1);
    }

    #[test]
    fn ranged_views_shrink_to_fit_the_memory_the_ranges_need() {
        let plan = Plan::new(8, 7).ranged(8, 1185);
        let frame_bytes = 1920 * 1080 * 3 / 2;
        let fitted = plan.fit(frame_bytes);
        assert_eq!(fitted.run, 149);
        assert!(fitted.views >= 1 && fitted.views < 8);
        let ahead = (fitted.views as u64 - 1) * fitted.run + fitted.views as u64;
        assert!(ahead * frame_bytes <= memory_budget());
    }

    #[test]
    fn a_run_is_cut_to_an_even_share_but_not_below_the_minimum() {
        let plan = Plan::new(4, 200);
        assert_eq!(plan.sized(1900).run, 200);
        assert_eq!(plan.sized(400).run, 100);
        assert_eq!(plan.sized(150).run, MIN_RUN);
        assert_eq!(Plan::new(4, 7).sized(1000).run, 7);
        assert_eq!(Plan::single().sized(300).run, 300);
    }
}
