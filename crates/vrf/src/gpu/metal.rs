use std::collections::HashMap;
use std::collections::hash_map::Entry;

use anyhow::{Context, Result, bail};
use objc2_io_surface::IOSurfaceRef;
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTextureDescriptor, MTLTextureType,
    MTLTextureUsage,
};

use vrf_encode::videotoolbox::pool::PoolBuffer;

use super::Gpu;
use super::convert::{Converter, Target};

fn metal_format(format: wgpu::TextureFormat) -> Result<MTLPixelFormat> {
    Ok(match format {
        wgpu::TextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        wgpu::TextureFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        wgpu::TextureFormat::R8Uint => MTLPixelFormat::R8Uint,
        wgpu::TextureFormat::Rg8Uint => MTLPixelFormat::RG8Uint,
        wgpu::TextureFormat::R16Uint => MTLPixelFormat::R16Uint,
        wgpu::TextureFormat::Rg16Uint => MTLPixelFormat::RG16Uint,
        other => bail!("{other:?} cannot be imported from an IOSurface"),
    })
}

impl Gpu {
    /// Wraps one plane of an IOSurface as a texture without copying. The texture
    /// can be sampled, copied from and rendered to. Keep `surface` alive for as
    /// long as the texture is used.
    pub fn import_iosurface(
        &self,
        surface: &IOSurfaceRef,
        plane: usize,
        format: wgpu::TextureFormat,
    ) -> Result<wgpu::Texture> {
        let (width, height) = (
            surface.width_of_plane(plane) as u32,
            surface.height_of_plane(plane) as u32,
        );
        let desc = wgpu::TextureDescriptor {
            label: Some("iosurface"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: if format.sample_type(None, None) == Some(wgpu::TextureSampleType::Uint) {
                wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
            } else {
                wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING
            },
            view_formats: &[],
        };
        let metal_desc = MTLTextureDescriptor::new();
        unsafe {
            metal_desc.setTextureType(MTLTextureType::Type2D);
            metal_desc.setPixelFormat(metal_format(format)?);
            metal_desc.setWidth(width as usize);
            metal_desc.setHeight(height as usize);
            metal_desc.setStorageMode(MTLStorageMode::Shared);
            metal_desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite);
        }
        let raw = self
            .mtl_device
            .newTextureWithDescriptor_iosurface_plane(&metal_desc, surface, plane)
            .context("Metal rejected the IOSurface plane")?;
        let hal = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                raw,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
                None,
            )
        };
        Ok(unsafe {
            self.device
                .create_texture_from_hal::<wgpu::hal::api::Metal>(
                    hal,
                    &desc,
                    wgpu::TextureUses::RESOURCE,
                )
        })
    }
}

/// Convert targets for pool buffers, built on first use and kept by IOSurface ID. A pool
/// hands out the same few IOSurfaces for the whole render, so each is imported once.
#[derive(Default)]
pub struct PoolTargets {
    targets: HashMap<u32, Target>,
}

impl PoolTargets {
    pub fn get(
        &mut self,
        gpu: &Gpu,
        converter: &Converter,
        buffer: &PoolBuffer,
    ) -> Result<&Target> {
        match self.targets.entry(buffer.surface_id()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let (y_format, cbcr_format) = Converter::plane_formats(converter.format());
                let y = gpu.import_iosurface(buffer.surface(), 0, y_format)?;
                let cbcr = gpu.import_iosurface(buffer.surface(), 1, cbcr_format)?;
                Ok(entry.insert(converter.target(
                    &y.create_view(&Default::default()),
                    &cbcr.create_view(&Default::default()),
                )))
            }
        }
    }
}
