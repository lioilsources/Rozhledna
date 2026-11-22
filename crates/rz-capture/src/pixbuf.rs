//! `CVPixelBuffer` helpers (macOS).

use std::ptr::NonNull;

use anyhow::{Result, bail};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVReturnSuccess,
};

use crate::Nv12Image;

/// A retained `CVPixelBuffer` that may cross threads.
///
/// SAFETY: CoreVideo buffers are reference counted atomically and safe to pass between
/// threads; we never mutate one concurrently (writes only happen before it is shared).
pub struct PixelBuffer(pub CFRetained<CVPixelBuffer>);
unsafe impl Send for PixelBuffer {}
unsafe impl Sync for PixelBuffer {}

impl Clone for PixelBuffer {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// Attributes for IOSurface-backed, Metal-compatible buffers.
pub fn iosurface_attributes(pixel_format: Option<u32>) -> CFRetained<CFDictionary<CFString, CFType>> {
    let empty = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
    let yes = objc2_core_foundation::CFBoolean::new(true);
    let mut keys: Vec<&CFString> =
        unsafe { vec![kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey] };
    let mut vals: Vec<&CFType> = vec![(*empty).as_ref(), yes.as_ref()];
    let fmt;
    if let Some(f) = pixel_format {
        fmt = CFNumber::new_i32(f as i32);
        keys.push(unsafe { objc2_core_video::kCVPixelBufferPixelFormatTypeKey });
        vals.push(&fmt);
    }
    CFDictionary::<CFString, CFType>::from_slices(&keys, &vals)
}

impl PixelBuffer {
    pub fn cv(&self) -> &CVPixelBuffer {
        &self.0
    }

    pub fn width(&self) -> u32 {
        CVPixelBufferGetWidth(&self.0) as u32
    }

    pub fn height(&self) -> u32 {
        CVPixelBufferGetHeight(&self.0) as u32
    }

    pub fn pixel_format(&self) -> u32 {
        CVPixelBufferGetPixelFormatType(&self.0)
    }

    pub fn new(width: u32, height: u32, format: u32) -> Result<Self> {
        let attrs = iosurface_attributes(None);
        let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
        let r = unsafe {
            CVPixelBufferCreate(
                None,
                width as usize,
                height as usize,
                format,
                Some(attrs.as_opaque()),
                NonNull::from(&mut out),
            )
        };
        if r != kCVReturnSuccess || out.is_null() {
            bail!("CVPixelBufferCreate failed: {r}");
        }
        Ok(Self(unsafe { CFRetained::from_raw(NonNull::new_unchecked(out)) }))
    }

    /// Copies a CPU NV12 image into a new IOSurface-backed 420v buffer.
    pub fn from_nv12(img: &Nv12Image) -> Result<Self> {
        let pb = Self::new(img.width, img.height, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)?;
        unsafe {
            CVPixelBufferLockBaseAddress(&pb.0, CVPixelBufferLockFlags(0));
            for (plane, src, rows) in [(0usize, &img.y, img.height), (1, &img.uv, img.height / 2)] {
                let dst = CVPixelBufferGetBaseAddressOfPlane(&pb.0, plane) as *mut u8;
                let stride = CVPixelBufferGetBytesPerRowOfPlane(&pb.0, plane);
                let w = img.width as usize;
                for r in 0..rows as usize {
                    std::ptr::copy_nonoverlapping(src.as_ptr().add(r * w), dst.add(r * stride), w);
                }
            }
            CVPixelBufferUnlockBaseAddress(&pb.0, CVPixelBufferLockFlags(0));
        }
        Ok(pb)
    }

    /// Calls `f(luma(x,y))` with read access. Works for BGRA (uses G) and 420v/420f (Y plane).
    pub fn with_luma<R>(&self, f: impl FnOnce(&dyn Fn(u32, u32) -> u8) -> R) -> R {
        const READ_ONLY: CVPixelBufferLockFlags = CVPixelBufferLockFlags(1);
        unsafe {
            CVPixelBufferLockBaseAddress(&self.0, READ_ONLY);
            let fmt = self.pixel_format();
            let r = if fmt == kCVPixelFormatType_32BGRA {
                let base = CVPixelBufferGetBaseAddress(&self.0) as *const u8;
                let stride = CVPixelBufferGetBytesPerRow(&self.0);
                f(&|x, y| *base.add(y as usize * stride + x as usize * 4 + 1))
            } else {
                let base = CVPixelBufferGetBaseAddressOfPlane(&self.0, 0) as *const u8;
                let stride = CVPixelBufferGetBytesPerRowOfPlane(&self.0, 0);
                f(&|x, y| *base.add(y as usize * stride + x as usize))
            };
            CVPixelBufferUnlockBaseAddress(&self.0, READ_ONLY);
            r
        }
    }

    /// Calls `f(row)` for each row of a BGRA buffer (row slices include stride padding).
    pub fn with_bgra_rows(&self, mut f: impl FnMut(&[u8])) {
        const READ_ONLY: CVPixelBufferLockFlags = CVPixelBufferLockFlags(1);
        assert_eq!(self.pixel_format(), kCVPixelFormatType_32BGRA, "BGRA buffer expected");
        unsafe {
            CVPixelBufferLockBaseAddress(&self.0, READ_ONLY);
            let base = CVPixelBufferGetBaseAddress(&self.0) as *const u8;
            let stride = CVPixelBufferGetBytesPerRow(&self.0);
            for y in 0..self.height() as usize {
                f(std::slice::from_raw_parts(base.add(y * stride), stride));
            }
            CVPixelBufferUnlockBaseAddress(&self.0, READ_ONLY);
        }
    }
}
