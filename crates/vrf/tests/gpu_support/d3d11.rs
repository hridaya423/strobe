use std::ffi::c_void;

use vrf::gpu::d3d12::Luid;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_QUERY_DESC, D3D11_QUERY_EVENT, D3D11_RESOURCE_MISC_SHARED,
    D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter,
    IDXGIFactory4, IDXGIResource1,
};
use windows::core::{BOOL, Interface, PCWSTR};

pub struct D3d11 {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
}

pub struct SharedTexture {
    texture: ID3D11Texture2D,
    target: ID3D11RenderTargetView,
    handle: HANDLE,
}

impl D3d11 {
    pub fn new(luid: Luid) -> Self {
        let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory1() }.unwrap();
        let adapter: IDXGIAdapter = unsafe {
            factory.EnumAdapterByLuid::<windows::Win32::Graphics::Dxgi::IDXGIAdapter1>(LUID {
                LowPart: luid.low,
                HighPart: luid.high,
            })
        }
        .unwrap()
        .cast()
        .unwrap();
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .unwrap();
        Self {
            device: device.unwrap(),
            context: context.unwrap(),
        }
    }

    pub fn shared_texture(&self, width: u32, height: u32) -> SharedTexture {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
                as u32,
        };
        let mut texture = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }.unwrap();
        let texture = texture.unwrap();
        let mut target = None;
        unsafe {
            self.device
                .CreateRenderTargetView(&texture, None, Some(&mut target))
        }
        .unwrap();
        let handle = unsafe {
            texture
                .cast::<IDXGIResource1>()
                .unwrap()
                .CreateSharedHandle(
                    None,
                    DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                    PCWSTR::null(),
                )
        }
        .unwrap();
        SharedTexture {
            texture,
            target: target.unwrap(),
            handle,
        }
    }

    pub fn write(&self, texture: &SharedTexture, bgra: &[u8]) {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.texture.GetDesc(&mut desc) };
        assert_eq!(bgra.len(), (desc.Width * desc.Height * 4) as usize);
        unsafe {
            self.context.UpdateSubresource(
                &texture.texture,
                0,
                None,
                bgra.as_ptr().cast(),
                desc.Width * 4,
                0,
            );
        }
        self.finish();
    }

    pub fn clear(&self, texture: &SharedTexture, bgra: [u8; 4]) {
        let [b, g, r, a] = bgra.map(|c| f32::from(c) / 255.0);
        unsafe {
            self.context
                .ClearRenderTargetView(&texture.target, &[r, g, b, a]);
        }
        self.finish();
    }

    fn finish(&self) {
        let mut query = None;
        unsafe {
            self.device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut query),
            )
        }
        .unwrap();
        let query = query.unwrap();
        unsafe { self.context.End(&query) };
        let mut done = BOOL(0);
        while !done.as_bool() {
            unsafe {
                self.context.GetData(
                    &query,
                    Some((&raw mut done).cast()),
                    size_of::<BOOL>() as u32,
                    0,
                )
            }
            .unwrap();
        }
    }
}

impl SharedTexture {
    pub fn handle(&self) -> *mut c_void {
        self.handle.0
    }
}

impl Drop for SharedTexture {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}
