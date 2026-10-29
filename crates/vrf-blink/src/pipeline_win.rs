use anyhow::Result;
use vrf::colour::{Chroma, Format};
use vrf::gpu::Gpu;
use vrf::gpu::convert::Converter;
use vrf::gpu::d3d12::{PoolTarget, Releaser};
use vrf_encode::EncoderConfig;
use vrf_encode::nvenc::{InputPool, NvencEncoder, Options, PoolFrame};

use crate::Paint;

pub const POOL_MARGIN: usize = 32;

pub type Frame = PoolFrame;
pub type Pool = InputPool;
pub type VideoEncoder = NvencEncoder;

#[derive(Clone)]
pub struct Backend {
    gpu: Gpu,
}

impl Backend {
    pub fn new() -> Result<Backend> {
        Ok(Backend { gpu: Gpu::new()? })
    }

    pub fn gpu(&self) -> Result<Gpu> {
        Ok(self.gpu.clone())
    }

    pub fn encoder(&self, config: EncoderConfig, window: usize) -> Result<(VideoEncoder, Pool)> {
        let options = Options::from(config.quality);
        let encoder = NvencEncoder::with_pool_size(
            &self.gpu.d3d12_device()?,
            config,
            options,
            window + POOL_MARGIN,
        )?;
        let pool = encoder.pool().clone();
        Ok((encoder, pool))
    }
}

pub struct Converting {
    gpu: Gpu,
    converter: Converter,
    releaser: Releaser,
    pool: Pool,
}

impl Converting {
    pub fn new(gpu: Gpu, pool: Pool) -> Result<Converting> {
        let converter = Converter::new(&gpu, Format::Nv12);
        let releaser = Releaser::new(&gpu)?;
        Ok(Converting {
            gpu,
            converter,
            releaser,
            pool,
        })
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
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
