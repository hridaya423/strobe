use std::cell::Cell;
use std::time::Instant;

use cef::AcceleratedPaintInfo;
use cef::osr_texture_import::SharedTextureHandle;
use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, WaitForSingleObject,
};
use windows::core::Interface;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub width: u32,
    pub height: u32,
    raw: Raw,
}

struct Raw {
    queue: ID3D12CommandQueue,
    alloc: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    event: HANDLE,
    value: Cell<u64>,
    spin: bool,
    owned: ID3D12Resource,
    readback: ID3D12Resource,
}

pub struct RawTimings {
    pub open_us: f64,
    pub submit_us: f64,
    pub wait_us: f64,
    pub release_us: f64,
    pub handle: usize,
}

impl Gpu {
    pub fn new(width: u32, height: u32, pick: Option<&str>, spin: bool) -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::DX12));
        for a in &adapters {
            let i = a.get_info();
            let luid = unsafe { a.as_hal::<Dx12>() }
                .map(|h| unsafe { h.as_raw().GetDesc().unwrap().AdapterLuid })
                .map(|l| format!("{:#x}:{:#x}", l.HighPart, l.LowPart))
                .unwrap_or_default();
            println!(
                "adapter: {} vendor={:#x} device={:#x} type={:?} luid={luid}",
                i.name, i.vendor, i.device, i.device_type
            );
        }
        let adapter = match pick {
            Some(p) => adapters
                .into_iter()
                .find(|a| a.get_info().name.to_lowercase().contains(&p.to_lowercase()))
                .expect("no adapter matches --adapter"),
            None => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            }))
            .expect("no DX12 adapter"),
        };
        println!("using adapter: {}", adapter.get_info().name);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("request_device");
        let raw = Raw::new(&device, width, height, spin);
        Gpu {
            device,
            queue,
            width,
            height,
            raw,
        }
    }

    pub fn import_cef(&self, info: &AcceleratedPaintInfo) -> Result<wgpu::Texture, String> {
        let handle = SharedTextureHandle::new(info);
        unsafe { handle.import_texture(&self.device, &self.queue) }.map_err(|e| e.to_string())
    }

    pub fn duplicate(&self, info: &AcceleratedPaintInfo) -> Result<HANDLE, String> {
        let mut dup = HANDLE::default();
        unsafe {
            let me = GetCurrentProcess();
            DuplicateHandle(
                me,
                HANDLE(info.shared_texture_handle),
                me,
                &mut dup,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .map_err(|e| format!("DuplicateHandle: {e}"))?;
        Ok(dup)
    }

    pub fn close(&self, h: HANDLE) {
        unsafe {
            let _ = CloseHandle(h);
        }
    }

    pub fn import_raw(&self, handle: HANDLE) -> Result<RawTimings, String> {
        let hal = unsafe { self.device.as_hal::<Dx12>() }.ok_or("not a DX12 device")?;
        let t0 = Instant::now();
        let mut src: Option<ID3D12Resource> = None;
        unsafe { hal.raw_device().OpenSharedHandle(handle, &mut src) }
            .map_err(|e| format!("OpenSharedHandle: {e}"))?;
        let src = src.ok_or("OpenSharedHandle returned null")?;
        let t1 = Instant::now();
        let (submit_us, wait_us) = self.raw.copy_from(&src);
        let t2 = Instant::now();
        drop(src);
        let t3 = Instant::now();
        Ok(RawTimings {
            open_us: (t1 - t0).as_secs_f64() * 1e6,
            submit_us,
            wait_us,
            release_us: (t3 - t2).as_secs_f64() * 1e6,
            handle: handle.0 as usize,
        })
    }

    pub fn centre_raw(&self) -> [u8; 4] {
        self.raw.centre(self.width / 2, self.height / 2)
    }

    pub fn centre_wgpu(&self, tex: &wgpu::Texture) -> [u8; 4] {
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: self.width / 2,
                    y: self.height / 2,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .unwrap();
        let data = buf.slice(..).get_mapped_range().unwrap();
        [data[0], data[1], data[2], data[3]]
    }
}

impl Raw {
    fn new(device: &wgpu::Device, width: u32, height: u32, spin: bool) -> Self {
        let hal = unsafe { device.as_hal::<Dx12>() }.expect("not a DX12 device");
        let dev = hal.raw_device();
        unsafe {
            let queue: ID3D12CommandQueue = dev
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    ..Default::default()
                })
                .unwrap();
            let alloc: ID3D12CommandAllocator = dev
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                .unwrap();
            let list: ID3D12GraphicsCommandList = dev
                .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &alloc, None)
                .unwrap();
            list.Close().unwrap();
            let fence: ID3D12Fence = dev.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
            let event = CreateEventW(None, false, false, None).unwrap();

            let default_heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            };
            let mut owned: Option<ID3D12Resource> = None;
            dev.CreateCommittedResource(
                &default_heap,
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: width as u64,
                    Height: height,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                    Flags: D3D12_RESOURCE_FLAG_NONE,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut owned,
            )
            .unwrap();

            let readback_heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_READBACK,
                ..Default::default()
            };
            let mut readback: Option<ID3D12Resource> = None;
            dev.CreateCommittedResource(
                &readback_heap,
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Width: 256,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut readback,
            )
            .unwrap();

            Raw {
                queue,
                alloc,
                list,
                fence,
                event,
                value: Cell::new(0),
                spin,
                owned: owned.unwrap(),
                readback: readback.unwrap(),
            }
        }
    }

    unsafe fn submit_and_wait(&self) -> (f64, f64) {
        unsafe {
            let t0 = Instant::now();
            self.list.Close().unwrap();
            self.queue
                .ExecuteCommandLists(&[Some(self.list.cast::<ID3D12CommandList>().unwrap())]);
            let v = self.value.get() + 1;
            self.value.set(v);
            self.queue.Signal(&self.fence, v).unwrap();
            let t1 = Instant::now();
            if self.spin {
                while self.fence.GetCompletedValue() < v {
                    std::hint::spin_loop();
                }
            } else if self.fence.GetCompletedValue() < v {
                self.fence.SetEventOnCompletion(v, self.event).unwrap();
                WaitForSingleObject(self.event, INFINITE);
            }
            (
                (t1 - t0).as_secs_f64() * 1e6,
                t1.elapsed().as_secs_f64() * 1e6,
            )
        }
    }

    fn copy_from(&self, src: &ID3D12Resource) -> (f64, f64) {
        unsafe {
            self.alloc.Reset().unwrap();
            self.list.Reset(&self.alloc, None).unwrap();
            self.list.CopyResource(&self.owned, src);
            self.submit_and_wait()
        }
    }

    fn centre(&self, x: u32, y: u32) -> [u8; 4] {
        unsafe {
            self.alloc.Reset().unwrap();
            self.list.Reset(&self.alloc, None).unwrap();
            let barrier = |from, to| D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: std::mem::ManuallyDrop::new(Some(self.owned.clone())),
                        Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: from,
                        StateAfter: to,
                    }),
                },
            };
            self.list.ResourceBarrier(&[barrier(
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            let dst = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::ManuallyDrop::new(Some(self.readback.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                        Offset: 0,
                        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            Width: 1,
                            Height: 1,
                            Depth: 1,
                            RowPitch: 256,
                        },
                    },
                },
            };
            let src = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::ManuallyDrop::new(Some(self.owned.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            self.list.CopyTextureRegion(
                &dst,
                0,
                0,
                0,
                &src,
                Some(&D3D12_BOX {
                    left: x,
                    top: y,
                    front: 0,
                    right: x + 1,
                    bottom: y + 1,
                    back: 1,
                }),
            );
            self.list.ResourceBarrier(&[barrier(
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COPY_DEST,
            )]);
            self.submit_and_wait();
            let mut p = std::ptr::null_mut();
            self.readback.Map(0, None, Some(&mut p)).unwrap();
            let px = std::slice::from_raw_parts(p as *const u8, 4);
            let out = [px[0], px[1], px[2], px[3]];
            self.readback.Unmap(0, None);
            out
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.event);
        }
    }
}
