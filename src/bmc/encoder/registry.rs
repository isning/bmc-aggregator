use super::backend::FRAME_ENCODER_BACKENDS;
use super::{Codec, CodecEntry};
use crate::bmc::stream::codec_config::codec_pipeline_spec;
use crate::bmc::stream::encoder::STREAM_ENCODER_BACKENDS;

#[derive(Debug, Clone)]
pub struct EncoderBackend {
    pub id: &'static str,
    pub name: &'static str,
    pub is_hardware: bool,
    pub available: bool,
}

pub struct EncoderRegistry;

/// Probe whether a stream encoder backend+codec combo is available via GStreamer.
pub fn probe_stream_encoder(id: &str) -> bool {
    probe_stream_encoder_for_codec(id, None)
}

/// Probe whether a specific codec is available on a given backend.
pub fn probe_stream_encoder_for_codec(id: &str, codec: Option<Codec>) -> bool {
    if crate::bmc::stream::gst_init::ensure_gst_init().is_err() {
        return false;
    }
    match (id, codec) {
        ("software", Some(Codec::H264)) | ("software", None) => {
            gstreamer::ElementFactory::find("x264enc").is_some()
        }
        ("software", Some(Codec::H265)) => gstreamer::ElementFactory::find("x265enc").is_some(),
        ("software", Some(Codec::VP8)) => gstreamer::ElementFactory::find("vp8enc").is_some(),
        ("software", Some(Codec::VP9)) => gstreamer::ElementFactory::find("vp9enc").is_some(),
        ("software", Some(Codec::AV1)) => gstreamer::ElementFactory::find("av1enc").is_some(),
        ("nvenc", Some(Codec::H264)) | ("nvenc", None) => {
            gstreamer::ElementFactory::find("nvh264enc").is_some()
        }
        ("nvenc", Some(Codec::H265)) => gstreamer::ElementFactory::find("nvh265enc").is_some(),
        ("nvenc", Some(Codec::AV1)) => gstreamer::ElementFactory::find("nvav1enc").is_some(),
        _ => false,
    }
}

fn has_element(name: &str) -> bool {
    gstreamer::ElementFactory::find(name).is_some()
}

fn stream_encoder_element_for_codec(backend: &str, codec: Codec) -> Option<&'static str> {
    let spec = codec_pipeline_spec(codec).ok()?;
    match backend {
        "software" => Some(spec.encoder_name),
        "nvenc" => spec.nv_encoder_name,
        _ => None,
    }
}

pub fn probe_webtransport_codec_for_backend(backend: &str, codec: Codec) -> bool {
    if crate::bmc::stream::gst_init::ensure_gst_init().is_err() {
        return false;
    }
    let Some(encoder) = stream_encoder_element_for_codec(backend, codec) else {
        return false;
    };
    if !has_element(encoder) {
        return false;
    }
    let Ok(spec) = codec_pipeline_spec(codec) else {
        return false;
    };
    spec.parser.map(has_element).unwrap_or(true)
}

pub fn probe_webrtc_codec_for_backend(backend: &str, codec: Codec) -> bool {
    if crate::bmc::stream::gst_init::ensure_gst_init().is_err() {
        return false;
    }
    let Ok(spec) = codec_pipeline_spec(codec) else {
        return false;
    };
    probe_webtransport_codec_for_backend(backend, codec) && has_element(spec.payloader)
}

pub fn codec_available_for_webrtc(codec: Codec) -> bool {
    probe_webrtc_codec_for_backend("software", codec)
        || probe_webrtc_codec_for_backend("nvenc", codec)
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

    pub fn http_mode_ids() -> Vec<&'static str> {
        Self::codecs()
            .iter()
            .filter(|c| c.protocol == "http")
            .map(|c| c.id)
            .collect()
    }

    pub fn find_codec(id: &str) -> Option<CodecEntry> {
        let normalized = super::Codec::from_str(id).map(|c| c.as_str()).unwrap_or(id);
        Self::codecs().into_iter().find(|c| c.id == normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webrtc_codec_probe_requires_payloader() {
        crate::bmc::stream::gst_init::ensure_gst_init().expect("initialize GStreamer");
        for codec in [Codec::H264, Codec::H265, Codec::VP8, Codec::VP9, Codec::AV1] {
            let available = codec_available_for_webrtc(codec);
            if available {
                let spec = codec_pipeline_spec(codec).unwrap();
                assert!(
                    gstreamer::ElementFactory::find(spec.payloader).is_some(),
                    "{} marked WebRTC-available without {}",
                    codec.as_str(),
                    spec.payloader
                );
            }
        }
    }

    #[test]
    fn webtransport_codec_probe_does_not_require_rtp_payloader() {
        crate::bmc::stream::gst_init::ensure_gst_init().expect("initialize GStreamer");
        if gstreamer::ElementFactory::find("av1enc").is_some()
            && gstreamer::ElementFactory::find("av1parse").is_some()
        {
            assert!(probe_webtransport_codec_for_backend("software", Codec::AV1));
        }
    }
}
