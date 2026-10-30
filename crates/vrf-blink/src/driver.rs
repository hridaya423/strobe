use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use vrf::mux::{Container, Muxer};
use vrf::sched::{self, Consumer, Producer, Submission};
use vrf::sink::Sink;
use vrf_encode::{Codec, Encoder, EncoderFrame, Rational};

#[cfg(target_os = "macos")]
pub use crate::pipeline_mac::Warm;
#[cfg(target_os = "macos")]
use crate::pipeline_mac::{
    Converting, ConvertingSpec, Frame, Keep, POOL_MARGIN, Started, Submit, start, turbo_default,
};
#[cfg(windows)]
pub use crate::pipeline_win::Warm;
#[cfg(windows)]
use crate::pipeline_win::{
    Converting, ConvertingSpec, Frame, Keep, POOL_MARGIN, Started, Submit, start, turbo_default,
};
use crate::{Paint, View};

const WINDOW: usize = 32;
const MEMORY_BUDGET: u64 = 3 << 29;
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
            window: WINDOW,
            turbo: turbo_default(codec),
        }
    }
}

#[derive(Debug, Default)]
pub struct Stats {
    pub elapsed: Duration,
    pub pipeline: Duration,
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

pub(crate) fn submit_loop<F: Clone + Into<EncoderFrame>>(
    mut encoder: impl Encoder,
    mut consumer: Consumer<F>,
    mut sink: Sink,
    frame_rate: Rational,
) -> Result<(Muxer, Vec<Duration>)> {
    let mut encode = Vec::new();
    while let Some((index, buffer)) = consumer.recv()? {
        let started = Instant::now();
        let pts = Rational::new(index as i64 * frame_rate.den, frame_rate.num);
        encoder.submit(buffer.into(), pts)?;
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
            let Some(paint) = view.recv_paint(wait) else {
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

/// What a worker thread needs to build its own [`Lane`]. The GPU device is created on the
/// thread that calls [`LaneSpec::open`].
pub struct LaneSpec {
    producer: Producer<Frame>,
    converting: ConvertingSpec,
    width: u32,
    height: u32,
}

impl LaneSpec {
    pub fn open(self) -> Result<Lane> {
        Ok(Lane {
            converting: self.converting.open()?,
            producer: self.producer,
            width: self.width,
            height: self.height,
            repeats: 0,
            convert: Vec::new(),
        })
    }
}

/// One producer of a [`Pipeline`]: converts painted surfaces into encoder buffers and queues them
/// by frame index. Each view thread has its own lane.
pub struct Lane {
    converting: Converting,
    producer: Producer<Frame>,
    width: u32,
    height: u32,
    pub repeats: u64,
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

    pub fn spec(&self) -> LaneSpec {
        LaneSpec {
            producer: self.producer.clone(),
            converting: self.converting.spec(),
            width: self.width,
            height: self.height,
        }
    }
}

/// Feeds frames, in frame order, to an encoder and muxer on a submit thread. Frames come in
/// through lanes; the pipeline is itself the first lane, and uses the GPU state of `warm`.
pub struct Pipeline<'w> {
    lane: Lane,
    submit: Submit,
    warm: &'w mut Warm,
    keep: Keep,
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
        let (producer, consumer) = sched::ordered::<Frame>(config.window);
        let container = match config.codec {
            Codec::ProRes422 => Container::Mov,
            Codec::H264 | Codec::Hevc | Codec::Av1 => Container::Mp4,
        };
        let sink = Sink::new(
            Muxer::create(out, container, config.frame_rate.num as u32)?,
            config.frame_rate.den as u32,
        );
        let Started {
            converting,
            keep,
            submit,
        } = start(config, warm, consumer, sink)?;
        let lane = Lane {
            converting,
            producer,
            width: config.width,
            height: config.height,
            repeats: 0,
            convert: Vec::new(),
        };
        Ok(Pipeline {
            lane,
            submit,
            warm,
            keep,
        })
    }

    /// Waits for every queued frame to be encoded and returns the muxer, still open, and the
    /// time each encoder submit took. Every other lane must be dropped first.
    pub fn finish(self) -> Result<(Muxer, Vec<Duration>)> {
        let Pipeline {
            lane,
            submit,
            warm,
            keep,
        } = self;
        let Lane {
            converting,
            producer,
            ..
        } = lane;
        drop(producer);
        warm.keep(keep, converting);
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

/// How a render is split across views: frames are dealt in contiguous runs of `run`, and each
/// view seeks and paints `lead_in` frames before its run and discards them, because a page's
/// output can depend on the frames before it, and on their having been painted, not only seeked.
/// Views must be ticking, since a repeat of the frame before a run would repeat another view's
/// frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub views: usize,
    pub run: u64,
    pub lead_in: u64,
    #[serde(default = "enabled")]
    pub turbo: bool,
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
        }
    }

    pub fn new(views: usize, run: u64) -> Plan {
        Plan {
            views: views.max(1),
            run: run.max(1),
            lead_in: LEAD_IN,
            turbo: true,
        }
    }

    /// Frames the reorder buffer holds when every view is inside its run: the views ahead of the
    /// next frame to encode hold up to `run` frames each.
    fn window(&self) -> usize {
        let ahead = (self.views as u64 - 1).saturating_mul(self.run) + self.views as u64;
        ahead.max(WINDOW as u64) as usize
    }

    /// Cuts `run` until the buffers the window needs fit in `MEMORY_BUDGET`.
    fn fit(mut self, frame_bytes: u64) -> Plan {
        let buffers = (MEMORY_BUDGET / frame_bytes) as usize;
        let allowed = buffers.saturating_sub(POOL_MARGIN).max(WINDOW);
        if self.views > 1 {
            let views = self.views as u64;
            self.run = self
                .run
                .min(((allowed as u64).saturating_sub(views) / (views - 1)).max(1));
        }
        self
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
    count: u64,
    plan: Plan,
    watchdog: Duration,
    next_run: AtomicU64,
    done: AtomicU64,
    on_frame: &'a (dyn Fn(u64) -> Result<()> + Sync),
    open: &'a (dyn Fn() -> Result<View> + Sync),
    seek: &'a (dyn Fn(&mut View, u64) -> Result<T> + Sync),
}

struct Worker<T> {
    frames: Vec<(u64, T)>,
    seek: Vec<Duration>,
    lead_in: u64,
    capture: Capture,
    repeats: u64,
    convert: Vec<Duration>,
}

fn work<T>(lane: &mut Lane, shared: &Shared<T>) -> Result<Worker<T>> {
    let mut worker = Worker {
        frames: Vec::new(),
        seek: Vec::new(),
        lead_in: 0,
        capture: Capture::default(),
        repeats: 0,
        convert: Vec::new(),
    };
    let mut view = (shared.open)()?;
    let mut last = None;
    loop {
        let run = shared.next_run.fetch_add(1, Ordering::Relaxed);
        let from = run.saturating_mul(shared.plan.run);
        if from >= shared.count {
            break;
        }
        let to = from.saturating_add(shared.plan.run).min(shared.count);
        let start = shared.first + from;
        let resume = start
            .saturating_sub(shared.plan.lead_in)
            .max(last.map_or(0, |last| last + 1));
        for before in resume..start {
            (shared.seek)(&mut view, before).with_context(|| format!("seek to frame {before}"))?;
            worker
                .capture
                .next(&view, shared.watchdog)
                .with_context(|| format!("capture frame {before}"))?;
            worker.lead_in += 1;
        }
        for index in from..to {
            let frame = shared.first + index;
            let step = Instant::now();
            let value = (shared.seek)(&mut view, frame)
                .with_context(|| format!("seek to frame {frame}"))?;
            worker.seek.push(step.elapsed());
            match worker
                .capture
                .next(&view, shared.watchdog)
                .with_context(|| format!("capture frame {frame}"))?
            {
                Some(paint) => lane.frame(index, &paint)?,
                None => lane.repeat(index)?,
            }
            worker.frames.push((index, value));
            let done = shared.done.fetch_add(1, Ordering::Relaxed) + 1;
            (shared.on_frame)(done)?;
        }
        last = Some(shared.first + to - 1);
    }
    view.check_project()?;
    view.close()?;
    worker.repeats = lane.repeats;
    worker.convert = std::mem::take(&mut lane.convert);
    Ok(worker)
}

/// Renders `config.frames` frames starting at `first` to a file at `out`, with `plan.views` views
/// each on its own thread. `open` makes a view and `seek` leaves it showing a frame and returns
/// what the adapter collects for it; `seek` may replace the view. The first error any view hits
/// stops the others and is the one returned. `on_frame` gets the number of frames done after each
/// one, from whichever view finished it.
pub fn render_runs<T: Send>(
    config: &Config,
    plan: Plan,
    first: u64,
    out: &Path,
    warm: &mut Warm,
    open: impl Fn() -> Result<View> + Sync,
    seek: impl Fn(&mut View, u64) -> Result<T> + Sync,
    on_frame: impl Fn(u64) -> Result<()> + Sync,
) -> Result<Runs<T>> {
    let started = Instant::now();
    let frame_bytes = u64::from(config.width) * u64::from(config.height) * 3 / 2;
    let plan = plan.fit(frame_bytes);
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
        count: config.frames,
        plan,
        watchdog: config.watchdog,
        next_run: AtomicU64::new(0),
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
