use std::collections::HashMap;

use anyhow::Result;
use vrf::colour::{Chroma, Format};
use vrf::gpu::Gpu;
use vrf::gpu::convert::{Converter, Source};
use vrf::gpu::metal::PoolTargets;
use vrf_encode::EncoderConfig;
use vrf_encode::videotoolbox::VideoToolboxEncoder;
use vrf_encode::videotoolbox::pool::{PixelFormat, PixelPool, PoolBuffer};

use crate::Paint;

const MAX_IMPORTS: usize = 16;
pub const POOL_MARGIN: usize = 32;

pub type Frame = PoolBuffer;
pub type Pool = PixelPool;
pub type VideoEncoder = VideoToolboxEncoder;

#[derive(Clone)]
pub struct Backend;

impl Backend {
    pub fn new() -> Result<Backend> {
        Ok(Backend)
    }

    pub fn gpu(&self) -> Result<Gpu> {
        Gpu::new()
    }

    pub fn encoder(&self, config: EncoderConfig, window: usize) -> Result<(VideoEncoder, Pool)> {
        let pool = PixelPool::with_size(
            config.width as usize,
            config.height as usize,
            PixelFormat::Nv12,
            window + POOL_MARGIN,
        )?;
        let encoder = VideoToolboxEncoder::with_pool(config, pool, None)?;
        let pool = encoder.pool().clone();
        Ok((encoder, pool))
    }
}

pub struct Converting {
    gpu: Gpu,
    converter: Converter,
    targets: PoolTargets,
    imports: HashMap<u32, Source>,
    pool: Pool,
}

impl Converting {
    pub fn new(gpu: Gpu, pool: Pool) -> Result<Converting> {
        let converter = Converter::new(&gpu, Format::Nv12);
        Ok(Converting {
            gpu,
            converter,
            targets: PoolTargets::default(),
            imports: HashMap::new(),
            pool,
        })
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    pub fn convert(&mut self, paint: &Paint) -> Result<Frame> {
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
