use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use crate::avcc::AvcRecord;
use crate::videotoolbox::pool::{POOL_SIZE, PixelFormat, PixelPool, PoolBuffer};
use crate::videotoolbox::{VideoToolboxEncoder, capabilities};
use crate::x264::X264Encoder;
use crate::{Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Rational, StreamInfo};

/// Frames VideoToolbox keeps in flight to reach its ceiling (`02` §2.3). With more than this
/// waiting, the renderer is outrunning the media engine.
const VT_HEADROOM: usize = 32;

/// Starting guesses for lane speed, replaced by measurements after a quarter second of work:
/// VideoToolbox H.264 runs about 480 Mpixel/s (`02` §2.1) and libx264 `veryfast` about half that.
const VT_PRIOR_PIXELS_PER_SECOND: f64 = 480e6;
const X264_PRIOR_SHARE: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurboOptions {
    /// Frames per closed GOP. Each chunk starts with an IDR frame and goes whole to one lane.
    pub chunk_frames: u32,
    /// Threads for the libx264 lane; 0 lets libx264 choose. Leave cores for the renderer.
    pub x264_threads: u32,
    /// Adds the libx264 lane (H.264 only).
    pub cpu_lane: bool,
    /// VideoToolbox sessions; `None` is the machine's `RecommendedParallelizationLimit`.
    pub vt_sessions: Option<usize>,
    /// Frames the VideoToolbox lane must have waiting before the CPU lane gets a chunk.
    pub vt_headroom: usize,
}

impl TurboOptions {
    /// Chunks of 60 frames up to 1080p and 30 above, which keeps the buffers a chunk holds under
    /// a gigabyte at 4K.
    pub fn for_frame(width: u32, height: u32) -> TurboOptions {
        let large = u64::from(width) * u64::from(height) > 1920 * 1088;
        TurboOptions {
            chunk_frames: if large { 30 } else { 60 },
            ..TurboOptions::default()
        }
    }
}

