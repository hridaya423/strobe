use std::ffi::{CString, c_char, c_void};
use std::path::Path;
use std::ptr::{NonNull, null_mut};
use std::slice;

use anyhow::{Result, bail, ensure};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};

#[repr(C)]
#[derive(Default)]
struct RawInfo {
    width: i32,
    height: i32,
    timescale: i32,
    codec: u32,
    pixel_format: u32,
    matrix: i32,
    alpha: i32,
    profile: i32,
}

unsafe extern "C" {
    fn vrf_clip_open(
        path: *const c_char,
        info: *mut RawInfo,
        table: *mut *mut i64,
        count: *mut usize,
        keys: *mut *mut i64,
        key_count: *mut usize,
        error: *mut c_char,
        error_length: usize,
    ) -> *mut c_void;
    fn vrf_clip_close(clip: *mut c_void);
    fn vrf_reader_open(clip: *mut c_void, from: i64, pixel_format: u32) -> *mut c_void;
    fn vrf_reader_next(reader: *mut c_void, pts: *mut i64) -> *mut CVPixelBuffer;
    fn vrf_reader_close(reader: *mut c_void);
    fn free(pointer: *mut c_void);
}

/// The matrix swscale gets for a frame: BT.709 for BT.709 and untagged sources, BT.601 for every
/// other tag, as Remotion's compositor chooses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    Bt709,
    Bt601,
}

/// One source file: its displayed frames in presentation order, with timestamps in the track's
/// timescale and after the container's edit list, as `ffprobe` reports them.
pub struct Clip {
    handle: NonNull<c_void>,
    pub width: u32,
    pub height: u32,
    pub timescale: i32,
    pub pts: Vec<i64>,
    pub keyframes: Vec<usize>,
    pub matrix: Matrix,
    pub full_range: bool,
    codec: u32,
    pixel_format: u32,
    alpha: bool,
    profile: i32,
}

unsafe impl Send for Clip {}
unsafe impl Sync for Clip {}

