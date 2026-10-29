use std::ptr::NonNull;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetIOSurface, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey,
};
use objc2_io_surface::IOSurfaceRef;

pub const BGRA: u32 = 0x42475241;
pub const NV12: u32 = 0x34323076;
pub const P010: u32 = 0x78343230;

pub struct Surface {
    pub buffer: CFRetained<CVPixelBuffer>,
    pub surface: CFRetained<IOSurfaceRef>,
}

impl Surface {
    pub fn new(width: usize, height: usize, pixel_format: u32) -> Self {
        let empty = CFDictionary::<CFType, CFType>::empty();
        let yes = CFBoolean::new(true);
        let attrs = unsafe {
            CFDictionary::<CFType, CFType>::from_slices(
                &[
                    kCVPixelBufferIOSurfacePropertiesKey.as_ref(),
                    kCVPixelBufferMetalCompatibilityKey.as_ref(),
                ],
                &[empty.as_ref(), yes.as_ref()],
            )
        };
        let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                pixel_format,
                Some(attrs.as_opaque()),
                NonNull::from(&mut out),
            )
        };
        assert_eq!(status, 0);
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(out).unwrap()) };
        let surface = CVPixelBufferGetIOSurface(Some(&buffer)).unwrap();
        Self { buffer, surface }
    }

    pub fn with_locked<R>(&self, f: impl FnOnce() -> R) -> R {
        let flags = CVPixelBufferLockFlags(0);
        unsafe { CVPixelBufferLockBaseAddress(&self.buffer, flags) };
        let r = f();
        unsafe { CVPixelBufferUnlockBaseAddress(&self.buffer, flags) };
        r
    }

    pub fn write_plane(&self, plane: usize, row_bytes: usize, rows: usize, data: &[u8]) {
        self.with_locked(|| {
            let base = self
                .surface
                .base_address_of_plane(plane)
                .as_ptr()
                .cast::<u8>();
            let stride = self.surface.bytes_per_row_of_plane(plane);
            for y in 0..rows {
                let src = &data[y * row_bytes..(y + 1) * row_bytes];
                unsafe {
                    std::ptr::copy_nonoverlapping(src.as_ptr(), base.add(y * stride), row_bytes)
                };
            }
        });
    }

    pub fn read_plane(&self, plane: usize, row_bytes: usize, rows: usize) -> Vec<u8> {
        self.with_locked(|| {
            let base = self
                .surface
                .base_address_of_plane(plane)
                .as_ptr()
                .cast::<u8>();
            let stride = self.surface.bytes_per_row_of_plane(plane);
            let mut out = Vec::with_capacity(row_bytes * rows);
            for y in 0..rows {
                out.extend_from_slice(unsafe {
                    std::slice::from_raw_parts(base.add(y * stride), row_bytes)
                });
            }
            out
        })
    }
}
