use std::sync::Arc;
use tokio::sync::RwLock;

pub use super::error::EncoderError;
use super::jpeg;
use super::Codec;
use super::registry::EncoderBackend;
use crate::bmc::registry::HasId;

/// Generic frame encoder interface.
/// Every encoder accepts RGBA pixels + dimensions and produces encoded bytes.
pub trait FrameEncoder: Send + Sync {
    fn id(&self) -> &str;
    fn codec(&self) -> Codec;
    fn encode(&self, pixels: &[u8], width: u16, height: u16, quality: u8) -> Result<Vec<u8>, EncoderError>;
}

/// Software JPEG encoder — always available, uses turbojpeg → image fallback.
pub struct SoftwareEncoder;

impl FrameEncoder for SoftwareEncoder {
    fn id(&self) -> &str {
        "software"
    }

    fn codec(&self) -> Codec {
        Codec::Mjpeg
    }

    fn encode(&self, pixels: &[u8], width: u16, height: u16, quality: u8) -> Result<Vec<u8>, EncoderError> {
        jpeg::encode_jpeg(pixels, width, height, quality)
            .map_err(|e| EncoderError::EncodeFailed(e.to_string()))
    }
}

// ── Registry ──────────────────────────────────────────────────────

/// A registered frame encoder backend.
pub struct FrameEncoderBackend {
    pub id: &'static str,
    pub name: &'static str,
    pub is_hardware: bool,
    pub probe: fn() -> bool,
    pub create: fn() -> Box<dyn FrameEncoder>,
}

impl HasId for FrameEncoderBackend {
    fn id(&self) -> &str {
        self.id
    }
}

/// Inventory of all known frame encoder backends.
/// Adding a new backend means adding one entry here.
pub static FRAME_ENCODER_BACKENDS: &[FrameEncoderBackend] = &[
    FrameEncoderBackend {
        id: "software",
        name: "Software",
        is_hardware: false,
        probe: || true,
        create: || Box::new(SoftwareEncoder) as Box<dyn FrameEncoder>,
    },
    FrameEncoderBackend {
        id: "webp",
        name: "WebP",
        is_hardware: false,
        probe: || true,
        create: || Box::new(super::webp::WebpEncoder) as Box<dyn FrameEncoder>,
    },
];

/// Probe whether a given encoder backend ID is available on this system.
pub fn probe_encoder(id: &str) -> EncoderBackend {
    crate::bmc::registry::find_backend(FRAME_ENCODER_BACKENDS, id)
        .map(|b| EncoderBackend {
            id: b.id,
            name: b.name,
            is_hardware: b.is_hardware,
            available: (b.probe)(),
        })
        .unwrap_or(EncoderBackend {
            id: "unknown",
            name: "Unknown",
            is_hardware: false,
            available: false,
        })
}

/// Create a concrete encoder instance for the given backend ID.
/// Falls back to `SoftwareEncoder` if the requested backend is not available.
pub fn create_encoder(id: &str) -> Box<dyn FrameEncoder> {
    crate::bmc::registry::find_backend(FRAME_ENCODER_BACKENDS, id)
        .map(|b| (b.create)())
        .unwrap_or_else(|| {
            tracing::debug!("encoder backend '{}' not available, falling back to software", id);
            Box::new(SoftwareEncoder)
        })
}

/// Thread-safe handle to a swappable frame encoder.
pub type SharedEncoder = Arc<RwLock<Box<dyn FrameEncoder>>>;

/// Create a new `SharedEncoder` initialized with the given backend.
pub fn create_shared_encoder(id: &str) -> SharedEncoder {
    Arc::new(RwLock::new(create_encoder(id)))
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_probe_encoder_software() {
        let p = probe_encoder("software");
        assert_eq!(p.id, "software");
        assert!(p.available);
    }

    #[test]
    fn test_probe_encoder_webp() {
        let p = probe_encoder("webp");
        assert_eq!(p.id, "webp");
        assert!(!p.is_hardware);
        assert!(p.available);
    }

    #[test]
    fn test_probe_encoder_unknown() {
        let p = probe_encoder("vaapi");
        assert_eq!(p.id, "unknown");
        assert!(!p.available);
    }

    #[test]
    fn test_codec_mjpeg() {
        assert_eq!(Codec::from_str("mjpeg"), Some(Codec::Mjpeg));
        assert_eq!(Codec::Mjpeg.as_str(), "mjpeg");
    }

    #[test]
    fn test_codec_webp() {
        assert_eq!(Codec::from_str("webp"), Some(Codec::WebP));
        assert_eq!(Codec::WebP.as_str(), "webp");
    }

    #[test]
    fn test_codec_streaming_not_in_frame() {
        assert_eq!(Codec::from_str("h264"), Some(Codec::H264));
        assert_eq!(Codec::from_str("hevc"), Some(Codec::H265));
        assert_eq!(Codec::from_str("av1"), Some(Codec::AV1));
    }

    #[test]
    fn test_software_encoder() {
        let enc = SoftwareEncoder;
        assert_eq!(enc.id(), "software");
        assert_eq!(enc.codec(), Codec::Mjpeg);
        let rgba = vec![128u8; 16 * 16 * 4];
        let result = enc.encode(&rgba, 16, 16, 75).unwrap();
        assert!(!result.is_empty());
        assert_eq!(result[0], 0xFF);
        assert_eq!(result[1], 0xD8);
    }

    #[test]
    fn test_create_webp_encoder() {
        let enc = create_encoder("webp");
        assert_eq!(enc.id(), "webp");
        assert_eq!(enc.codec().as_str(), "webp");
    }

    #[test]
    fn test_create_encoder_fallback() {
        let enc = create_encoder("nonexistent");
        assert_eq!(enc.id(), "software");
    }

}
