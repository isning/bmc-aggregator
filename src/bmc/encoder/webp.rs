use super::backend::FrameEncoder;
use super::error::EncoderError;
use super::Codec;

/// Software WebP encoder using the `image` crate.
pub struct WebpEncoder;

impl FrameEncoder for WebpEncoder {
    fn id(&self) -> &str {
        "webp"
    }

    fn codec(&self) -> Codec {
        Codec::WebP
    }

    fn encode(
        &self,
        pixels: &[u8],
        width: u16,
        height: u16,
        _quality: u8,
    ) -> Result<Vec<u8>, EncoderError> {
        use image::codecs::webp::WebPEncoder;
        use image::ColorType;
        let w = width as u32;
        let h = height as u32;
        let mut buf = Vec::new();
        let encoder = WebPEncoder::new_lossless(&mut buf);
        encoder
            .encode(pixels, w, h, ColorType::Rgba8)
            .map_err(|e| EncoderError::EncodeFailed(e.to_string()))?;
        Ok(buf)
    }
}
