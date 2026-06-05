/// Encode RGBA pixel data as JPEG with the given quality.
/// Tries turbojpeg first, falls back to pure-Rust `image`.
pub fn encode_jpeg(pixels: &[u8], width: u16, height: u16, quality: u8) -> Result<Vec<u8>, anyhow::Error> {
    if let Ok(jpeg) = encode_jpeg_turbo(pixels, width, height, quality) {
        return Ok(jpeg);
    }
    encode_jpeg_image(pixels, width, height, quality)
}

fn encode_jpeg_image(pixels: &[u8], width: u16, height: u16, quality: u8) -> Result<Vec<u8>, anyhow::Error> {
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

fn encode_jpeg_turbo(pixels: &[u8], width: u16, height: u16, quality: u8) -> Result<Vec<u8>, anyhow::Error> {
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
    let mut bgra = Vec::with_capacity(w * h * 4);
    for chunk in pixels.chunks_exact(4) {
        bgra.push(chunk[2]);
        bgra.push(chunk[1]);
        bgra.push(chunk[0]);
        bgra.push(255);
    }

    let image = turbojpeg::Image {
        pixels: bgra.as_slice(),
        width: w,
        pitch: w * 4,
        height: h,
        format: turbojpeg::PixelFormat::BGRA,
    };

    compressor
        .compress_to_vec(image)
        .map_err(|e| anyhow::anyhow!("turbojpeg encode failed: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

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
