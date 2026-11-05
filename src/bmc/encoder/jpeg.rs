use tracing::warn;

/// Encode RGBA pixel data as JPEG with the given quality.
/// Tries turbojpeg first, falls back to pure-Rust `image`.
pub fn encode_jpeg(
    pixels: &[u8],
    width: u16,
    height: u16,
    quality: u8,
) -> Result<Vec<u8>, anyhow::Error> {
    let expected = width as usize * height as usize * 4;
    anyhow::ensure!(width > 0 && height > 0, "JPEG dimensions must be nonzero");
    anyhow::ensure!(
        pixels.len() == expected,
        "JPEG pixel buffer must match RGBA dimensions"
    );
    if let Ok(jpeg) = encode_jpeg_turbo(pixels, width, height, quality) {
        return Ok(jpeg);
    }
    warn!("turbojpeg encode failed, falling back to image crate");
    encode_jpeg_image(pixels, width, height, quality)
}

fn encode_jpeg_image(
    pixels: &[u8],
    width: u16,
    height: u16,
    quality: u8,
) -> Result<Vec<u8>, anyhow::Error> {
    use image::codecs::jpeg::JpegEncoder;
    let total = width as usize * height as usize;
    let mut rgb = Vec::with_capacity(total * 3);
    for chunk in pixels.chunks_exact(4) {
        rgb.push(chunk[0]);
        rgb.push(chunk[1]);
        rgb.push(chunk[2]);
    }
    let mut buf = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    encoder.encode(&rgb, width as u32, height as u32, image::ColorType::Rgb8)?;
    Ok(buf)
}

fn encode_jpeg_turbo(
    pixels: &[u8],
    width: u16,
    height: u16,
    quality: u8,
) -> Result<Vec<u8>, anyhow::Error> {
    let mut compressor = turbojpeg::Compressor::new()
        .map_err(|e| anyhow::anyhow!("turbojpeg init failed: {}", e))?;
    compressor
        .set_quality(quality as i32)
        .map_err(|e| anyhow::anyhow!("turbojpeg set_quality failed: {}", e))?;
    compressor
        .set_subsamp(turbojpeg::Subsamp::Sub2x2)
        .map_err(|e| anyhow::anyhow!("turbojpeg set_subsamp failed: {}", e))?;

    let w = width as usize;
    let h = height as usize;
    let image = turbojpeg::Image {
        pixels,
        width: w,
        pitch: w * 4,
        height: h,
        format: turbojpeg::PixelFormat::RGBA,
    };

    compressor
        .compress_to_vec(image)
        .map_err(|e| anyhow::anyhow!("turbojpeg encode failed: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_channels_are_preserved() {
        let pixels = [240, 20, 10, 255].repeat(16 * 16);
        let jpeg = encode_jpeg(&pixels, 16, 16, 95).unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        for pixel in decoded.pixels() {
            for (actual, expected) in pixel.0.into_iter().zip([240u8, 20, 10]) {
                assert!(actual.abs_diff(expected) < 10);
            }
        }
    }

    #[test]
    fn invalid_dimensions_and_buffers_return_errors() {
        for (pixels, width, height) in [
            (vec![], 0, 1),
            (vec![], 1, 0),
            (vec![0; 3], 1, 1),
            (vec![0; 5], 1, 1),
        ] {
            assert!(encode_jpeg(&pixels, width, height, 85).is_err());
        }
    }

    #[test]
    fn test_encode_jpeg_basic() {
        let pixels = vec![128u8; 16 * 16 * 4];
        let jpeg = encode_jpeg(&pixels, 16, 16, 85).unwrap();
        assert!(!jpeg.is_empty());
        assert_eq!(jpeg[0], 0xFF);
        assert_eq!(jpeg[1], 0xD8);
    }

    #[test]
    fn test_encode_jpeg_zero_quality() {
        let pixels = vec![0u8; 8 * 8 * 4];
        let jpeg = encode_jpeg(&pixels, 8, 8, 1).unwrap();
        assert!(!jpeg.is_empty());
        assert_eq!(jpeg[0], 0xFF);
    }

    #[test]
    fn test_encode_jpeg_varied_pixels() {
        let mut pixels = vec![0u8; 4 * 4 * 4];
        for i in 0..4 {
            for j in 0..4 {
                let idx = (i * 4 + j) * 4;
                pixels[idx] = (i * 64) as u8;
                pixels[idx + 1] = (j * 64) as u8;
                pixels[idx + 2] = 128;
                pixels[idx + 3] = 255;
            }
        }
        let jpeg = encode_jpeg(&pixels, 4, 4, 90).unwrap();
        assert!(!jpeg.is_empty());
        assert_eq!(jpeg[0], 0xFF);
    }
}
