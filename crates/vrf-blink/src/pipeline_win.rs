use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Result;
use vrf::colour::{Chroma, Format};
use vrf::gpu::Gpu;
use vrf::gpu::convert::Converter;
use vrf::gpu::d3d12::{PoolTarget, Releaser};
use vrf::mux::Muxer;
use vrf::sched::Consumer;
use vrf::sink::Sink;
use vrf_encode::nvenc::{InputPool, NvencEncoder, Options, PoolFrame};
use vrf_encode::{Codec, EncoderConfig};

use crate::Paint;
use crate::driver::{Config, submit_loop};

pub const POOL_MARGIN: usize = 32;

pub type Frame = PoolFrame;
pub type Keep = ();
pub type Submit = JoinHandle<Result<(Muxer, Vec<Duration>)>>;

pub fn turbo_default(_: Codec) -> bool {
    false
}

/// The GPU device and the converter, kept from one render to the next. An NVENC input pool
/// belongs to its encoder session, so it is made per render.
pub struct Warm {
    gpu: Gpu,
    converter: Arc<Converter>,
}

unsafe impl Send for Warm {}

impl Warm {
    pub fn new() -> Result<Warm> {
        let gpu = Gpu::new()?;
        let converter = Converter::new(&gpu, Format::Nv12);
        Ok(Warm {
            gpu,
            converter: Arc::new(converter),
        })
    }

    pub fn prepare(&mut self, _width: u32, _height: u32) -> Result<()> {
        Ok(())
    }

    pub fn keep(&mut self, _: Keep, _: Converting) {}
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
    let options = Options::from(encoder_config.quality);
    let encoder = NvencEncoder::with_pool_size(
        &warm.gpu.d3d12_device()?,
        encoder_config,
        options,
        config.window + POOL_MARGIN,
    )?;
    let converting = Converting::new(
        warm.gpu.clone(),
        warm.converter.clone(),
        encoder.pool().clone(),
    )?;
    let frame_rate = config.frame_rate;
    let submit = thread::spawn(move || submit_loop(encoder, consumer, sink, frame_rate));
    Ok(Started {
        converting,
        keep: (),
        submit,
    })
}

pub struct ConvertingSpec {
    gpu: Gpu,
    converter: Arc<Converter>,
    pool: InputPool,
}

impl ConvertingSpec {
    pub fn open(self) -> Result<Converting> {
        Converting::new(self.gpu, self.converter, self.pool)
    }
}

pub struct Converting {
    gpu: Gpu,
    converter: Arc<Converter>,
    releaser: Releaser,
    pool: InputPool,
}

impl Converting {
    fn new(gpu: Gpu, converter: Arc<Converter>, pool: InputPool) -> Result<Converting> {
        let releaser = Releaser::new(&gpu)?;
        Ok(Converting {
            gpu,
            converter,
            releaser,
            pool,
        })
    }

    pub fn spec(&self) -> ConvertingSpec {
        ConvertingSpec {
            gpu: self.gpu.clone(),
            converter: self.converter.clone(),
            pool: self.pool.clone(),
        }
    }

    pub fn convert(&mut self, paint: &Paint) -> Result<Frame> {
        let texture = self.gpu.import_shared_handle(
            &paint.surface,
            wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
        )?;
        let source = self
            .converter
            .source(&texture.create_view(&Default::default()));
        let frame = self.pool.acquire();
        let planes = PoolTarget::new(&self.gpu, &self.converter, &frame)?;
        let mut commands = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = commands.begin_compute_pass(&Default::default());
            self.converter.encode(
                &mut pass,
                &source,
                &planes.target,
                paint.visible,
                Chroma::Swscale,
            );
        }
        self.gpu.queue.submit([commands.finish()]);
        let done = self.releaser.release(&self.gpu, &frame)?;
        self.releaser.wait(done)?;
        Ok(frame)
    }
}
