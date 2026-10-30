use std::collections::HashMap;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Result;
use vrf::colour::{Chroma, Format};
use vrf::gpu::Gpu;
use vrf::gpu::convert::{Converter, Source};
use vrf::gpu::metal::PoolTargets;
use vrf::mux::Muxer;
use vrf::sched::Consumer;
use vrf::sink::Sink;
use vrf_encode::turbo::{TurboEncoder, TurboOptions};
use vrf_encode::videotoolbox::pool::{PixelFormat, PixelPool, PoolBuffer};
use vrf_encode::videotoolbox::{self, VideoToolboxEncoder};
use vrf_encode::x264;
use vrf_encode::{Codec, EncoderConfig, Rational};

use crate::Paint;
use crate::driver::{Config, submit_loop};

const MAX_IMPORTS: usize = 16;
const WARM_POOLS: usize = 2;
pub const POOL_MARGIN: usize = 32;

pub type Frame = PoolBuffer;
pub type Keep = PoolKey;
pub type Submit = JoinHandle<Result<(Muxer, Vec<Duration>)>>;

type PoolKey = (u32, u32, PixelFormat, usize);

pub fn turbo_default(codec: Codec) -> bool {
    codec == Codec::H264 && x264::available().is_ok()
}

fn turbo_options(config: &Config) -> Option<TurboOptions> {
    config
        .turbo
        .then(|| TurboOptions::for_frame(config.width, config.height))
}

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
    pub fn prepare(&mut self, width: u32, height: u32) -> Result<()> {
        let config = Config::new(width, height, Rational::new(30, 1), Codec::H264, 0);
        let encoder_config = EncoderConfig::new(config.codec, width, height, config.frame_rate);
        let key = pool_key(&config, &encoder_config, turbo_options(&config))?;
        self.slot(key).map(|_| ())
    }

    fn slot(&mut self, key: PoolKey) -> Result<&mut PoolSlot> {
        let at = match self.pools.iter().position(|slot| slot.key == key) {
            Some(at) => at,
            None => {
                if self.pools.len() >= WARM_POOLS {
                    self.pools.remove(0);
                }
                let (width, height, format, size) = key;
                self.pools.push(PoolSlot {
                    key,
                    pool: PixelPool::with_size(width as usize, height as usize, format, size)?,
                    targets: PoolTargets::default(),
                });
                self.pools.len() - 1
            }
        };
        Ok(&mut self.pools[at])
    }

    pub fn keep(&mut self, key: Keep, converting: Converting) {
        if let Some(slot) = self.pools.iter_mut().find(|slot| slot.key == key) {
            slot.targets = converting.targets;
        }
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
    ))
}

pub struct Started {
    pub converting: Converting,
    pub keep: Keep,
    pub submit: Submit,
}

pub fn start(
    config: &Config,
    warm: &mut Warm,
    consumer: Consumer<Frame>,
    sink: Sink,
) -> Result<Started> {
    let encoder_config =
        EncoderConfig::new(config.codec, config.width, config.height, config.frame_rate);
    let turbo = turbo_options(config);
    let key = pool_key(config, &encoder_config, turbo)?;
    let gpu = warm.gpu.clone();
    let converter = warm.converter.clone();
    let slot = warm.slot(key)?;
    let pool = slot.pool.clone();
    let targets = std::mem::take(&mut slot.targets);
    let converting = Converting {
        gpu,
        converter,
        targets,
        imports: HashMap::new(),
        pool: pool.clone(),
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
    Ok(Started {
        converting,
        keep: key,
        submit,
    })
}

pub struct ConvertingSpec {
    pool: PixelPool,
}

impl ConvertingSpec {
    pub fn open(self) -> Result<Converting> {
        let gpu = Gpu::new()?;
        let converter = Converter::new(&gpu, Format::Nv12);
        Ok(Converting {
            gpu: Arc::new(gpu),
            converter: Arc::new(converter),
            targets: PoolTargets::default(),
            imports: HashMap::new(),
            pool: self.pool,
        })
    }
}

pub struct Converting {
    gpu: Arc<Gpu>,
    converter: Arc<Converter>,
    targets: PoolTargets,
    imports: HashMap<u32, Source>,
    pool: PixelPool,
}

impl Converting {
    pub fn spec(&self) -> ConvertingSpec {
        ConvertingSpec {
            pool: self.pool.clone(),
        }
    }

    pub fn convert(&mut self, _index: u64, paint: &Paint) -> Result<Frame> {
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
