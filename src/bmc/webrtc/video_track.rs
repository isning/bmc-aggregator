use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use webrtc::media::Sample;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use super::config::WebRtcConfig;
use crate::bmc::encoder::Codec;
use crate::bmc::stream::pipeline::EncodedFrame;

fn codec_capability(codec: Codec) -> RTCRtpCodecCapability {
    let mime_type = codec.mime_type().to_owned();
    let sdp_fmtp_line = match codec {
        Codec::H264 => "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
        Codec::H265 => "profile-id=1".to_owned(),
        _ => String::new(),
    };
    RTCRtpCodecCapability {
        mime_type,
        clock_rate: 90_000,
        sdp_fmtp_line,
        ..Default::default()
    }
}

pub struct VideoTrack {
    track: Arc<TrackLocalStaticSample>,
    rx: broadcast::Receiver<EncodedFrame>,
}

static FRAME_COUNTER: AtomicU64 = AtomicU64::new(0);

impl VideoTrack {
    pub fn new(
        rx: broadcast::Receiver<EncodedFrame>,
        codec: Codec,
        _config: &WebRtcConfig,
    ) -> Self {
        let track = Arc::new(TrackLocalStaticSample::new(
            codec_capability(codec),
            "video".to_owned(),
            "bmc-aggregator".to_owned(),
        ));

        Self {
            track,
            rx,
        }
    }

    pub fn track(&self) -> Arc<TrackLocalStaticSample> {
        self.track.clone()
    }

    pub async fn run(self) {
        let frame_duration = Duration::from_millis(33);

        let mut rx = self.rx;
        loop {
            let frame = match rx.recv().await {
                Ok(f) => f,
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("video_track: dropped {n} frame(s)");
                    continue;
                }
            };

            let seq = FRAME_COUNTER.fetch_add(1, Ordering::Relaxed);
            let is_keyframe = frame.data.len() > 1000;
            tracing::debug!(
                "video_track: frame #{seq} {} bytes (keyframe: {is_keyframe})",
                frame.data.len(),
            );

            if seq == 0 && frame.data.len() >= 16 {
                let preview: Vec<String> = frame.data[..16].iter().map(|b| format!("{b:02x}")).collect();
                tracing::info!(
                    "video_track: FIRST frame starts with: {} {}, has_annexb={}",
                    preview[..4].join(" "),
                    preview[4..8].join(" "),
                    frame.data[0] == 0 && frame.data[1] == 0 && (frame.data[2] == 1 || (frame.data[2] == 0 && frame.data[3] == 1)),
                );
            }

            let sample = Sample {
                data: bytes::Bytes::from(frame.data),
                duration: frame_duration,
                ..Default::default()
            };

            if let Err(e) = self.track.write_sample(&sample).await {
                tracing::warn!("video_track: write_sample error at frame #{seq}: {e}");
                break;
            }
        }
    }
}
