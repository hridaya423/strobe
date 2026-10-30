use std::ffi::c_void;
use std::fmt;
use std::mem::ManuallyDrop;

use anyhow::{Context, Result, bail};
use vrf_encode::nvenc::PoolFrame;
use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_FENCE_FLAG_NONE, D3D12_RESOURCE_BARRIER,
    D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
    D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_STATE_COMMON,
    D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_TRANSITION_BARRIER,
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12Device, ID3D12Fence,
    ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, WaitForSingleObject,
};
use windows::core::Interface;

use super::Gpu;
use super::convert::{Converter, Target};

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

    /// The LUID of the adapter [`Gpu::new`] opens, without opening a device.
    pub fn default_luid() -> Result<Luid> {
        let adapter =
            pollster::block_on(instance().request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            }))
            .context("no D3D12 adapter")?;
        adapter_luid(&adapter).context("the adapter has no LUID")
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

impl Gpu {
    /// The D3D12 device behind this wgpu device, for encoders whose input textures this device
    /// must be able to write.
    pub fn d3d12_device(&self) -> Result<ID3D12Device> {
        let hal = unsafe { self.device.as_hal::<Dx12>() }.context("device is not D3D12")?;
        Ok(hal.raw_device().clone())
    }
}

/// The planes of an encoder's NV12 or P010 pool texture, wrapped as wgpu textures for one
/// convert pass. The wrappers start in the COMMON state, so the pass transitions the planes to
/// unordered access, and [`Releaser::release`] returns them to COMMON for the encoder. Drop it
/// once the pass is submitted.
pub struct PoolTarget {
    pub target: Target,
    _y: wgpu::Texture,
    _cbcr: wgpu::Texture,
}

impl PoolTarget {
    pub fn new(gpu: &Gpu, converter: &Converter, frame: &PoolFrame) -> Result<PoolTarget> {
        let (y_format, cbcr_format) = Converter::plane_formats(converter.format());
        let (width, height) = frame.size();
        let y = plane(gpu, frame, 0, y_format, width, height);
        let cbcr = plane(gpu, frame, 1, cbcr_format, width / 2, height / 2);
        let target = converter.target(
            &y.create_view(&Default::default()),
            &cbcr.create_view(&Default::default()),
        );
        Ok(PoolTarget {
            target,
            _y: y,
            _cbcr: cbcr,
        })
    }
}

fn plane(
    gpu: &Gpu,
    frame: &PoolFrame,
    slice: u32,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = unsafe {
        wgpu::hal::dx12::Device::texture_from_raw(
            frame.resource().clone(),
            format,
            wgpu::TextureDimension::D2,
            size,
            1,
            1,
        )
    }
    .with_plane_slice(slice);
    unsafe {
        gpu.device.create_texture_from_hal::<Dx12>(
            texture,
            &wgpu::TextureDescriptor {
                label: Some("pool plane"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            },
            wgpu::TextureUses::PRESENT,
        )
    }
}

const RELEASE_LISTS: usize = 8;

struct ReleaseList {
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    last: u64,
}

/// Puts a pool texture back in the COMMON state on the device's queue after a convert pass, and
/// signals a fence for the point where the pass and the transition have finished.
pub struct Releaser {
    fence: ID3D12Fence,
    event: HANDLE,
    lists: Vec<ReleaseList>,
    value: u64,
}

unsafe impl Send for Releaser {}

impl Releaser {
    pub fn new(gpu: &Gpu) -> Result<Releaser> {
        let device = gpu.d3d12_device()?;
        let lists = (0..RELEASE_LISTS)
            .map(|_| unsafe {
                let allocator: ID3D12CommandAllocator =
                    device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
                let list: ID3D12GraphicsCommandList = device.CreateCommandList(
                    0,
                    D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &allocator,
                    None,
                )?;
                list.Close()?;
                Ok(ReleaseList {
                    allocator,
                    list,
                    last: 0,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Releaser {
            fence: unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE)? },
            event: unsafe { CreateEventW(None, false, false, None)? },
            lists,
            value: 0,
        })
    }

    /// Queues the transition behind everything already submitted to the queue, and returns the
    /// fence value that marks its completion.
    pub fn release(&mut self, gpu: &Gpu, frame: &PoolFrame) -> Result<u64> {
        let hal = unsafe { gpu.device.as_hal::<Dx12>() }.context("device is not D3D12")?;
        let slot = (self.value as usize) % RELEASE_LISTS;
        let last = self.lists[slot].last;
        self.wait(last)?;
        let entry = &mut self.lists[slot];
        unsafe {
            entry.allocator.Reset()?;
            entry.list.Reset(&entry.allocator, None)?;
            let barrier = D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: ManuallyDrop::new(Some(frame.resource().clone())),
                        Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                        StateAfter: D3D12_RESOURCE_STATE_COMMON,
                    }),
                },
            };
            entry.list.ResourceBarrier(std::slice::from_ref(&barrier));
            let transition = ManuallyDrop::into_inner(barrier.Anonymous.Transition);
            drop(ManuallyDrop::into_inner(transition.pResource));
            entry.list.Close()?;
            let queue = hal.raw_queue();
            queue.ExecuteCommandLists(&[Some(entry.list.cast::<ID3D12CommandList>()?)]);
            self.value += 1;
            queue.Signal(&self.fence, self.value)?;
        }
        entry.last = self.value;
        Ok(self.value)
    }

    pub fn wait(&self, value: u64) -> Result<()> {
        unsafe {
            if self.fence.GetCompletedValue() < value {
                self.fence.SetEventOnCompletion(value, self.event)?;
                WaitForSingleObject(self.event, INFINITE);
            }
        }
        Ok(())
    }
}

impl Drop for Releaser {
    fn drop(&mut self) {
        let _ = self.wait(self.value);
        let _ = unsafe { CloseHandle(self.event) };
    }
}
