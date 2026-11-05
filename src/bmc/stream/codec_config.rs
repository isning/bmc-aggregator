use crate::bmc::encoder::Codec;

pub struct CodecPipeSpec {
    pub encoder_name: &'static str,
    pub encoder_props: &'static str,
    pub encoder_bitrate: BitrateProperty,
    pub encoder_raw_format: &'static str,
    pub nv_encoder_name: Option<&'static str>,
    pub nv_encoder_props: &'static str,
    pub nv_encoder_bitrate: BitrateProperty,
    pub nv_encoder_raw_format: &'static str,
    pub parser: Option<&'static str>,
    pub parser_props: &'static str,
    pub caps: Option<&'static str>,
    pub payloader: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitrateProperty {
    None,
    Kbps(&'static str),
    Bps(&'static str),
}

impl BitrateProperty {
    pub fn render(self, bitrate_kbps: u32) -> Option<String> {
        match self {
            Self::None => None,
            Self::Kbps(name) => Some(format!("{name}={bitrate_kbps}")),
            Self::Bps(name) => Some(format!("{name}={}", bitrate_kbps.saturating_mul(1000))),
        }
    }
}

pub fn encoder_props_with_bitrate(
    base_props: &str,
    bitrate: BitrateProperty,
    bitrate_kbps: u32,
) -> String {
    match bitrate.render(bitrate_kbps) {
        Some(rate_prop) if base_props.trim().is_empty() => rate_prop,
        Some(rate_prop) => format!("{base_props} {rate_prop}"),
        None => base_props.to_string(),
    }
}

pub fn codec_pipeline_spec(codec: Codec) -> Result<CodecPipeSpec, String> {
    match codec {
        Codec::H264 => Ok(CodecPipeSpec {
            encoder_name: "x264enc",
            encoder_props: "tune=zerolatency speed-preset=ultrafast",
            encoder_bitrate: BitrateProperty::Kbps("bitrate"),
            encoder_raw_format: "NV12",
            nv_encoder_name: Some("nvh264enc"),
            nv_encoder_props: "tune=ultra-low-latency preset=p2 zerolatency=true",
            nv_encoder_bitrate: BitrateProperty::Kbps("bitrate"),
            nv_encoder_raw_format: "NV12",
            parser: Some("h264parse"),
            parser_props: "config-interval=-1",
            caps: Some("video/x-h264,stream-format=byte-stream,alignment=au"),
            payloader: "rtph264pay",
        }),
        Codec::H265 => Ok(CodecPipeSpec {
            encoder_name: "x265enc",
            encoder_props: "tune=zerolatency speed-preset=ultrafast",
            encoder_bitrate: BitrateProperty::Kbps("bitrate"),
            encoder_raw_format: "I420",
            nv_encoder_name: Some("nvh265enc"),
            nv_encoder_props: "tune=ultra-low-latency preset=p2 zerolatency=true",
            nv_encoder_bitrate: BitrateProperty::Kbps("bitrate"),
            nv_encoder_raw_format: "NV12",
            parser: Some("h265parse"),
            parser_props: "config-interval=-1",
            caps: Some("video/x-h265,stream-format=byte-stream,alignment=au"),
            payloader: "rtph265pay",
        }),
        Codec::VP8 => Ok(CodecPipeSpec {
            encoder_name: "vp8enc",
            encoder_props: "deadline=1 cpu-used=16 lag-in-frames=0 threads=1",
            encoder_bitrate: BitrateProperty::Bps("target-bitrate"),
            encoder_raw_format: "I420",
            nv_encoder_name: None,
            nv_encoder_props: "",
            nv_encoder_bitrate: BitrateProperty::None,
            nv_encoder_raw_format: "NV12",
            parser: None,
            parser_props: "",
            caps: Some("video/x-vp8"),
            payloader: "rtpvp8pay",
        }),
        Codec::VP9 => Ok(CodecPipeSpec {
            encoder_name: "vp9enc",
            encoder_props: "deadline=1 cpu-used=8 lag-in-frames=0 threads=1 row-mt=false",
            encoder_bitrate: BitrateProperty::Bps("target-bitrate"),
            encoder_raw_format: "I420",
            nv_encoder_name: None,
            nv_encoder_props: "",
            nv_encoder_bitrate: BitrateProperty::None,
            nv_encoder_raw_format: "NV12",
            parser: None,
            parser_props: "",
            caps: Some("video/x-vp9"),
            payloader: "rtpvp9pay",
        }),
        Codec::AV1 => Ok(CodecPipeSpec {
            encoder_name: "av1enc",
            encoder_props: "cpu-used=8 end-usage=cbr",
            encoder_bitrate: BitrateProperty::Kbps("target-bitrate"),
            encoder_raw_format: "I420",
            nv_encoder_name: Some("nvav1enc"),
            nv_encoder_props: "tune=ultra-low-latency preset=p2 zerolatency=true",
            nv_encoder_bitrate: BitrateProperty::Kbps("bitrate"),
            nv_encoder_raw_format: "NV12",
            parser: Some("av1parse"),
            parser_props: "",
            caps: Some("video/x-av1"),
            payloader: "rtpav1pay",
        }),
        Codec::Mjpeg | Codec::WebP => Err(format!("codec {codec:?} is not a streaming codec")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h265_software_uses_x265_supported_input_format() {
        let spec = codec_pipeline_spec(Codec::H265).unwrap();
        assert_eq!(spec.encoder_name, "x265enc");
        assert_eq!(spec.encoder_raw_format, "I420");
        assert_eq!(spec.nv_encoder_raw_format, "NV12");
    }

    #[test]
    fn av1_software_uses_av1enc_supported_input_format() {
        let spec = codec_pipeline_spec(Codec::AV1).unwrap();
        assert_eq!(spec.encoder_name, "av1enc");
        assert_eq!(spec.encoder_raw_format, "I420");
        assert_eq!(spec.nv_encoder_raw_format, "NV12");
        assert_eq!(
            encoder_props_with_bitrate(spec.encoder_props, spec.encoder_bitrate, 5000),
            "cpu-used=8 end-usage=cbr target-bitrate=5000"
        );
    }

    #[test]
    fn vp8_vp9_use_bits_per_second_target_bitrate() {
        for codec in [Codec::VP8, Codec::VP9] {
            let spec = codec_pipeline_spec(codec).unwrap();
            assert_eq!(spec.encoder_raw_format, "I420");
            let props = encoder_props_with_bitrate(spec.encoder_props, spec.encoder_bitrate, 5000);
            assert!(props.contains("target-bitrate=5000000"));
            assert!(props.contains("lag-in-frames=0"));
        }
    }

    #[test]
    fn nvenc_uses_kbps_bitrate_property() {
        let spec = codec_pipeline_spec(Codec::AV1).unwrap();
        let props =
            encoder_props_with_bitrate(spec.nv_encoder_props, spec.nv_encoder_bitrate, 5000);
        assert!(props.contains("bitrate=5000"));
        assert!(props.contains("zerolatency=true"));
    }
}
