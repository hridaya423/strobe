#![cfg(target_os = "macos")]

use std::collections::HashSet;
use std::ptr::null_mut;

use objc2_core_foundation::CFString;
use objc2_core_video::{
    kCVImageBufferChromaLocation_Left, kCVImageBufferChromaLocationTopFieldKey,
    kCVImageBufferColorPrimaries_ITU_R_709_2, kCVImageBufferColorPrimariesKey,
    kCVImageBufferTransferFunction_ITU_R_709_2, kCVImageBufferTransferFunctionKey,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVImageBufferYCbCrMatrixKey,
};
use vrf_encode::videotoolbox::pool::{POOL_SIZE, PixelFormat, PixelPool, PoolBuffer};

const NV12_VIDEO_RANGE: u32 = 0x34323076;
const P010_VIDEO_RANGE: u32 = 0x78343230;

fn attachment(buffer: &PoolBuffer, key: &CFString) -> String {
    let value = unsafe { buffer.pixel_buffer().attachment(key, null_mut()) }
        .unwrap_or_else(|| panic!("missing attachment {key}"));
    value.downcast::<CFString>().unwrap().to_string()
}

fn check_planes(format: PixelFormat, pixel_format: u32, luma_bytes: usize) {
    let pool = PixelPool::new(1920, 1080, format).unwrap();
    let buffer = pool.acquire().unwrap();
    let surface = buffer.surface();
    assert_eq!(surface.pixel_format(), pixel_format);
    assert_eq!(surface.plane_count(), 2);
    assert_eq!(buffer.surface_id(), surface.id());

    assert_eq!(surface.width_of_plane(0), 1920);
    assert_eq!(surface.height_of_plane(0), 1080);
    assert_eq!(surface.bytes_per_element_of_plane(0), luma_bytes);
    assert!(surface.bytes_per_row_of_plane(0) >= 1920 * luma_bytes);

    assert_eq!(surface.width_of_plane(1), 960);
    assert_eq!(surface.height_of_plane(1), 540);
    assert_eq!(surface.bytes_per_element_of_plane(1), 2 * luma_bytes);
    assert!(surface.bytes_per_row_of_plane(1) >= 960 * 2 * luma_bytes);
}

#[test]
fn nv12_buffer_has_two_planes_with_the_right_sizes() {
    check_planes(PixelFormat::Nv12, NV12_VIDEO_RANGE, 1);
}

#[test]
fn p010_buffer_has_two_planes_with_the_right_sizes() {
    check_planes(PixelFormat::P010, P010_VIDEO_RANGE, 2);
}

#[test]
fn buffers_are_tagged_bt709_video_range_left_chroma() {
    for format in [PixelFormat::Nv12, PixelFormat::P010] {
        let pool = PixelPool::new(640, 360, format).unwrap();
        let buffer = pool.acquire().unwrap();
        unsafe {
            assert_eq!(
                attachment(&buffer, kCVImageBufferColorPrimariesKey),
                kCVImageBufferColorPrimaries_ITU_R_709_2.to_string()
            );
            assert_eq!(
                attachment(&buffer, kCVImageBufferTransferFunctionKey),
                kCVImageBufferTransferFunction_ITU_R_709_2.to_string()
            );
            assert_eq!(
                attachment(&buffer, kCVImageBufferYCbCrMatrixKey),
                kCVImageBufferYCbCrMatrix_ITU_R_709_2.to_string()
            );
            assert_eq!(
                attachment(&buffer, kCVImageBufferChromaLocationTopFieldKey),
                kCVImageBufferChromaLocation_Left.to_string()
            );
        }
    }
}

#[test]
fn pool_holds_48_stable_surfaces_and_blocks_when_exhausted() {
    let pool = PixelPool::new(640, 360, PixelFormat::Nv12).unwrap();
    let held: Vec<PoolBuffer> = (0..POOL_SIZE).map(|_| pool.acquire().unwrap()).collect();
    assert_eq!(POOL_SIZE, 48);
    let ids: HashSet<u32> = held.iter().map(PoolBuffer::surface_id).collect();
    assert_eq!(ids.len(), POOL_SIZE);
    assert!(pool.try_acquire().unwrap().is_none());

    drop(held);
    let again: HashSet<u32> = (0..POOL_SIZE)
        .map(|_| pool.acquire().unwrap())
        .collect::<Vec<_>>()
        .iter()
        .map(PoolBuffer::surface_id)
        .collect();
    assert_eq!(again, ids);
}
