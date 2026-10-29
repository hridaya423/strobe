use std::ffi::{CStr, c_void};
use std::mem::MaybeUninit;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result, bail, ensure};
use libloading::Library;
use nvidia_video_codec_sdk::sys::nvEncodeAPI as nv;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::Interface;

use super::version;

pub(super) unsafe fn zeroed<T>() -> T {
    unsafe { MaybeUninit::<T>::zeroed().assume_init() }
}

pub(super) struct Session {
    _library: Library,
    pub(super) functions: nv::NV_ENCODE_API_FUNCTION_LIST,
    pub(super) handle: *mut c_void,
    pub(super) api_version: u32,
    lock: Mutex<()>,
}

unsafe impl Send for Session {}
unsafe impl Sync for Session {}

impl Session {
    pub(super) fn open(device: &ID3D12Device) -> Result<Session> {
        let library = unsafe { Library::new("nvEncodeAPI64.dll") }
            .context("nvEncodeAPI64.dll did not load; is an NVIDIA driver installed?")?;
        let mut api_version = 0u32;
        let mut functions: nv::NV_ENCODE_API_FUNCTION_LIST = unsafe { zeroed() };
        unsafe {
            let max_supported: libloading::Symbol<
                unsafe extern "C" fn(*mut u32) -> nv::NVENCSTATUS,
            > = library.get(b"NvEncodeAPIGetMaxSupportedVersion")?;
            ensure!(
                max_supported(&mut api_version) == nv::NVENCSTATUS::NV_ENC_SUCCESS,
                "NvEncodeAPIGetMaxSupportedVersion failed"
            );
            let wanted = nv::NVENCAPI_MAJOR_VERSION << 4 | nv::NVENCAPI_MINOR_VERSION;
            ensure!(
                api_version >= wanted,
                "the driver supports NVENC API {}.{}, this build needs {}.{}",
                api_version >> 4,
                api_version & 0xf,
                nv::NVENCAPI_MAJOR_VERSION,
                nv::NVENCAPI_MINOR_VERSION
            );
            let create: libloading::Symbol<
                unsafe extern "C" fn(*mut nv::NV_ENCODE_API_FUNCTION_LIST) -> nv::NVENCSTATUS,
            > = library.get(b"NvEncodeAPICreateInstance")?;
            functions.version = version::NV_ENCODE_API_FUNCTION_LIST_VER;
            ensure!(
                create(&mut functions) == nv::NVENCSTATUS::NV_ENC_SUCCESS,
                "NvEncodeAPICreateInstance failed"
            );
        }
        let mut session = Session {
            _library: library,
            functions,
            handle: std::ptr::null_mut(),
            api_version,
            lock: Mutex::new(()),
        };
        let mut params: nv::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = unsafe { zeroed() };
        params.version = version::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
        params.deviceType = nv::NV_ENC_DEVICE_TYPE::NV_ENC_DEVICE_TYPE_DIRECTX;
        params.device = device.as_raw();
        params.apiVersion = nv::NVENCAPI_VERSION;
        let status = unsafe {
            (session.functions.nvEncOpenEncodeSessionEx.unwrap())(&mut params, &mut session.handle)
        };
        session.check(status, "opening an NVENC session on the D3D12 device")?;
        Ok(session)
    }

    pub(super) fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn check(&self, status: nv::NVENCSTATUS, what: &str) -> Result<()> {
        if status == nv::NVENCSTATUS::NV_ENC_SUCCESS {
            return Ok(());
        }
        let detail = unsafe {
            let message = (self.functions.nvEncGetLastErrorString.unwrap())(self.handle);
            if message.is_null() {
                String::new()
            } else {
                CStr::from_ptr(message).to_string_lossy().into_owned()
            }
        };
        bail!("{what}: {status:?} {detail}");
    }

    pub(super) fn guids(&self) -> Result<Vec<nv::GUID>> {
        let mut count = 0u32;
        let mut guids = Vec::new();
        let _guard = self.guard();
        unsafe {
            self.check(
                (self.functions.nvEncGetEncodeGUIDCount.unwrap())(self.handle, &mut count),
                "counting codecs",
            )?;
            guids.resize(count as usize, zeroed::<nv::GUID>());
            self.check(
                (self.functions.nvEncGetEncodeGUIDs.unwrap())(
                    self.handle,
                    guids.as_mut_ptr(),
                    count,
                    &mut count,
                ),
                "listing codecs",
            )?;
        }
        guids.truncate(count as usize);
        Ok(guids)
    }

    pub(super) fn cap(&self, codec: nv::GUID, cap: nv::NV_ENC_CAPS) -> Result<i32> {
        let mut param: nv::NV_ENC_CAPS_PARAM = unsafe { zeroed() };
        param.version = version::NV_ENC_CAPS_PARAM_VER;
        param.capsToQuery = cap;
        let mut value = 0i32;
        let _guard = self.guard();
        self.check(
            unsafe {
                (self.functions.nvEncGetEncodeCaps.unwrap())(
                    self.handle,
                    codec,
                    &mut param,
                    &mut value,
                )
            },
            &format!("querying {cap:?}"),
        )?;
        Ok(value)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { (self.functions.nvEncDestroyEncoder.unwrap())(self.handle) };
    }
}

pub(super) struct Event(HANDLE);

unsafe impl Send for Event {}

impl Event {
    pub(super) fn new() -> Result<Event> {
        Ok(Event(unsafe { CreateEventW(None, false, false, None)? }))
    }

    pub(super) fn wait(&self, fence: &ID3D12Fence, value: u64) -> Result<()> {
        unsafe {
            if fence.GetCompletedValue() < value {
                fence.SetEventOnCompletion(value, self.0)?;
                WaitForSingleObject(self.0, INFINITE);
            }
        }
        Ok(())
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
