pub struct CodecEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub protocol: &'static str,
    pub mime: &'static str,
}

/// Return the codecs that a given encoder backend can handle.
/// Codecs can appear under multiple backends (e.g. h264 under software AND nvenc).
pub fn codecs_for_backend(backend_id: &str) -> Vec<&'static str> {
    match backend_id {
        "software" => vec!["mjpeg", "webp", "h264", "h265", "vp8", "vp9", "av1"],
        "nvenc" => vec!["h264", "h265", "av1"],
        _ => Vec::new(),
    }
}

/// Unified codec type covering both intra-frame (single-image) and inter-frame (streaming) codecs.
///
/// This is the single interface that callers use. Internal modules (`StreamCodec`, `FrameCodec`)
/// are private to their respective domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    // Streaming (inter-frame) codecs — used by FfmpegEncoder for WebRTC
    H264,
    H265,
    VP8,
    VP9,
    AV1,
    // Frame (intra-frame) codecs — used by FrameEncoder for HTTP MJPEG
    Mjpeg,
    WebP,
}

impl Codec {
    #[allow(clippy::should_implement_trait)] // Existing API returns Option and is used by config/UI parsing.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "h264" => Some(Self::H264),
            "h265" | "hevc" => Some(Self::H265),
            "vp8" => Some(Self::VP8),
            "vp9" => Some(Self::VP9),
            "av1" => Some(Self::AV1),
            "mjpeg" => Some(Self::Mjpeg),
            "webp" => Some(Self::WebP),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
            Self::VP8 => "vp8",
            Self::VP9 => "vp9",
            Self::AV1 => "av1",
            Self::Mjpeg => "mjpeg",
            Self::WebP => "webp",
        }
    }

    pub fn supports_webrtc(&self) -> bool {
        matches!(
            self,
            Self::H264 | Self::H265 | Self::VP8 | Self::VP9 | Self::AV1
        )
    }

    pub fn entry(&self) -> CodecEntry {
        match self {
            Self::H264 => CodecEntry {
                id: "h264",
                name: "H.264",
                protocol: "webrtc",
                mime: "",
            },
            Self::H265 => CodecEntry {
                id: "h265",
                name: "H.265",
                protocol: "webrtc",
                mime: "",
            },
            Self::VP8 => CodecEntry {
                id: "vp8",
                name: "VP8",
                protocol: "webrtc",
                mime: "",
            },
            Self::VP9 => CodecEntry {
                id: "vp9",
                name: "VP9",
                protocol: "webrtc",
                mime: "",
            },
            Self::AV1 => CodecEntry {
                id: "av1",
                name: "AV1",
                protocol: "webrtc",
                mime: "",
            },
            Self::Mjpeg => CodecEntry {
                id: "mjpeg",
                name: "MJPEG",
                protocol: "http",
                mime: "image/jpeg",
            },
            Self::WebP => CodecEntry {
                id: "webp",
                name: "WebP",
                protocol: "http",
                mime: "image/webp",
            },
        }
    }

    pub fn all_entries() -> Vec<CodecEntry> {
        use self::Codec::*;
        vec![H264, H265, VP8, VP9, AV1, Mjpeg, WebP]
            .into_iter()
            .map(|c| c.entry())
            .collect()
    }
}