impl Default for TurboOptions {
    fn default() -> Self {
        TurboOptions {
            chunk_frames: 60,
            x264_threads: 0,
            cpu_lane: true,
            vt_sessions: None,
            vt_headroom: VT_HEADROOM,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneKind {
    VideoToolbox,
    X264,
}

/// How much of the timeline one lane was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneStats {
    pub kind: LaneKind,
    pub chunks: u64,
    pub frames: u64,
}

enum LaneEncoder {
    VideoToolbox(Box<VideoToolboxEncoder>),
    X264(Box<X264Encoder>),
}

impl LaneEncoder {
    fn submit(&mut self, frame: EncoderFrame, pts: Rational, keyframe: bool) -> Result<()> {
        match self {
            LaneEncoder::VideoToolbox(e) => e.submit_keyframe(frame, pts, keyframe),
            LaneEncoder::X264(e) => e.submit_keyframe(frame, pts, keyframe),
        }
    }

    fn drain(&mut self) -> Result<Vec<Packet>> {
        match self {
            LaneEncoder::VideoToolbox(e) => e.drain(),
            LaneEncoder::X264(e) => e.drain(),
        }
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        match self {
            LaneEncoder::VideoToolbox(e) => e.flush(),
            LaneEncoder::X264(e) => e.flush(),
        }
    }

    fn stream_info(&self) -> Result<StreamInfo> {
        match self {
            LaneEncoder::VideoToolbox(e) => e.stream_info(),
            LaneEncoder::X264(e) => e.stream_info(),
        }
    }
}

enum Job {
    Frame {
        buffer: PoolBuffer,
        pts: Rational,
        keyframe: bool,
    },
    Flush(Sender<()>),
}

struct Ordered {
    next: u64,
    pending: BTreeMap<u64, Packet>,
    ready: Vec<Packet>,
}

struct Shared {
    frame_rate: Rational,
    ordered: Mutex<Ordered>,
    video_toolbox_config: OnceLock<Vec<u8>>,
    error: OnceLock<String>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Ordered> {
        self.ordered.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn index(&self, time: Rational) -> u64 {
        let scaled = i128::from(time.num) * i128::from(self.frame_rate.num);
        (scaled / (i128::from(time.den) * i128::from(self.frame_rate.den))) as u64
    }

    fn push(&self, packets: Vec<Packet>) {
        let mut ordered = self.lock();
        for packet in packets {
            let index = self.index(packet.pts);
            ordered.pending.insert(index, packet);
        }
        loop {
            let next = ordered.next;
            let Some(packet) = ordered.pending.remove(&next) else {
                break;
            };
            ordered.ready.push(packet);
            ordered.next += 1;
        }
    }
}

struct Counters {
    backlog: AtomicUsize,
    done: AtomicU64,
}

fn run_lane(
    mut encoder: LaneEncoder,
    jobs: Receiver<Job>,
    shared: Arc<Shared>,
    counters: Arc<Counters>,
) {
    let output = |encoder: &LaneEncoder, packets: Vec<Packet>| {
        if packets.is_empty() {
            return;
        }
        if let (LaneEncoder::VideoToolbox(_), Ok(info)) = (encoder, encoder.stream_info()) {
            let _ = shared.video_toolbox_config.set(info.config);
        }
        counters.backlog.fetch_sub(packets.len(), Ordering::SeqCst);
        counters
            .done
            .fetch_add(packets.len() as u64, Ordering::SeqCst);
        shared.push(packets);
    };
    let fail = |error: anyhow::Error| {
        let _ = shared.error.set(format!("{error:#}"));
    };
    for job in jobs {
        match job {
            Job::Frame {
                buffer,
                pts,
                keyframe,
            } => {
                let result = encoder
                    .submit(EncoderFrame::PixelBuffer(buffer), pts, keyframe)
                    .and_then(|()| encoder.drain());
                match result {
                    Ok(packets) => output(&encoder, packets),
                    Err(error) => fail(error),
                }
            }
            Job::Flush(done) => {
                match encoder.flush() {
                    Ok(packets) => output(&encoder, packets),
                    Err(error) => fail(error),
                }
                let _ = done.send(());
            }
        }
    }
}

const RATE_WINDOW: Duration = Duration::from_millis(250);

/// A lane's throughput in frames per second while it has work, from windows of at least
/// `RATE_WINDOW` of busy time.
struct Meter {
    rate: f64,
    measured: bool,
    sampled_at: Instant,
    backlog: usize,
    done: u64,
    window_done: u64,
    window_busy: Duration,
}

impl Meter {
    fn new(prior: f64) -> Meter {
        Meter {
            rate: prior,
            measured: false,
            sampled_at: Instant::now(),
            backlog: 0,
            done: 0,
            window_done: 0,
            window_busy: Duration::ZERO,
        }
    }

    fn sample(&mut self, counters: &Counters) {
        let now = Instant::now();
        let backlog = counters.backlog.load(Ordering::SeqCst);
        let done = counters.done.load(Ordering::SeqCst);
        if self.backlog > 0 || backlog > 0 {
            self.window_busy += now - self.sampled_at;
        }
        self.window_done += done - self.done;
        (self.sampled_at, self.backlog, self.done) = (now, backlog, done);
        if self.window_busy >= RATE_WINDOW {
            let window = self.window_done as f64 / self.window_busy.as_secs_f64();
            self.rate = if self.measured {
                (self.rate + window) / 2.0
            } else {
                window
            };
            self.measured = true;
            (self.window_done, self.window_busy) = (0, Duration::ZERO);
        }
    }
}

struct Lane {
    kind: LaneKind,
    jobs: Option<Sender<Job>>,
    counters: Arc<Counters>,
    meter: Meter,
    thread: Option<JoinHandle<()>>,
    chunks: u64,
    frames: u64,
}

/// An H.264 or HEVC encoder that splits the timeline into closed-GOP chunks and runs them on
/// several encoders at once: VideoToolbox sessions (as many as the machine recommends) and, for
/// H.264, a libx264 lane on the CPU.
///
/// Frames come in order, as for any [`Encoder`]. The first frame of every chunk is an IDR frame
/// and the chunk goes whole to one lane, picked when it starts: the CPU lane gets it only when
/// VideoToolbox has more than a pipeline's worth of frames waiting and the CPU lane would finish
/// the chunk sooner, by the rates the lanes have shown. A renderer that is slower than the media
/// engine therefore never touches the CPU lane. Lanes buffer whole chunks, so
/// [`TurboEncoder::pool`] is larger than a single encoder's.
///
/// Packets leave in presentation order with `dts` equal to `pts`, and the stream parameters are
/// one `avcC` that holds the SPS and PPS of every lane, so the file has one sample entry and the
/// muxer needs no special case. HEVC chunks come from VideoToolbox sessions only: mixing HEVC
/// encoders produced files that ffmpeg and AVFoundation cannot decode (`02` §3.3).
pub struct TurboEncoder {
    config: EncoderConfig,
    options: TurboOptions,
    shared: Arc<Shared>,
    lanes: Vec<Lane>,
    pool: PixelPool,
    first: Option<u64>,
    submitted: u64,
    current: usize,
    x264_record: Option<AvcRecord>,
    merged_config: OnceLock<Vec<u8>>,
}

fn spawn(kind: LaneKind, encoder: LaneEncoder, shared: &Arc<Shared>, prior: f64) -> Lane {
    let (jobs, receiver) = channel();
    let counters = Arc::new(Counters {
        backlog: AtomicUsize::new(0),
        done: AtomicU64::new(0),
    });
    let thread = thread::Builder::new()
        .name(format!("turbo-{kind:?}"))
        .spawn({
            let (shared, counters) = (shared.clone(), counters.clone());
            move || run_lane(encoder, receiver, shared, counters)
        })
        .expect("spawning a lane thread");
    Lane {
        kind,
        jobs: Some(jobs),
        counters,
        meter: Meter::new(prior),
        thread: Some(thread),
        chunks: 0,
        frames: 0,
    }
}

impl TurboEncoder {
    pub fn new(config: EncoderConfig) -> Result<TurboEncoder> {
        let options = TurboOptions::for_frame(config.width, config.height);
        TurboEncoder::with_options(config, options)
    }

    pub fn with_options(config: EncoderConfig, options: TurboOptions) -> Result<TurboEncoder> {
        ensure!(
            matches!(config.codec, Codec::H264 | Codec::Hevc),
            "turbo mode chunks H.264 and HEVC; {:?} has its own engine",
            config.codec
        );
        ensure!(options.chunk_frames > 0, "chunks need at least one frame");
        let cpu_lane = options.cpu_lane && config.codec == Codec::H264;
        let sessions = match options.vt_sessions {
            Some(sessions) => sessions,
            None => capabilities(config.codec)?.parallelization_limit,
        };
        ensure!(sessions > 0, "turbo mode needs a VideoToolbox session");
        let chunk = options.chunk_frames as usize;
        let size = POOL_SIZE + chunk * (sessions - 1 + if cpu_lane { 2 } else { 0 });
        let format = if config.ten_bit {
            PixelFormat::P010
        } else {
            PixelFormat::Nv12
        };
        let pool =
            PixelPool::with_size(config.width as usize, config.height as usize, format, size)?;
        let shared = Arc::new(Shared {
            frame_rate: config.frame_rate,
            ordered: Mutex::new(Ordered {
                next: 0,
                pending: BTreeMap::new(),
                ready: Vec::new(),
            }),
            video_toolbox_config: OnceLock::new(),
            error: OnceLock::new(),
        });
        let pixels = f64::from(config.width) * f64::from(config.height);
        let video_toolbox_prior = VT_PRIOR_PIXELS_PER_SECOND / pixels;
        let mut lanes = Vec::new();
        for _ in 0..sessions {
            let mut lane_config = config.clone();
            lane_config.keyframe_interval = options.chunk_frames;
            let encoder =
                VideoToolboxEncoder::with_pool(lane_config, pool.clone(), options.vt_sessions)?;
            lanes.push(spawn(
                LaneKind::VideoToolbox,
                LaneEncoder::VideoToolbox(Box::new(encoder)),
                &shared,
                video_toolbox_prior,
            ));
        }
        let mut x264_record = None;
        if cpu_lane {
            let mut lane_config = config.clone();
            lane_config.keyframe_interval = options.chunk_frames;
            let encoder = X264Encoder::new(lane_config, options.x264_threads)?;
            x264_record = Some(AvcRecord::parse(&encoder.stream_info()?.config)?);
            lanes.push(spawn(
                LaneKind::X264,
                LaneEncoder::X264(Box::new(encoder)),
                &shared,
                video_toolbox_prior * X264_PRIOR_SHARE,
            ));
        }
        Ok(TurboEncoder {
            config,
            options,
            shared,
            lanes,
            pool,
            first: None,
            submitted: 0,
            current: 0,
            x264_record,
            merged_config: OnceLock::new(),
        })
    }

    /// The pool to render into. It holds more buffers than a single encoder's, because every
    /// lane keeps up to a chunk of frames waiting.
    pub fn pool(&self) -> &PixelPool {
        &self.pool
    }

    pub fn lane_stats(&self) -> Vec<LaneStats> {
        self.lanes
            .iter()
            .map(|lane| LaneStats {
                kind: lane.kind,
                chunks: lane.chunks,
                frames: lane.frames,
            })
            .collect()
    }

    fn check(&self) -> Result<()> {
        match self.shared.error.get() {
            Some(error) => bail!("{error}"),
            None => Ok(()),
        }
    }

    fn pick(&self) -> usize {
        let chunk = f64::from(self.options.chunk_frames);
        let backlog = |i: usize| self.lanes[i].counters.backlog.load(Ordering::SeqCst);
        let finish = |i: usize| (backlog(i) as f64 + chunk) / self.lanes[i].meter.rate;
        let video_toolbox = (0..self.lanes.len())
            .filter(|&i| self.lanes[i].kind == LaneKind::VideoToolbox)
            .min_by(|&a, &b| finish(a).total_cmp(&finish(b)))
            .expect("a VideoToolbox lane exists");
        match self.lanes.iter().position(|l| l.kind == LaneKind::X264) {
            Some(cpu)
                if backlog(video_toolbox) > self.options.vt_headroom
                    && finish(cpu) < finish(video_toolbox) =>
            {
                cpu
            }
            _ => video_toolbox,
        }
    }

    fn index_of(&self, pts: Rational) -> Result<u64> {
        let rate = self.config.frame_rate;
        let scaled = i128::from(pts.num) * i128::from(rate.num);
        let step = i128::from(pts.den) * i128::from(rate.den);
        ensure!(
            scaled >= 0 && scaled % step == 0,
            "{}/{} s is not a whole frame at {}/{} fps",
            pts.num,
            pts.den,
            rate.num,
            rate.den
        );
        u64::try_from(scaled / step).context("frame index out of range")
    }
}

impl Encoder for TurboEncoder {
    fn submit(&mut self, frame: EncoderFrame, pts: Rational) -> Result<()> {
        self.check()?;
        let EncoderFrame::PixelBuffer(buffer) = frame;
        let index = self.index_of(pts)?;
        for lane in &mut self.lanes {
            lane.meter.sample(&lane.counters);
        }
        let first = *self.first.get_or_insert_with(|| {
            self.shared.lock().next = index;
            index
        });
        ensure!(
            index == first + self.submitted,
            "frame {index} is out of order; expected {}",
            first + self.submitted
        );
        let keyframe = self
            .submitted
            .is_multiple_of(u64::from(self.options.chunk_frames));
        if keyframe {
            self.current = self.pick();
            self.lanes[self.current].chunks += 1;
        }
        self.submitted += 1;
        let lane = &mut self.lanes[self.current];
        lane.frames += 1;
        lane.counters.backlog.fetch_add(1, Ordering::SeqCst);
        lane.jobs
            .as_ref()
            .expect("lanes run until the encoder drops")
            .send(Job::Frame {
                buffer,
                pts,
                keyframe,
            })
            .map_err(|_| anyhow::anyhow!("a lane stopped: {:?}", self.shared.error.get()))
    }

    fn drain(&mut self) -> Result<Vec<Packet>> {
        self.check()?;
        Ok(std::mem::take(&mut self.shared.lock().ready))
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        let waits: Vec<_> = self
            .lanes
            .iter()
            .map(|lane| {
                let (done, wait) = channel();
                let sent = lane
                    .jobs
                    .as_ref()
                    .expect("lanes run until the encoder drops")
                    .send(Job::Flush(done));
                (sent.is_ok(), wait)
            })
            .collect();
        for (sent, wait) in waits {
            if sent {
                let _ = wait.recv();
            }
        }
        self.drain()
    }

    fn stream_info(&self) -> Result<StreamInfo> {
        let video_toolbox = self
            .shared
            .video_toolbox_config
            .get()
            .context("no frame was encoded")?;
        let config = match (&self.x264_record, self.merged_config.get()) {
            (_, Some(merged)) => merged.clone(),
            (Some(x264), None) => {
                let mut record = AvcRecord::parse(video_toolbox)?;
                record.merge(x264)?;
                self.merged_config.get_or_init(|| record.to_bytes()).clone()
            }
            (None, None) => video_toolbox.clone(),
        };
        Ok(StreamInfo {
            codec: self.config.codec,
            width: self.config.width,
            height: self.config.height,
            timebase: Rational::new(1, self.config.frame_rate.num),
            config,
        })
    }

    fn finish(mut self) -> Result<StreamInfo> {
        let undrained = self.flush()?.len();
        ensure!(
            undrained == 0,
            "finish found {undrained} undrained packets; drain after flush"
        );
        self.stream_info()
    }
}

impl Drop for TurboEncoder {
    fn drop(&mut self) {
        for lane in &mut self.lanes {
            lane.jobs = None;
        }
        for lane in &mut self.lanes {
            if let Some(thread) = lane.thread.take() {
                let _ = thread.join();
            }
        }
    }
}
