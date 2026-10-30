use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{Context, Result, ensure};
use nvidia_video_codec_sdk::sys::nvEncodeAPI as nv;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_DESC,
    D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
    D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON,
    D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12Device, ID3D12Fence, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC,
};
use windows::core::Interface;

use super::api::{Session, zeroed};
use super::version;

/// Input textures per encoder by default, and the depth of the output ring.
pub const POOL_SIZE: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Nv12,
    P010,
}

impl PixelFormat {
    pub fn dxgi(self) -> DXGI_FORMAT {
        match self {
            PixelFormat::Nv12 => DXGI_FORMAT_NV12,
            PixelFormat::P010 => DXGI_FORMAT_P010,
        }
    }

    pub(super) fn buffer_format(self) -> nv::NV_ENC_BUFFER_FORMAT {
        match self {
            PixelFormat::Nv12 => nv::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12,
            PixelFormat::P010 => nv::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_YUV420_10BIT,
        }
    }
}

/// A fence value the encoder waits for on the GPU before it reads the texture.
#[derive(Clone)]
pub struct Ready {
    pub fence: ID3D12Fence,
    pub value: u64,
}

pub(super) struct Slot {
    pub(super) resource: ID3D12Resource,
    pub(super) registered: *mut c_void,
    pub(super) mapped: *mut c_void,
}

pub(super) struct PoolShared {
    pub(super) slots: Vec<Slot>,
    free: Mutex<Vec<usize>>,
    released: Condvar,
    format: PixelFormat,
    size: (u32, u32),
}

unsafe impl Send for PoolShared {}
unsafe impl Sync for PoolShared {}

/// NV12 or P010 textures registered with one NVENC session. A convert pass writes a frame into
/// a texture from [`InputPool::acquire`] and hands the [`PoolFrame`] to the encoder.
#[derive(Clone)]
pub struct InputPool {
    pub(super) shared: Arc<PoolShared>,
}

struct Lease {
    shared: Arc<PoolShared>,
    slot: usize,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.shared.lock_free().push(self.slot);
        self.shared.released.notify_one();
    }
}

/// One pool texture. Clones share it, and it goes back to the pool when the last clone drops,
/// which the encoder does for its own once the frame's packet has come out.
#[derive(Clone)]
pub struct PoolFrame {
    lease: Arc<Lease>,
    /// Set after the GPU work that wrote the texture, so the encoder waits for it.
    pub ready: Option<Ready>,
}

fn texture(
    device: &ID3D12Device,
    width: u32,
    height: u32,
    format: PixelFormat,
) -> Result<ID3D12Resource> {
    ensure!(
        width % 2 == 0 && height % 2 == 0,
        "{width}x{height} is not a 4:2:0 size"
    );
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: width as u64,
        Height: height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: format.dxgi(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS
            | D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let mut resource: Option<ID3D12Resource> = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &desc,
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut resource,
        )?;
    }
    resource.context("no texture returned")
}

