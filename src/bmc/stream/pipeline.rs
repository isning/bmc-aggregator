use std::time::Instant;

use gstreamer as gst;

pub use bmc_feature_api::video::RawPixelFormat;

#[derive(Clone, Debug)]
pub struct RawFrame {
    /// GstBuffer — broadcast clones via gst_buffer_ref (shares GstMemory, zero data copy).
    pub data: gst::Buffer,
    pub width: u32,
    pub height: u32,
    pub pixel_format: RawPixelFormat,
    pub trace_id: u64,
    pub created_at: Instant,
}

impl RawFrame {
    /// Validate dimensions and buffer length before copying into packed RGBA.
    /// Call on the bounded CPU executor for browser encoding.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let expected = rgba_frame_len(self.width, self.height)?;
        if self.data.size() != expected {
            return Err(format!(
                "raw frame buffer must contain {expected} bytes, got {}",
                self.data.size()
            ));
        }
        Ok(())
    }

    pub(crate) fn to_rgba(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let map = self
            .data
            .map_readable()
            .map_err(|error| format!("raw frame map: {error}"))?;
        let mut pixels = map.as_slice().to_vec();
        if self.pixel_format == RawPixelFormat::Bgra {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        Ok(pixels)
    }

    pub(crate) fn image_dimensions(&self) -> Result<(u16, u16), String> {
        rgba_frame_len(self.width, self.height)?;
        let width = u16::try_from(self.width)
            .map_err(|_| "image width exceeds encoder range".to_string())?;
        let height = u16::try_from(self.height)
            .map_err(|_| "image height exceeds encoder range".to_string())?;
        Ok((width, height))
    }
}

// Bound a single uncompressed frame before allocation or arithmetic.
pub(crate) fn rgba_frame_len(width: u32, height: u32) -> Result<usize, String> {
    const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|size| *size > 0 && *size <= MAX_FRAME_BYTES)
        .ok_or_else(|| format!("invalid or oversized RGBA frame: {width}x{height}"))
}

pub(super) fn frame_time_ns(index: u64, framerate: u32) -> u64 {
    ((index as u128 * 1_000_000_000) / framerate as u128).min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod frame_size_tests {
    use super::*;

    fn frame(pixels: Vec<u8>, width: u32, height: u32, pixel_format: RawPixelFormat) -> RawFrame {
        crate::bmc::stream::gst_init::ensure_gst_init().unwrap();
        RawFrame {
            data: gst::Buffer::from_mut_slice(pixels),
            width,
            height,
            pixel_format,
            trace_id: 0,
            created_at: Instant::now(),
        }
    }

    #[test]
    fn packed_conversion_preserves_rgba_and_normalizes_bgra() {
        assert_eq!(
            frame(vec![1, 2, 3, 4], 1, 1, RawPixelFormat::Rgba)
                .to_rgba()
                .unwrap(),
            [1, 2, 3, 4]
        );
        assert_eq!(
            frame(vec![3, 2, 1, 4], 1, 1, RawPixelFormat::Bgra)
                .to_rgba()
                .unwrap(),
            [1, 2, 3, 4]
        );
    }

    #[test]
    fn packed_conversion_rejects_mismatched_buffers_and_image_truncation() {
        for size in [0, 3, 5] {
            assert!(frame(vec![0; size], 1, 1, RawPixelFormat::Rgba)
                .to_rgba()
                .is_err());
        }
        assert!(frame(vec![], 65536, 1, RawPixelFormat::Rgba)
            .image_dimensions()
            .is_err());
        assert!(frame(vec![], 0, 1, RawPixelFormat::Rgba)
            .image_dimensions()
            .is_err());
    }

    #[test]
    fn rejects_empty_overflowing_and_oversized_frames() {
        assert_eq!(rgba_frame_len(1920, 1080).unwrap(), 8_294_400);
        for (width, height) in [(0, 1), (1, 0), (u32::MAX, u32::MAX), (8192, 8192)] {
            assert!(rgba_frame_len(width, height).is_err());
        }
    }
}
