use std::ptr::{NonNull, null_mut};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, bail, ensure};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{
    CVAttachmentMode, CVPixelBuffer, CVPixelBufferGetIOSurface, CVPixelBufferPool,
    kCVImageBufferChromaLocation_Left, kCVImageBufferChromaLocationBottomFieldKey,
    kCVImageBufferChromaLocationTopFieldKey, kCVImageBufferColorPrimaries_ITU_R_709_2,
    kCVImageBufferColorPrimariesKey, kCVImageBufferTransferFunction_ITU_R_709_2,
    kCVImageBufferTransferFunctionKey, kCVImageBufferYCbCrMatrix_ITU_R_601_4,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVImageBufferYCbCrMatrixKey, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferPoolAllocationThresholdKey,
    kCVPixelBufferPoolMinimumBufferCountKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange, kCVReturnSuccess,
    kCVReturnWouldExceedAllocationThreshold,
};
use objc2_io_surface::IOSurfaceRef;

use crate::{ColourSpace, Matrix, Range};

pub const POOL_SIZE: usize = 48;

const RETRY_INTERVAL: Duration = Duration::from_micros(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    Nv12,
    P010,
}

impl PixelFormat {
    pub(super) fn os_type(self, range: Range) -> u32 {
        match (self, range) {
            (PixelFormat::Nv12, Range::Limited) => kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            (PixelFormat::Nv12, Range::Full) => kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            (PixelFormat::P010, _) => kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
        }
    }
}

type Dict = CFDictionary<CFString, CFType>;

#[derive(Clone)]
pub struct PixelPool {
    pool: CFRetained<CVPixelBufferPool>,
    threshold: CFRetained<Dict>,
    size: usize,
    matrix: Matrix,
}

/// A clone is another reference to the same buffer. The buffer returns to its pool when the
/// last reference drops.
#[derive(Clone)]
pub struct PoolBuffer {
    buffer: CFRetained<CVPixelBuffer>,
    surface: CFRetained<IOSurfaceRef>,
}

// CoreVideo documents pixel buffer pools and pixel buffers as safe to use from any thread.
unsafe impl Send for PixelPool {}
unsafe impl Sync for PixelPool {}
unsafe impl Send for PoolBuffer {}

fn number(value: usize) -> CFRetained<CFNumber> {
    CFNumber::new_i64(value as i64)
}

fn tag(buffer: &CVPixelBuffer, matrix: Matrix) {
    let tags = unsafe {
        [
            (
                kCVImageBufferColorPrimariesKey,
                kCVImageBufferColorPrimaries_ITU_R_709_2,
            ),
            (
                kCVImageBufferTransferFunctionKey,
                kCVImageBufferTransferFunction_ITU_R_709_2,
            ),
            (
                kCVImageBufferYCbCrMatrixKey,
                match matrix {
                    Matrix::Bt601 => kCVImageBufferYCbCrMatrix_ITU_R_601_4,
                    Matrix::Bt709 => kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                },
            ),
            (
                kCVImageBufferChromaLocationTopFieldKey,
                kCVImageBufferChromaLocation_Left,
            ),
            (
                kCVImageBufferChromaLocationBottomFieldKey,
                kCVImageBufferChromaLocation_Left,
            ),
        ]
    };
    for (key, value) in tags {
        unsafe {
            buffer.set_attachment(key, value.as_ref(), CVAttachmentMode::ShouldPropagate);
        }
    }
}

impl PixelPool {
    pub fn new(width: usize, height: usize, format: PixelFormat) -> Result<Self> {
        Self::with_size(width, height, format, POOL_SIZE)
    }

    /// A pool that hands out at most `size` buffers at once.
    pub fn with_size(
        width: usize,
        height: usize,
        format: PixelFormat,
        size: usize,
    ) -> Result<Self> {
        Self::with_colour(width, height, format, size, ColourSpace::BT709)
    }

    pub fn with_colour(
        width: usize,
        height: usize,
        format: PixelFormat,
        size: usize,
        colour: ColourSpace,
    ) -> Result<Self> {
        ensure!(
            format == PixelFormat::Nv12 || colour.range == Range::Limited,
            "full range is for 8-bit buffers only"
        );
        let format_number = CFNumber::new_i32(format.os_type(colour.range) as i32);
        let (width, height) = (number(width), number(height));
        let iosurface_properties = Dict::empty();
        let metal = objc2_core_foundation::CFBoolean::new(true);
        let buffer_attributes = unsafe {
            Dict::from_slices(
                &[
                    kCVPixelBufferPixelFormatTypeKey,
                    kCVPixelBufferWidthKey,
                    kCVPixelBufferHeightKey,
                    kCVPixelBufferIOSurfacePropertiesKey,
                    kCVPixelBufferMetalCompatibilityKey,
                ],
                &[
                    format_number.as_ref(),
                    width.as_ref(),
                    height.as_ref(),
                    iosurface_properties.as_ref(),
                    metal.as_ref(),
                ],
            )
        };
        let count = number(size);
        let pool_attributes = unsafe {
            Dict::from_slices(
                &[kCVPixelBufferPoolMinimumBufferCountKey],
                &[count.as_ref()],
            )
        };
        let mut raw = null_mut();
        let status = unsafe {
            CVPixelBufferPool::create(
                None,
                Some(pool_attributes.as_opaque()),
                Some(buffer_attributes.as_opaque()),
                NonNull::from(&mut raw),
            )
        };
        ensure!(
            status == kCVReturnSuccess,
            "CVPixelBufferPoolCreate: {status}"
        );
        let pool = unsafe {
            CFRetained::from_raw(NonNull::new(raw).expect("pool created without a pointer"))
        };
        let threshold = unsafe {
            Dict::from_slices(
                &[kCVPixelBufferPoolAllocationThresholdKey],
                &[count.as_ref()],
            )
        };
        let this = PixelPool {
            pool,
            threshold,
            size,
            matrix: colour.matrix,
        };
        let warm = (0..size)
            .map(|_| this.acquire())
            .collect::<Result<Vec<_>>>()?;
        drop(warm);
        Ok(this)
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn try_acquire(&self) -> Result<Option<PoolBuffer>> {
        let mut raw = null_mut();
        let status = unsafe {
            CVPixelBufferPool::create_pixel_buffer_with_aux_attributes(
                None,
                &self.pool,
                Some(self.threshold.as_opaque()),
                NonNull::from(&mut raw),
            )
        };
        if status == kCVReturnWouldExceedAllocationThreshold {
            return Ok(None);
        }
        ensure!(
            status == kCVReturnSuccess,
            "CVPixelBufferPoolCreatePixelBuffer: {status}"
        );
        let buffer = unsafe {
            CFRetained::from_raw(NonNull::new(raw).expect("buffer created without a pointer"))
        };
        let Some(surface) = CVPixelBufferGetIOSurface(Some(&buffer)) else {
            bail!("pool buffer has no IOSurface");
        };
        tag(&buffer, self.matrix);
        Ok(Some(PoolBuffer { buffer, surface }))
    }

    pub fn acquire(&self) -> Result<PoolBuffer> {
        loop {
            if let Some(buffer) = self.try_acquire()? {
                return Ok(buffer);
            }
            sleep(RETRY_INTERVAL);
        }
    }
}

impl PoolBuffer {
    pub fn pixel_buffer(&self) -> &CVPixelBuffer {
        &self.buffer
    }

    pub fn surface(&self) -> &IOSurfaceRef {
        &self.surface
    }

    pub fn surface_id(&self) -> u32 {
        self.surface.id()
    }
}
