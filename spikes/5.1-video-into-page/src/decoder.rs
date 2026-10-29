use std::ffi::{CString, c_void};
use std::ptr::NonNull;
use std::time::Instant;

use anyhow::{Result, ensure};
use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;

pub const NV12_VIDEO_RANGE: u32 = 0x3432_3076;

type FrameCallback = extern "C" fn(*mut c_void, *mut c_void, i64, i32);

unsafe extern "C" {
    fn vsp_decode(path: *const i8, pixel_format: u32, callback: FrameCallback, ctx: *mut c_void) -> i32;
    fn vsp_surface(buffer: *mut c_void) -> *mut c_void;
    fn CFRetain(object: *const c_void) -> *const c_void;
    fn CFRelease(object: *const c_void);
}

pub struct Frame {
    buffer: NonNull<c_void>,
    pub value: i64,
    pub timescale: i32,
}

unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Frame {
    pub fn surface(&self) -> CFRetained<IOSurfaceRef> {
        let raw = unsafe { vsp_surface(self.buffer.as_ptr()) };
        unsafe { CFRetained::retain(NonNull::new(raw.cast::<IOSurfaceRef>()).expect("no IOSurface")) }
    }

    pub fn seconds(&self) -> f64 {
        self.value as f64 / f64::from(self.timescale)
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { CFRelease(self.buffer.as_ptr()) };
    }
}

extern "C" fn collect(ctx: *mut c_void, buffer: *mut c_void, value: i64, timescale: i32) {
    let frames = unsafe { &mut *ctx.cast::<Vec<Frame>>() };
    unsafe { CFRetain(buffer) };
    frames.push(Frame {
        buffer: NonNull::new(buffer).unwrap(),
        value,
        timescale,
    });
}

pub struct Decoded {
    pub frames: Vec<Frame>,
    pub seconds: f64,
}

pub fn decode(path: &str, pixel_format: u32) -> Result<Decoded> {
    let path = CString::new(path)?;
    let mut frames: Vec<Frame> = Vec::new();
    let started = Instant::now();
    let status = unsafe {
        vsp_decode(path.as_ptr(), pixel_format, collect, (&mut frames as *mut Vec<Frame>).cast())
    };
    ensure!(status == 0, "decode failed with status {status}");
    Ok(Decoded { frames, seconds: started.elapsed().as_secs_f64() })
}
