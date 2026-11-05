use crate::bmc::encoder::Codec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderBackendId {
    value: String,
}

impl EncoderBackendId {
    pub fn new(value: impl AsRef<str>) -> Self {
        let value = value.as_ref().trim();
        let value = if value.is_empty() { "software" } else { value };
        Self {
            value: value.to_string(),
        }
    }

    pub fn from_stream_request(value: impl AsRef<str>) -> Self {
        Self::new(normalize_stream_backend(value.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }

    pub fn stream_backend(&self) -> &'static str {
        normalize_stream_backend(&self.value)
    }
}

impl Default for EncoderBackendId {
    fn default() -> Self {
        Self::new("software")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodingSelection {
    codec: Codec,
    backend: EncoderBackendId,
}

impl EncodingSelection {
    pub fn new(codec: Codec, backend: impl AsRef<str>) -> Self {
        Self {
            codec,
            backend: EncoderBackendId::new(backend),
        }
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    pub fn codec_id(&self) -> &'static str {
        self.codec.as_str()
    }

    pub fn backend_id(&self) -> &str {
        self.backend.as_str()
    }

    pub fn stream_backend_id(&self) -> &'static str {
        self.backend.stream_backend()
    }

    pub fn set_codec(&mut self, codec: Codec) {
        self.codec = codec;
    }

    pub fn set_backend(&mut self, backend: impl AsRef<str>) {
        self.backend = EncoderBackendId::new(backend);
    }

    pub fn set_stream_backend(&mut self, backend: impl AsRef<str>) {
        self.backend = EncoderBackendId::from_stream_request(backend);
    }

    pub fn set_encoder_value(&mut self, value: impl AsRef<str>) {
        let value = value.as_ref().trim();
        if let Some((codec, backend)) = value.split_once('/') {
            if let Some(codec) = Codec::from_str(codec.trim()) {
                self.codec = codec;
            }
            self.set_backend(backend);
        } else {
            self.set_backend(value);
        }
    }

    pub fn streaming_codec_or(&self, default: Codec) -> Codec {
        if self.codec.supports_webrtc() {
            self.codec
        } else {
            default
        }
    }
}

impl Default for EncodingSelection {
    fn default() -> Self {
        Self::new(Codec::Mjpeg, "software")
    }
}

pub fn parse_stream_backend(value: &str) -> Option<&'static str> {
    match value.trim() {
        "software" => Some("software"),
        "nvenc" | "nvh264enc" | "nvh265enc" | "nvav1enc" => Some("nvenc"),
        _ => None,
    }
}

pub fn normalize_stream_backend(value: &str) -> &'static str {
    parse_stream_backend(value).unwrap_or("software")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_value_can_carry_codec_and_backend() {
        let mut selection = EncodingSelection::default();
        selection.set_encoder_value("h264/nvenc");

        assert_eq!(selection.codec(), Codec::H264);
        assert_eq!(selection.backend_id(), "nvenc");
        assert_eq!(selection.stream_backend_id(), "nvenc");
    }

    #[test]
    fn stream_backend_normalizes_gstreamer_element_names() {
        let mut selection = EncodingSelection::default();
        selection.set_stream_backend("nvh264enc");

        assert_eq!(selection.backend_id(), "nvenc");
        assert_eq!(selection.stream_backend_id(), "nvenc");
    }

    #[test]
    fn runtime_config_preserves_unknown_backend_name() {
        let mut selection = EncodingSelection::default();
        selection.set_encoder_value("vaapi");

        assert_eq!(selection.backend_id(), "vaapi");
        assert_eq!(selection.stream_backend_id(), "software");
    }
}
