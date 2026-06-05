use super::backend::FRAME_ENCODER_BACKENDS;
use super::CodecEntry;
use crate::bmc::stream::encoder::STREAM_ENCODER_BACKENDS;

#[derive(Debug, Clone)]
pub struct EncoderBackend {
    pub id: &'static str,
    pub name: &'static str,
    pub is_hardware: bool,
    pub available: bool,
}

pub struct EncoderRegistry;

/// Probe whether a stream encoder backend is available at the FFmpeg level.
pub fn probe_stream_encoder(id: &str) -> bool {
    match id {
        "software" => true,
        "nvenc" => {
            let _ = ffmpeg_next::init();
            ffmpeg_next::encoder::find_by_name("h264_nvenc").is_some()
                || ffmpeg_next::encoder::find_by_name("hevc_nvenc").is_some()
                || ffmpeg_next::encoder::find_by_name("av1_nvenc").is_some()
        }
        _ => false,
    }
}

/// Expose whether a stream encoder behaves as hardware-accelerated.
fn stream_encoder_is_hardware(id: &str) -> bool {
    matches!(id, "nvenc")
}

impl EncoderRegistry {
    /// All user-facing encoder backends (frame + stream), with real availability probes.
    /// "webp" is excluded — it's an HTTP codec format, not an independent encoder backend.
    pub fn discover() -> Vec<EncoderBackend> {
        let mut seen = std::collections::HashSet::new();
        let mut backends = Vec::new();

        // Frame encoder backends (HTTP MJPEG/WebP)
        for b in FRAME_ENCODER_BACKENDS.iter() {
            if b.id == "webp" {
                continue;
            }
            if seen.insert(b.id) {
                backends.push(EncoderBackend {
                    id: b.id,
                    name: b.name,
                    is_hardware: b.is_hardware,
                    available: (b.probe)(),
                });
            }
        }

        // Stream encoder backends (WebRTC)
        for b in STREAM_ENCODER_BACKENDS.iter() {
            if seen.insert(b.id) {
                backends.push(EncoderBackend {
                    id: b.id,
                    name: b.name,
                    is_hardware: stream_encoder_is_hardware(b.id),
                    available: probe_stream_encoder(b.id),
                });
            }
        }

        backends
    }

    pub fn codecs() -> Vec<CodecEntry> {
        super::Codec::all_entries()
    }

    pub fn codec_ids() -> Vec<&'static str> {
        Self::codecs().iter().map(|c| c.id).collect()
    }

    pub fn http_mode_ids() -> Vec<&'static str> {
        Self::codecs().iter()
            .filter(|c| c.protocol == "http")
            .map(|c| c.id)
            .collect()
    }

    pub fn find_codec(id: &str) -> Option<CodecEntry> {
        let normalized = super::Codec::from_str(id).map(|c| c.as_str()).unwrap_or(id);
        Self::codecs().into_iter().find(|c| c.id == normalized)
    }
}
