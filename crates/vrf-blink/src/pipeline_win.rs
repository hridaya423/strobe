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
use vrf_encode::nvenc::{InputPool, NvencEncoder, NvencSessions, Options, PoolFrame};
use vrf_encode::{Codec, ColourSpace};

use crate::Paint;
use crate::driver::{Config, submit_loop};

pub const POOL_MARGIN: usize = 32;

fn chunk_frames(width: u32, height: u32) -> u64 {
    if u64::from(width) * u64::from(height) > 1920 * 1088 {
        30
    } else {
        60
    }
}

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

    pub fn prepare(&mut self, _width: u32, _height: u32, _colour: ColourSpace) -> Result<()> {
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
    let encoder_config = config.encoder_config();
    let options = Options::from(encoder_config.quality);
    let device = warm.gpu.d3d12_device()?;
    let pool_size = config.window + POOL_MARGIN;
    let frame_rate = config.frame_rate;
    let chunk = chunk_frames(config.width, config.height);
    let (converting, submit) = if config.sessions > 1 {
        let encoder =
            NvencSessions::new(&device, encoder_config, options, config.sessions, pool_size)?;
        let converting = Converting::new(
            warm.gpu.clone(),
            warm.converter.clone(),
            encoder.pools(),
            chunk,
            config.chroma(),
        )?;
        let submit = thread::spawn(move || submit_loop(encoder, consumer, sink, frame_rate));
        (converting, submit)
    } else {
        let encoder = NvencEncoder::with_pool_size(&device, encoder_config, options, pool_size)?;
        let converting = Converting::new(
            warm.gpu.clone(),
            warm.converter.clone(),
            vec![encoder.pool().clone()],
            chunk,
            config.chroma(),
        )?;
        let submit = thread::spawn(move || submit_loop(encoder, consumer, sink, frame_rate));
        (converting, submit)
    };
    Ok(Started {
        converting,
        keep: (),
        submit,
    })
}

pub struct ConvertingSpec {
    gpu: Gpu,
    converter: Arc<Converter>,
    pools: Vec<InputPool>,
    chunk_frames: u64,
    chroma: Chroma,
}

impl ConvertingSpec {
    pub fn open(self) -> Result<Converting> {
        Converting::new(
            self.gpu,
            self.converter,
            self.pools,
            self.chunk_frames,
            self.chroma,
        )
    }
}

pub struct Converting {
    gpu: Gpu,
    converter: Arc<Converter>,
    releaser: Releaser,
    pools: Vec<InputPool>,
    chunk_frames: u64,
    chroma: Chroma,
}

impl Converting {
    fn new(
        gpu: Gpu,
        converter: Arc<Converter>,
        pools: Vec<InputPool>,
        chunk_frames: u64,
        chroma: Chroma,
    ) -> Result<Converting> {
        let releaser = Releaser::new(&gpu)?;
        Ok(Converting {
            gpu,
            converter,
            releaser,
            pools,
            chunk_frames,
            chroma,
        })
    }

    pub fn spec(&self) -> ConvertingSpec {
        ConvertingSpec {
            gpu: self.gpu.clone(),
            converter: self.converter.clone(),
            pools: self.pools.clone(),
            chunk_frames: self.chunk_frames,
            chroma: self.chroma,
        }
    }

    pub fn convert(&mut self, index: u64, paint: &Paint) -> Result<Frame> {
        let texture = self.gpu.import_shared_handle(
            &paint.surface,
            wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
        )?;
        let source = self
            .converter
            .source(&texture.create_view(&Default::default()));
        let session = NvencSessions::session_of(index, self.chunk_frames, self.pools.len());
        let frame = self.pools[session].acquire();
        let planes = PoolTarget::new(&self.gpu, &self.converter, &frame)?;
        let mut commands = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = commands.begin_compute_pass(&Default::default());
            self.converter.encode(
                &mut pass,
                &source,
                &planes.target,
                paint.visible,
                self.chroma,
            );
        }
        self.gpu.queue.submit([commands.finish()]);
        let done = self.releaser.release(&self.gpu, &frame)?;
        self.releaser.wait(done)?;
        Ok(frame)
    }
}