impl InputPool {
    /// Textures for an encoder that takes D3D12 resources directly and registers nothing.
    pub fn unregistered(
        device: &ID3D12Device,
        width: u32,
        height: u32,
        format: PixelFormat,
        count: usize,
    ) -> Result<InputPool> {
        let slots = (0..count)
            .map(|_| {
                Ok(Slot {
                    resource: texture(device, width, height, format)?,
                    registered: std::ptr::null_mut(),
                    mapped: std::ptr::null_mut(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(InputPool {
            shared: Arc::new(PoolShared {
                free: Mutex::new((0..count).rev().collect()),
                slots,
                released: Condvar::new(),
                format,
                size: (width, height),
            }),
        })
    }

    pub(super) fn new(
        session: &Session,
        device: &ID3D12Device,
        fence: &ID3D12Fence,
        width: u32,
        height: u32,
        format: PixelFormat,
        count: usize,
    ) -> Result<InputPool> {
        let mut point = nv::NV_ENC_FENCE_POINT_D3D12 {
            version: version::NV_ENC_FENCE_POINT_D3D12_VER,
            reserved: 0,
            pFence: fence.as_raw(),
            waitValue: 0,
            signalValue: 0,
            _bitfield_align_1: [],
            _bitfield_1: Default::default(),
            reserved1: [0; 7],
        };
        let mut slots = Vec::with_capacity(count);
        for _ in 0..count {
            let resource = texture(device, width, height, format)?;
            let (registered, mapped) = unsafe {
                let mut register: nv::NV_ENC_REGISTER_RESOURCE = zeroed();
                register.version = version::NV_ENC_REGISTER_RESOURCE_VER;
                register.resourceType =
                    nv::NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
                register.width = width;
                register.height = height;
                register.resourceToRegister = resource.as_raw();
                register.bufferFormat = format.buffer_format();
                register.bufferUsage = nv::NV_ENC_BUFFER_USAGE::NV_ENC_INPUT_IMAGE;
                register.pInputFencePoint = &mut point;
                let _guard = session.guard();
                session.check(
                    (session.functions.nvEncRegisterResource.unwrap())(
                        session.handle,
                        &mut register,
                    ),
                    "registering an input texture",
                )?;
                let mut map: nv::NV_ENC_MAP_INPUT_RESOURCE = zeroed();
                map.version = version::NV_ENC_MAP_INPUT_RESOURCE_VER;
                map.registeredResource = register.registeredResource;
                session.check(
                    (session.functions.nvEncMapInputResource.unwrap())(session.handle, &mut map),
                    "mapping an input texture",
                )?;
                (register.registeredResource, map.mappedResource)
            };
            slots.push(Slot {
                resource,
                registered,
                mapped,
            });
        }
        Ok(InputPool {
            shared: Arc::new(PoolShared {
                free: Mutex::new((0..count).rev().collect()),
                slots,
                released: Condvar::new(),
                format,
                size: (width, height),
            }),
        })
    }

    pub fn format(&self) -> PixelFormat {
        self.shared.format
    }

    pub fn size(&self) -> (u32, u32) {
        self.shared.size
    }

    /// Takes a texture if one is free. The textures are lent out until the encoder has emitted
    /// the packet of the frame they carried.
    pub fn try_acquire(&self) -> Option<PoolFrame> {
        let slot = self.shared.lock_free().pop()?;
        Some(self.lend(slot))
    }

    /// Waits for a free texture. This does not need the caller to drain packets: the encoder
    /// returns textures from its own thread.
    pub fn acquire(&self) -> PoolFrame {
        let mut free = self.shared.lock_free();
        loop {
            if let Some(slot) = free.pop() {
                return self.lend(slot);
            }
            free = self
                .shared
                .released
                .wait(free)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn lend(&self, slot: usize) -> PoolFrame {
        PoolFrame {
            lease: Arc::new(Lease {
                shared: self.shared.clone(),
                slot,
            }),
            ready: None,
        }
    }

    pub fn len(&self) -> usize {
        self.shared.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shared.slots.is_empty()
    }

    pub(super) fn unregister(&self, session: &Session) {
        let _guard = session.guard();
        for slot in &self.shared.slots {
            unsafe {
                (session.functions.nvEncUnmapInputResource.unwrap())(session.handle, slot.mapped);
                (session.functions.nvEncUnregisterResource.unwrap())(
                    session.handle,
                    slot.registered,
                );
            }
        }
    }
}

impl PoolShared {
    fn lock_free(&self) -> std::sync::MutexGuard<'_, Vec<usize>> {
        self.free
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl PoolFrame {
    /// The texture to write. Leave it in the common state: it allows simultaneous access, so
    /// it needs no barrier between the queue that writes it and the encoder.
    pub fn resource(&self) -> &ID3D12Resource {
        &self.lease.shared.slots[self.lease.slot].resource
    }

    pub fn format(&self) -> PixelFormat {
        self.lease.shared.format
    }

    pub fn size(&self) -> (u32, u32) {
        self.lease.shared.size
    }

    /// The index of this texture in its pool. A pool hands out the same textures for the whole
    /// render, so per-texture state can be kept by slot.
    pub fn slot(&self) -> usize {
        self.lease.slot
    }

    pub fn pool_is(&self, pool: &InputPool) -> bool {
        Arc::ptr_eq(&self.lease.shared, &pool.shared)
    }

    pub(super) fn mapped(&self) -> *mut c_void {
        self.lease.shared.slots[self.lease.slot].mapped
    }
}
