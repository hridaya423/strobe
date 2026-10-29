use std::ffi::c_void;
use std::fmt;

use anyhow::{Context, Result, bail};
use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows::Win32::Graphics::Direct3D12::{D3D12_RESOURCE_DIMENSION_TEXTURE2D, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM,
};
use windows::Win32::System::Threading::GetCurrentProcess;

use super::Gpu;

/// The locally unique id of a graphics adapter. Chromium takes it as
/// `--use-adapter-luid=<high>,<low>`, which is what `Display` prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Luid {
    pub high: i32,
    pub low: u32,
}

impl fmt::Display for Luid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{}", self.high, self.low)
    }
}

/// A duplicate of the NT handle CEF passes to `OnAcceleratedPaint`. CEF closes
/// its own handle when the callback returns, so duplicate inside the callback
/// and import later. The duplicate is closed on drop.
pub struct SharedHandle(HANDLE);

unsafe impl Send for SharedHandle {}

impl SharedHandle {
    /// # Safety
    /// `handle` must be a live NT handle owned by this process.
    pub unsafe fn duplicate(handle: *mut c_void) -> Result<Self> {
        let mut duplicate = HANDLE::default();
        let process = unsafe { GetCurrentProcess() };
        unsafe {
            DuplicateHandle(
                process,
                HANDLE(handle),
                process,
                &mut duplicate,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .context("DuplicateHandle")?;
        Ok(Self(duplicate))
    }
}

impl Drop for SharedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn adapter_luid(adapter: &wgpu::Adapter) -> Option<Luid> {
    let hal = unsafe { adapter.as_hal::<Dx12>() }?;
    let luid = unsafe { hal.as_raw().GetDesc() }.ok()?.AdapterLuid;
    Some(Luid {
        high: luid.HighPart,
        low: luid.LowPart,
    })
}

fn instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

impl Gpu {
    /// Opens the high-performance adapter. On a machine with an integrated and
    /// a discrete GPU, start Chromium with `--use-adapter-luid=` and
    /// [`Gpu::luid`], or its shared handles will not open here. Use
    /// [`Gpu::with_luid`] to follow a Chromium that is already running.
    pub fn new() -> Result<Self> {
        let adapter =
            pollster::block_on(instance().request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            }))
            .context("no D3D12 adapter")?;
        let (device, queue, adapter) = Self::open(adapter)?;
        Ok(Self {
            device,
            queue,
            adapter,
        })
    }

    /// Opens the adapter with this LUID, the one Chromium renders on.
    pub fn with_luid(luid: Luid) -> Result<Self> {
        let adapter = pollster::block_on(instance().enumerate_adapters(wgpu::Backends::DX12))
            .into_iter()
            .find(|adapter| adapter_luid(adapter) == Some(luid))
            .with_context(|| format!("no D3D12 adapter with LUID {luid}"))?;
        let (device, queue, adapter) = Self::open(adapter)?;
        Ok(Self {
            device,
            queue,
            adapter,
        })
    }

    /// Every D3D12 adapter by name and LUID, for `vrf doctor`.
    pub fn adapters() -> Vec<(String, Luid)> {
        pollster::block_on(instance().enumerate_adapters(wgpu::Backends::DX12))
            .iter()
            .filter_map(|adapter| Some((adapter.get_info().name, adapter_luid(adapter)?)))
            .collect()
    }

    pub fn luid(&self) -> Luid {
        adapter_luid(&self.adapter).expect("a D3D12 adapter has a LUID")
    }

    /// Opens the shared handle on this device and wraps the resource as a
    /// texture without copying it. The texture is in the COMMON state and CEF
    /// owns the contents: copy out of it before the next begin frame, then
    /// drop it. `usage` must be a subset of what the resource allows;
    /// `COPY_SRC` always is.
    pub fn import_shared_handle(
        &self,
        handle: &SharedHandle,
        usage: wgpu::TextureUsages,
    ) -> Result<wgpu::Texture> {
        let hal = unsafe { self.device.as_hal::<Dx12>() }.context("device is not D3D12")?;
        let mut resource: Option<ID3D12Resource> = None;
        unsafe { hal.raw_device().OpenSharedHandle(handle.0, &mut resource) }
            .context("OpenSharedHandle")?;
        let resource = resource.context("OpenSharedHandle returned no resource")?;
        let desc = unsafe { resource.GetDesc() };
        if desc.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D {
            bail!("shared handle is not a 2D texture");
        }
        let format = match desc.Format {
            DXGI_FORMAT_B8G8R8A8_UNORM => wgpu::TextureFormat::Bgra8Unorm,
            DXGI_FORMAT_R8G8B8A8_UNORM => wgpu::TextureFormat::Rgba8Unorm,
            other => bail!("shared texture format {other:?} is not supported"),
        };
        let size = wgpu::Extent3d {
            width: desc.Width as u32,
            height: desc.Height,
            depth_or_array_layers: 1,
        };
        let texture_desc = wgpu::TextureDescriptor {
            label: Some("shared handle"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        };
        let hal_texture = unsafe {
            wgpu::hal::dx12::Device::texture_from_raw(
                resource,
                format,
                wgpu::TextureDimension::D2,
                size,
                1,
                1,
            )
        };
        Ok(unsafe {
            self.device.create_texture_from_hal::<Dx12>(
                hal_texture,
                &texture_desc,
                wgpu::TextureUses::PRESENT,
            )
        })
    }
}