impl Clip {
    pub fn open(path: &Path) -> Result<Clip> {
        let path = CString::new(path.as_os_str().as_encoded_bytes())?;
        let mut info = RawInfo::default();
        let mut table = null_mut();
        let mut count = 0;
        let mut keys = null_mut();
        let mut key_count = 0;
        let mut error = [0 as c_char; 256];
        let handle = unsafe {
            vrf_clip_open(
                path.as_ptr(),
                &mut info,
                &mut table,
                &mut count,
                &mut keys,
                &mut key_count,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let Some(handle) = NonNull::new(handle) else {
            let message = unsafe { std::ffi::CStr::from_ptr(error.as_ptr()) };
            bail!("{}", message.to_string_lossy());
        };
        let pts = unsafe { slice::from_raw_parts(table, count) }.to_vec();
        unsafe { free(table.cast()) };
        let key_pts = unsafe { slice::from_raw_parts(keys, key_count) };
        let keyframes = pts
            .iter()
            .enumerate()
            .filter(|(_, pts)| key_pts.binary_search(pts).is_ok())
            .map(|(index, _)| index)
            .collect();
        unsafe { free(keys.cast()) };
        Ok(Clip {
            handle,
            width: info.width as u32,
            height: info.height as u32,
            timescale: info.timescale,
            pts,
            keyframes,
            matrix: if info.matrix == 0 || info.matrix == 1 {
                Matrix::Bt709
            } else {
                Matrix::Bt601
            },
            full_range: info.pixel_format == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            codec: info.codec,
            pixel_format: info.pixel_format,
            alpha: info.alpha != 0,
            profile: info.profile,
        })
    }

    /// Why this clip cannot go through the NV12 path, or `None` if it can: 8-bit 4:2:0 without
    /// alpha is the only layout whose conversion the page shader reproduces. H.264 profiles
    /// 66, 77, 88 and 100 and HEVC Main are that layout; anything else, such as ProRes, 4:2:2,
    /// 4:4:4 or 10-bit, goes to the caller's fallback.
    pub fn refusal(&self) -> Option<String> {
        const AVC1: u32 = u32::from_be_bytes(*b"avc1");
        const HVC1: u32 = u32::from_be_bytes(*b"hvc1");
        const HEV1: u32 = u32::from_be_bytes(*b"hev1");
        if self.alpha {
            return Some("the clip has an alpha channel".into());
        }
        let supported = match self.codec {
            AVC1 => matches!(self.profile, 66 | 77 | 88 | 100),
            HVC1 | HEV1 => matches!(self.profile & 0x1f, 1 | 3),
            _ => false,
        };
        (!supported).then(|| {
            format!(
                "{} profile {} is not 8-bit 4:2:0 H.264 or HEVC",
                String::from_utf8_lossy(&self.codec.to_be_bytes()),
                self.profile
            )
        })
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        unsafe { vrf_clip_close(self.handle.as_ptr()) };
    }
}

/// A decoded NV12 picture. CoreVideo buffers may be used from any thread.
pub struct Frame(CFRetained<CVPixelBuffer>);

unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Frame {
    /// Bytes of a tightly packed copy: the luma plane, then the interleaved chroma plane, whose
    /// rows are `2 * ceil(width / 2)` bytes.
    pub fn nv12_len(width: u32, height: u32) -> usize {
        let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
        width as usize * height as usize + 2 * cw * ch
    }

    pub fn write_nv12(&self, out: &mut [u8]) {
        let buffer = &*self.0;
        unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
        let mut at = 0;
        for plane in 0..2 {
            let height = CVPixelBufferGetHeightOfPlane(buffer, plane);
            let row = CVPixelBufferGetWidthOfPlane(buffer, plane) * (plane + 1);
            let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane);
            let base = CVPixelBufferGetBaseAddressOfPlane(buffer, plane).cast::<u8>();
            for y in 0..height {
                let source = unsafe { slice::from_raw_parts(base.add(y * stride), row) };
                out[at..at + row].copy_from_slice(source);
                at += row;
            }
        }
        unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    }
}

/// Decodes a clip forward from a given frame through `AVAssetReader` (VideoToolbox), producing
/// NV12 in the clip's own range.
pub struct Reader {
    handle: NonNull<c_void>,
    first: usize,
    produced: usize,
}

unsafe impl Send for Reader {}

impl Reader {
    /// Starts decoding at displayed frame `from`.
    pub fn open(clip: &Clip, from: usize) -> Result<Reader> {
        ensure!(from < clip.pts.len(), "frame {from} is past the end");
        let keyframe = clip.keyframes.partition_point(|key| *key <= from).saturating_sub(1);
        let start = clip.pts[clip.keyframes.get(keyframe).copied().unwrap_or(0)];
        let handle = unsafe { vrf_reader_open(clip.handle.as_ptr(), start, clip.pixel_format) };
        let handle = NonNull::new(handle).ok_or_else(|| anyhow::anyhow!("cannot start decoding"))?;
        Ok(Reader {
            handle,
            first: from,
            produced: 0,
        })
    }

    /// The next displayed frame and its index, or `None` at the end of the stream. Fails if the
    /// decoder's timestamps stop agreeing with the clip's table, which would mean a wrong frame.
    pub fn next(&mut self, clip: &Clip) -> Result<Option<(usize, Frame)>> {
        loop {
            let mut pts = 0;
            let Some(buffer) = NonNull::new(unsafe { vrf_reader_next(self.handle.as_ptr(), &mut pts) })
            else {
                return Ok(None);
            };
            let frame = Frame(unsafe { CFRetained::from_raw(buffer) });
            if self.produced == 0 && pts < clip.pts[self.first] {
                continue;
            }
            let index = self.first + self.produced;
            ensure!(
                clip.pts.get(index) == Some(&pts),
                "the decoder gave pts {pts} for frame {index}, the table says {:?}",
                clip.pts.get(index)
            );
            self.produced += 1;
            return Ok(Some((index, frame)));
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        unsafe { vrf_reader_close(self.handle.as_ptr()) };
    }
}
