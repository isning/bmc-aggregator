use crate::bmc::encoder::Codec;

pub use super::gst_encoder::GstEncoder;

pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub timestamp_us: u64,
    pub duration_us: Option<u64>,
}

pub trait VideoStreamEncoder: Send {
    fn encode_packets(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Vec<EncodedPacket>, String>;
    fn encode(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<Vec<u8>>, String> {
        self.encode_packets(rgba, width, height)
            .map(|packets| packets.into_iter().map(|packet| packet.data).collect())
    }
    fn codec(&self) -> Codec;
    fn request_keyframe(&self);
    fn perf_snapshot(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}

type StreamEncoderFactory =
    fn(Codec, u32, u32, u32, u32) -> Result<Box<dyn VideoStreamEncoder>, String>;

/// A registered stream encoder backend.
pub struct StreamEncoderBackend {
    pub id: &'static str,
    pub name: &'static str,
    pub create: StreamEncoderFactory,
}

impl crate::bmc::registry::HasId for StreamEncoderBackend {
    fn id(&self) -> &str {
        self.id
    }
}

pub static STREAM_ENCODER_BACKENDS: &[StreamEncoderBackend] = &[
    StreamEncoderBackend {
        id: "software",
        name: "Software",
        create: |codec, w, h, bitrate, fps| {
            GstEncoder::new_software(codec, w, h, bitrate, fps)
                .map(|e| Box::new(e) as Box<dyn VideoStreamEncoder>)
        },
    },
    StreamEncoderBackend {
        id: "nvenc",
        name: "NVENC",
        create: |codec, w, h, bitrate, fps| {
            GstEncoder::new_nvenc(codec, w, h, bitrate, fps)
                .map(|e| Box::new(e) as Box<dyn VideoStreamEncoder>)
        },
    },
];

pub fn create_stream_encoder(
    codec: Codec,
    backend: &str,
    width: u32,
    height: u32,
    bitrate_kbps: u32,
    framerate: u32,
) -> Result<Box<dyn VideoStreamEncoder>, String> {
    crate::bmc::registry::find_backend(STREAM_ENCODER_BACKENDS, backend)
        .map(|b| (b.create)(codec, width, height, bitrate_kbps, framerate))
        .unwrap_or_else(|| Err(format!("unknown stream encoder backend: '{backend}'")))
}
