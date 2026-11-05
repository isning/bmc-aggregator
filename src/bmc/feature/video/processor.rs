use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::debug;

use crate::bmc::encoder::registry::{
    codec_available_for_webrtc, probe_stream_encoder, EncoderRegistry,
};
use crate::bmc::encoder::Codec;
use crate::bmc::feature::connection::FeatureGuard;
use crate::bmc::feature::fault::{FeatureFault, FeatureFaultDomain, FeatureFaultStore};
use crate::bmc::feature::video::encoding::EncodingSelection;
use crate::bmc::feature::video::VideoSource;
use crate::bmc::stream::gst_webrtc::GstWebRtcSession;
use crate::bmc::stream::pipeline::{RawFrame, RawPixelFormat};

pub struct WebRtcSessionOwner {
    pub session: Arc<GstWebRtcSession>,
    _guard: FeatureGuard,
}

impl Drop for WebRtcSessionOwner {
    fn drop(&mut self) {
        self.session.close();
    }
}

pub struct ActiveWebTransportSession {
    codec: String,
    encoder_backend: String,
    width: u32,
    height: u32,
    pixel_format: RawPixelFormat,
    started_at: Instant,
    raw_frames_received: AtomicU64,
    encoded_packets: AtomicU64,
    encoded_bytes: AtomicU64,
    keyframes: AtomicU64,
    encode_count: AtomicU64,
    encode_time_total_us: AtomicU64,
    encode_time_max_us: AtomicU64,
    lagged_frames: AtomicU64,
    encode_errors: AtomicU64,
    write_errors: AtomicU64,
    last_trace_id: AtomicU64,
    last_frame_age_us: AtomicU64,
}

impl ActiveWebTransportSession {
    pub fn new(
        codec: String,
        encoder_backend: String,
        width: u32,
        height: u32,
        pixel_format: RawPixelFormat,
    ) -> Self {
        Self {
            codec,
            encoder_backend,
            width,
            height,
            pixel_format,
            started_at: Instant::now(),
            raw_frames_received: AtomicU64::new(0),
            encoded_packets: AtomicU64::new(0),
            encoded_bytes: AtomicU64::new(0),
            keyframes: AtomicU64::new(0),
            encode_count: AtomicU64::new(0),
            encode_time_total_us: AtomicU64::new(0),
            encode_time_max_us: AtomicU64::new(0),
            lagged_frames: AtomicU64::new(0),
            encode_errors: AtomicU64::new(0),
            write_errors: AtomicU64::new(0),
            last_trace_id: AtomicU64::new(0),
            last_frame_age_us: AtomicU64::new(0),
        }
    }

    pub fn record_raw_frame(&self, frame: &RawFrame) {
        self.raw_frames_received.fetch_add(1, Ordering::Relaxed);
        self.last_trace_id.store(frame.trace_id, Ordering::Relaxed);
        self.last_frame_age_us.store(
            frame.created_at.elapsed().as_micros() as u64,
            Ordering::Relaxed,
        );
    }

    pub fn record_packet(&self, bytes: usize, keyframe: bool) {
        self.encoded_packets.fetch_add(1, Ordering::Relaxed);
        self.encoded_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        if keyframe {
            self.keyframes.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_encode_time(&self, elapsed: Duration) {
        let elapsed_us = elapsed.as_micros().max(1) as u64;
        self.encode_count.fetch_add(1, Ordering::Relaxed);
        self.encode_time_total_us
            .fetch_add(elapsed_us, Ordering::Relaxed);
        if elapsed_us > self.encode_time_max_us.load(Ordering::Relaxed) {
            self.encode_time_max_us.store(elapsed_us, Ordering::Relaxed);
        }
    }

    pub fn record_lagged(&self, n: u64) {
        self.lagged_frames.fetch_add(n, Ordering::Relaxed);
    }

    pub fn record_encode_error(&self) {
        self.encode_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_write_error(&self) {
        self.write_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn trace_snapshot(&self, connection_id: &str) -> serde_json::Value {
        let elapsed = self.started_at.elapsed().max(Duration::from_millis(1));
        let packets = self.encoded_packets.load(Ordering::Relaxed);
        let bytes = self.encoded_bytes.load(Ordering::Relaxed);
        let encode_count = self.encode_count.load(Ordering::Relaxed);
        let encode_total_us = self.encode_time_total_us.load(Ordering::Relaxed);
        let seconds = elapsed.as_secs_f64();
        serde_json::json!({
            "connection_id": connection_id,
            "codec": self.codec,
            "encoder_backend": self.encoder_backend,
            "expected_caps": {
                "width": self.width,
                "height": self.height,
                "pixel_format": self.pixel_format.gst_caps_format(),
            },
            "frames": {
                "raw_received": self.raw_frames_received.load(Ordering::Relaxed),
                "lagged": self.lagged_frames.load(Ordering::Relaxed),
                "last_trace_id": self.last_trace_id.load(Ordering::Relaxed),
                "last_frame_age_ms": self.last_frame_age_us.load(Ordering::Relaxed) as f64 / 1000.0,
            },
            "encoder": {
                "packets": packets,
                "bytes": bytes,
                "keyframes": self.keyframes.load(Ordering::Relaxed),
                "packets_per_second": packets as f64 / seconds,
                "bitrate_mbps": (bytes as f64 * 8.0) / seconds / 1_000_000.0,
                "encode_time_avg_ms": if encode_count > 0 { encode_total_us as f64 / encode_count as f64 / 1000.0 } else { 0.0 },
                "encode_time_max_ms": self.encode_time_max_us.load(Ordering::Relaxed) as f64 / 1000.0,
                "encode_count": encode_count,
                "encode_errors": self.encode_errors.load(Ordering::Relaxed),
            },
            "stream": {
                "write_errors": self.write_errors.load(Ordering::Relaxed),
                "connected_secs": elapsed.as_secs(),
            },
        })
    }
}

impl WebRtcSessionOwner {
    pub fn new(session: Arc<GstWebRtcSession>, guard: FeatureGuard) -> Self {
        Self {
            session,
            _guard: guard,
        }
    }
}

#[derive(Clone, Debug)]
pub struct VideoEncodingState {
    selection: EncodingSelection,
    bitrate_kbps: u32,
}

impl VideoEncodingState {
    fn with_encoder_backend(encoder_backend: Option<String>) -> Self {
        let mut selection = EncodingSelection::default();
        if let Some(encoder_backend) = encoder_backend {
            selection.set_encoder_value(encoder_backend);
        }
        Self {
            selection,
            bitrate_kbps: 5000,
        }
    }

    pub fn stream_mode(&self) -> &'static str {
        self.selection.codec_id()
    }

    pub fn encoder_backend(&self) -> &str {
        self.selection.backend_id()
    }

    pub fn streaming_codec_or(&self, default: Codec) -> Codec {
        self.selection.streaming_codec_or(default)
    }

    pub fn streaming_backend(&self) -> &'static str {
        self.selection.stream_backend_id()
    }
}

pub struct VideoProcessor {
    frame_tx: broadcast::Sender<RawFrame>,
    width: AtomicU32,
    height: AtomicU32,
    pixel_format: RwLock<RawPixelFormat>,
    last_frame: Mutex<Option<RawFrame>>,
    webrtc_connections: Arc<RwLock<HashMap<String, WebRtcSessionOwner>>>,
    webtransport_connections:
        Arc<std::sync::Mutex<HashMap<String, Arc<ActiveWebTransportSession>>>>,
    encoding: RwLock<VideoEncodingState>,
    raw_frames_in: AtomicU64,
    raw_frames_published: AtomicU64,
    raw_frames_replayed: AtomicU64,
    raw_bytes_in: AtomicU64,
    last_trace_id: AtomicU64,
    last_frame_age_us: AtomicU64,
    mjpeg_encode_count: AtomicU64,
    mjpeg_encode_time_total_us: AtomicU64,
    mjpeg_encode_time_max_us: AtomicU64,
    faults: FeatureFaultStore,
}

impl VideoProcessor {
    pub fn new() -> Self {
        Self::with_encoder_backend(None)
    }

    pub fn with_encoder_backend(encoder_backend: Option<String>) -> Self {
        Self::with_encoder_backend_and_faults(encoder_backend, FeatureFaultStore::default())
    }

    pub fn with_encoder_backend_and_faults(
        encoder_backend: Option<String>,
        faults: FeatureFaultStore,
    ) -> Self {
        let (frame_tx, _) = broadcast::channel(8);
        Self {
            frame_tx,
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
            pixel_format: RwLock::new(RawPixelFormat::Rgba),
            last_frame: Mutex::new(None),
            webrtc_connections: Arc::new(RwLock::new(HashMap::new())),
            webtransport_connections: Arc::new(std::sync::Mutex::new(HashMap::new())),
            encoding: RwLock::new(VideoEncodingState::with_encoder_backend(encoder_backend)),
            raw_frames_in: AtomicU64::new(0),
            raw_frames_published: AtomicU64::new(0),
            raw_frames_replayed: AtomicU64::new(0),
            raw_bytes_in: AtomicU64::new(0),
            last_trace_id: AtomicU64::new(0),
            last_frame_age_us: AtomicU64::new(0),
            mjpeg_encode_count: AtomicU64::new(0),
            mjpeg_encode_time_total_us: AtomicU64::new(0),
            mjpeg_encode_time_max_us: AtomicU64::new(0),
            faults,
        }
    }

    pub fn frame_tx(&self) -> broadcast::Sender<RawFrame> {
        self.frame_tx.clone()
    }

    pub async fn record_fault(&self, fault: FeatureFault) {
        self.faults.record(fault).await;
    }

    pub async fn clear_feature_fault(&self) {
        self.faults.clear_active().await;
    }

    pub async fn active_feature_fault(&self) -> Option<FeatureFault> {
        self.faults.active().await
    }

    pub fn resolution(&self) -> (u32, u32) {
        (
            self.width.load(Ordering::Relaxed),
            self.height.load(Ordering::Relaxed),
        )
    }

    pub async fn frame_format(&self) -> RawPixelFormat {
        *self.pixel_format.read().await
    }

    pub async fn first_frame_metadata(&self) -> Option<(u32, u32, RawPixelFormat)> {
        let (width, height) = self.resolution();
        if width == 0 || height == 0 {
            return None;
        }
        Some((width, height, self.frame_format().await))
    }

    pub async fn publish_last_frame(&self) -> bool {
        let Some(frame) = self.last_frame.lock().await.clone() else {
            return false;
        };
        self.raw_frames_replayed.fetch_add(1, Ordering::Relaxed);
        self.frame_tx.send(frame).is_ok()
    }

    #[cfg(test)]
    pub(crate) async fn block_frame_publication(
        &self,
    ) -> tokio::sync::MutexGuard<'_, Option<RawFrame>> {
        self.last_frame.lock().await
    }

    pub async fn last_frame(&self) -> Option<RawFrame> {
        self.last_frame.lock().await.clone()
    }

    pub fn last_trace_id(&self) -> u64 {
        self.last_trace_id.load(Ordering::Relaxed)
    }

    pub async fn encoding_state(&self) -> VideoEncodingState {
        self.encoding.read().await.clone()
    }

    pub async fn current_encoder_backend(&self) -> String {
        self.encoding.read().await.encoder_backend().to_string()
    }

    pub async fn set_encoder_backend(&self, encoder: String) {
        self.encoding
            .write()
            .await
            .selection
            .set_encoder_value(encoder);
    }

    pub async fn streaming_selection_or_default(&self, default: Codec) -> (Codec, String) {
        let encoding = self.encoding.read().await;
        (
            encoding.streaming_codec_or(default),
            encoding.streaming_backend().to_string(),
        )
    }

    pub async fn bitrate_kbps(&self) -> u32 {
        self.encoding.read().await.bitrate_kbps
    }

    pub async fn set_bitrate_kbps(&self, bitrate: u32) {
        self.encoding.write().await.bitrate_kbps = bitrate;
    }

    pub async fn stream_mode(&self) -> String {
        self.encoding.read().await.stream_mode().to_string()
    }

    pub async fn set_stream_codec(&self, codec: Codec) {
        self.encoding.write().await.selection.set_codec(codec);
    }

    pub async fn set_encoding_selection(&self, codec: Codec, backend: &str) {
        self.encoding.write().await.selection = EncodingSelection::new(codec, backend);
    }

    pub async fn config_get(&self) -> serde_json::Value {
        let encoding = self.encoding_state().await;
        serde_json::json!({
            "mode": encoding.stream_mode(),
            "encoder": encoding.encoder_backend(),
            "bitrate_preset": { "type": "Balanced" },
            "bitrate": encoding.bitrate_kbps,
            "bitrate_kbps": encoding.bitrate_kbps,
            "has_public_ice_servers": false,
            "using_public_ice_servers": false,
        })
    }

    pub async fn stream_start(&self) -> serde_json::Value {
        let mode = self.stream_mode().await;
        serde_json::json!({
            "success": true,
            "streaming": true,
            "mode": mode,
        })
    }

    pub async fn stream_stop(&self) -> serde_json::Value {
        let mode = self.stream_mode().await;
        serde_json::json!({
            "success": true,
            "streaming": false,
            "mode": mode,
        })
    }

    pub async fn constraints(&self) -> serde_json::Value {
        let current_mode = self.stream_mode().await;
        serde_json::json!({
            "success": true,
            "constraints": {
                "allowed_codecs": allowed_codec_ids(),
                "http_modes": EncoderRegistry::http_mode_ids(),
                "locked_codec": null,
                "disallow_mjpeg": false,
                "sources": {
                    "rustdesk": false,
                    "rtsp": false
                },
                "reason": "",
                "current_mode": current_mode,
            },
            "allowed_codecs": allowed_codec_ids(),
            "http_modes": EncoderRegistry::http_mode_ids(),
            "locked_codec": null,
            "disallow_mjpeg": false,
            "sources": {
                "rustdesk": false,
                "rtsp": false
            },
            "reason": "",
            "current_mode": current_mode,
        })
    }

    pub async fn bitrate_response(&self) -> serde_json::Value {
        let bitrate = self.bitrate_kbps().await;
        serde_json::json!({
            "success": true,
            "bitrate": bitrate,
            "bitrate_kbps": bitrate,
            "preset": "balanced",
        })
    }

    pub async fn update_bitrate(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, &'static str> {
        let object = params
            .as_object()
            .ok_or("bitrate parameters must be an object")?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "bitrate_kbps" | "bitrate" | "bitrate_preset"))
        {
            return Err("unsupported bitrate parameter");
        }
        for key in ["bitrate_kbps", "bitrate"] {
            if let Some(raw) = object.get(key) {
                if raw
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .is_none()
                {
                    return Err("bitrate must be an unsigned integer");
                }
            }
        }
        if matches!((object.get("bitrate_kbps"), object.get("bitrate")), (Some(a), Some(b)) if a != b)
        {
            return Err("bitrate aliases disagree");
        }
        if let Some(raw) = object.get("bitrate_preset") {
            if object.contains_key("bitrate_kbps") || object.contains_key("bitrate") {
                return Err("provide either an explicit bitrate or a preset");
            }
            let preset = raw.as_object().ok_or("bitrate preset must be an object")?;
            let kind = preset.get("type").and_then(|value| value.as_str());
            if !matches!(kind, Some("Speed" | "Balanced" | "Quality" | "Custom"))
                || preset
                    .keys()
                    .any(|key| key != "type" && !(kind == Some("Custom") && key == "value"))
            {
                return Err("unsupported bitrate preset");
            }
        }
        let explicit = params
            .get("bitrate_kbps")
            .or_else(|| params.get("bitrate"))
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok());
        let preset = params
            .get("bitrate_preset")
            .and_then(|value| value.get("type"))
            .and_then(|value| value.as_str());
        let value = match (explicit, preset) {
            (Some(value), _) => value,
            (None, Some("Speed")) => 1_000,
            (None, Some("Balanced")) => 4_000,
            (None, Some("Quality")) => 8_000,
            (None, Some("Custom")) => params
                .get("bitrate_preset")
                .and_then(|value| value.get("value"))
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0),
            (None, Some(_)) => 0,
            (None, None) => 0,
        };
        if !(128..=100_000).contains(&value) {
            return Err("bitrate must be between 128 and 100000 kbps");
        }
        self.set_bitrate_kbps(value).await;
        Ok(self.bitrate_response().await)
    }

    pub fn encoder_self_check(&self) -> serde_json::Value {
        let current_hardware_encoder = if probe_stream_encoder("nvenc") {
            "nvenc"
        } else {
            "None"
        };
        let backends: Vec<serde_json::Value> = EncoderRegistry::discover()
            .into_iter()
            .map(|backend| {
                serde_json::json!({
                    "id": backend.id,
                    "name": backend.name,
                    "is_hardware": backend.is_hardware,
                    "available": backend.available,
                    "supported_formats": crate::bmc::encoder::codec::codecs_for_backend(backend.id),
                })
            })
            .collect();
        let codecs: Vec<serde_json::Value> = EncoderRegistry::codecs()
            .into_iter()
            .map(|codec| {
                serde_json::json!({
                    "id": codec.id,
                    "name": codec.name,
                    "protocol": codec.protocol,
                    "mime": codec.mime,
                })
            })
            .collect();
        serde_json::json!({
            "success": true,
            "current_hardware_encoder": current_hardware_encoder,
            "backends": backends,
            "codecs": codecs,
            "rows": [],
        })
    }

    pub async fn reset_stream(&self, source: Option<Arc<dyn VideoSource>>) -> serde_json::Value {
        let closed_webrtc_connections = self.close_all_webrtc_connections().await;
        let source_result = match source {
            Some(source) => match source.request_full_frame().await {
                Ok(true) => serde_json::json!({
                    "success": true,
                    "base_frame": false,
                    "invalidate_requested": true,
                    "source": "connected",
                }),
                Ok(false) | Err(_) => {
                    let fault = FeatureFault::operation(
                        FeatureFaultDomain::Operation,
                        "full_frame_request_failed",
                        "The video source could not request a full frame",
                        true,
                        "reset_stream",
                    );
                    self.record_fault(fault.clone()).await;
                    serde_json::json!({
                        "success": false,
                        "fault": fault,
                    })
                }
            },
            None => {
                let fault = FeatureFault::operation(
                    FeatureFaultDomain::Lifecycle,
                    "source_unavailable",
                    "The video source is not connected",
                    true,
                    "reset_stream",
                );
                self.record_fault(fault.clone()).await;
                serde_json::json!({
                    "success": false,
                    "fault": fault,
                })
            }
        };
        let base_frame_replayed = self.publish_last_frame().await;
        serde_json::json!({
            "success": source_result.get("success").and_then(serde_json::Value::as_bool) == Some(true),
            "closed_webrtc_connections": closed_webrtc_connections,
            "base_frame_replayed": base_frame_replayed,
            "source": source_result,
        })
    }

    pub fn record_mjpeg_encode_time(&self, elapsed: Duration) {
        let elapsed_us = elapsed.as_micros().max(1) as u64;
        self.mjpeg_encode_count.fetch_add(1, Ordering::Relaxed);
        self.mjpeg_encode_time_total_us
            .fetch_add(elapsed_us, Ordering::Relaxed);
        if elapsed_us > self.mjpeg_encode_time_max_us.load(Ordering::Relaxed) {
            self.mjpeg_encode_time_max_us
                .store(elapsed_us, Ordering::Relaxed);
        }
    }

    /// Push a raw GstBuffer into the pipeline and broadcast to WebRTC subscribers.
    pub async fn finish_frame(
        &self,
        buf: gst::Buffer,
        width: u32,
        height: u32,
        pixel_format: RawPixelFormat,
    ) -> bool {
        let frame = RawFrame {
            data: buf,
            width,
            height,
            pixel_format,
            trace_id: 0,
            created_at: Instant::now(),
        };
        self.finish_raw_frame(frame).await
    }

    pub async fn finish_raw_frame(&self, frame: RawFrame) -> bool {
        if let Err(error) = frame.validate() {
            debug!(%error, "rejecting invalid raw frame");
            return false;
        }
        let buf = frame.data.clone();

        self.width.store(frame.width, Ordering::Relaxed);
        self.height.store(frame.height, Ordering::Relaxed);
        *self.pixel_format.write().await = frame.pixel_format;
        self.raw_frames_in.fetch_add(1, Ordering::Relaxed);
        self.raw_frames_published.fetch_add(1, Ordering::Relaxed);
        self.raw_bytes_in
            .fetch_add(buf.size() as u64, Ordering::Relaxed);
        self.last_trace_id.store(frame.trace_id, Ordering::Relaxed);
        self.last_frame_age_us.store(
            frame.created_at.elapsed().as_micros() as u64,
            Ordering::Relaxed,
        );

        *self.last_frame.lock().await = Some(frame.clone());
        self.frame_tx.send(frame).ok();
        true
    }

    pub async fn trace_snapshot(&self, backend: Option<serde_json::Value>) -> serde_json::Value {
        let (width, height) = self.resolution();
        let sessions: Vec<serde_json::Value> = self
            .webrtc_connections
            .read()
            .await
            .iter()
            .map(|(id, active)| {
                serde_json::json!({
                    "connection_id": id,
                    "gstreamer": active.session.trace_snapshot(),
                })
            })
            .collect();
        let webtransport_sessions: Vec<serde_json::Value> = self
            .webtransport_connections
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(id, active)| active.trace_snapshot(id))
            .collect();
        let mjpeg_encode_count = self.mjpeg_encode_count.load(Ordering::Relaxed);
        let mjpeg_encode_total_us = self.mjpeg_encode_time_total_us.load(Ordering::Relaxed);

        serde_json::json!({
            "processor": {
                "width": width,
                "height": height,
                "pixel_format": self.frame_format().await.gst_caps_format(),
                "stream_mode": self.stream_mode().await,
                "encoder_backend": self.current_encoder_backend().await,
                "bitrate_kbps": self.bitrate_kbps().await,
                "raw_frames_in": self.raw_frames_in.load(Ordering::Relaxed),
                "raw_frames_published": self.raw_frames_published.load(Ordering::Relaxed),
                "raw_frames_replayed": self.raw_frames_replayed.load(Ordering::Relaxed),
                "raw_bytes_in": self.raw_bytes_in.load(Ordering::Relaxed),
                "last_trace_id": self.last_trace_id.load(Ordering::Relaxed),
                "last_frame_age_ms": self.last_frame_age_us.load(Ordering::Relaxed) as f64 / 1000.0,
                "broadcast_receivers": self.frame_tx.receiver_count(),
                "webrtc_connections": sessions.len(),
                "webtransport_connections": webtransport_sessions.len(),
            },
            "mjpeg_encoder": {
                "encode_time_avg_ms": if mjpeg_encode_count > 0 { mjpeg_encode_total_us as f64 / mjpeg_encode_count as f64 / 1000.0 } else { 0.0 },
                "encode_time_max_ms": self.mjpeg_encode_time_max_us.load(Ordering::Relaxed) as f64 / 1000.0,
                "encode_count": mjpeg_encode_count,
            },
            "faults": self.faults.snapshot().await,
            "backend": backend.unwrap_or_else(|| serde_json::json!({})),
            "webrtc_sessions": sessions,
            "webtransport_sessions": webtransport_sessions,
        })
    }

    // ── WebRTC session management ─────────────────────────────────
    // FC owns the session lifecycle. Each WebRTC session keeps its own guard.

    pub async fn set_webrtc_connection(self: &Arc<Self>, owner: WebRtcSessionOwner) {
        let session = &owner.session;
        let connection_id = session.id.clone();
        let weak = Arc::downgrade(self);
        let mut transport = session.connection_state_rx();
        let expected = session.clone();
        let key = connection_id.clone();
        let previous = self
            .webrtc_connections
            .write()
            .await
            .insert(connection_id, owner);
        // A client-supplied id must never orphan an already-running GStreamer
        // pipeline.  Dropping the guard alone does not close that pipeline.
        if let Some(previous) = previous {
            previous.session.close();
        }
        tokio::spawn(async move {
            use gstreamer_webrtc::WebRTCPeerConnectionState as State;
            let initial_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            let mut disconnected_deadline = None;
            loop {
                let state = *transport.borrow_and_update();
                let deadline = match state {
                    State::Closed | State::Failed => break,
                    State::Disconnected => Some(*disconnected_deadline.get_or_insert_with(|| {
                        tokio::time::Instant::now() + Duration::from_secs(10)
                    })),
                    State::Connected => {
                        disconnected_deadline = None;
                        None
                    }
                    State::New | State::Connecting => Some(initial_deadline),
                    _ => break,
                };
                tokio::select! {
                    changed = transport.changed() => { if changed.is_err() { break; } }
                    _ = async {
                        match deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => break,
                }
            }
            if let Some(processor) = weak.upgrade() {
                let removed = {
                    let mut sessions = processor.webrtc_connections.write().await;
                    if sessions
                        .get(&key)
                        .is_some_and(|active| Arc::ptr_eq(&active.session, &expected))
                    {
                        sessions.remove(&key)
                    } else {
                        None
                    }
                };
                if let Some(active) = removed {
                    active.session.close();
                }
            }
        });
    }

    pub async fn clear_webrtc_connection(&self, connection_id: &str) {
        let removed = self.webrtc_connections.write().await.remove(connection_id);
        drop(removed);
    }

    pub async fn get_webrtc_connection(
        &self,
        connection_id: &str,
    ) -> Option<Arc<GstWebRtcSession>> {
        self.webrtc_connections
            .read()
            .await
            .get(connection_id)
            .map(|s| s.session.clone())
    }

    pub async fn close_all_webrtc_connections(&self) -> usize {
        let mut connections = self.webrtc_connections.write().await;
        let sessions: Vec<_> = connections.drain().map(|(_, active)| active).collect();
        let count = sessions.len();
        drop(connections);
        drop(sessions);
        count
    }

    pub async fn register_webtransport_connection(
        &self,
        connection_id: String,
        session: Arc<ActiveWebTransportSession>,
    ) -> Result<WebTransportRegistration, String> {
        let mut sessions = self
            .webtransport_connections
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if sessions.contains_key(&connection_id) {
            return Err("WebTransport connection ID already active".into());
        }
        sessions.insert(connection_id.clone(), session.clone());
        Ok(WebTransportRegistration {
            sessions: self.webtransport_connections.clone(),
            connection_id,
            session,
        })
    }
}

fn allowed_codec_ids() -> Vec<&'static str> {
    EncoderRegistry::codecs()
        .into_iter()
        .filter_map(|entry| {
            let codec = Codec::from_str(entry.id)?;
            if !codec.supports_webrtc() || codec_available_for_webrtc(codec) {
                Some(entry.id)
            } else {
                None
            }
        })
        .collect()
}

impl Default for VideoProcessor {
    fn default() -> Self {
        Self::new()
    }
}

// The trace entry follows the task's lifetime, including cancellation.
pub struct WebTransportRegistration {
    sessions: Arc<std::sync::Mutex<HashMap<String, Arc<ActiveWebTransportSession>>>>,
    connection_id: String,
    session: Arc<ActiveWebTransportSession>,
}

impl Drop for WebTransportRegistration {
    fn drop(&mut self) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if sessions
            .get(&self.connection_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.session))
        {
            sessions.remove(&self.connection_id);
        }
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn video_mode_update_is_atomic_for_encoding_observers() {
        use crate::bmc::feature::{
            config::{FeatureConfig, FeatureConfigs},
            connection::FeatureConnection,
            device_registry::DeviceRegistry,
            managed::ManagedDevice,
            video::runtime::VideoRuntime,
        };
        use axum::{
            extract::{Path, State},
            Json,
        };
        let runtime = Arc::new(VideoRuntime::new());
        let processor = runtime.processor();
        processor
            .set_encoding_selection(Codec::H265, "software")
            .await;
        let fc = FeatureConnection::new(
            "device".into(),
            "video".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        fc.set_feature_runtime(runtime).await;
        let registry = DeviceRegistry::new();
        registry
            .insert(
                "device".into(),
                Arc::new(ManagedDevice::new(
                    "device".into(),
                    FeatureConfigs {
                        features: Default::default(),
                    },
                    [("video".into(), fc)].into(),
                )),
            )
            .await;
        let config = Arc::new(RwLock::new(serde_json::from_value::<crate::config::Config>(
            serde_json::json!({"server":{"listen":"127.0.0.1:0","frontend_dir":"frontend/dist"},"devices":[]})
        ).unwrap()));
        let state = Arc::new(crate::web::state::AppState {
            registry,
            config: config.clone(),
            config_path: String::new(),
            configuration: Default::default(),
            msd_storage: crate::bmc::msd_storage::FsMsdStorage::new(
                ".local/quality/mode-test".into(),
                config,
            ),
            active_downloads: Default::default(),
            webtransport: Default::default(),
        });
        let held = processor.encoding.read().await;
        let update = crate::bmc::feature::video::mode::post_mode(
            Path("device".into()),
            State(state),
            Json(serde_json::json!({"mode":"h264","backend":"nvenc"})),
        );
        tokio::pin!(update);
        assert!(futures::poll!(&mut update).is_pending());
        let observation = processor.encoding.read();
        tokio::pin!(observation);
        assert!(futures::poll!(&mut observation).is_pending());
        drop(held);
        let completed = match futures::poll!(&mut update) {
            std::task::Poll::Ready(response) => Some(response),
            std::task::Poll::Pending => None,
        };
        let encoding = observation.await;
        let observed = (
            encoding.stream_mode().to_owned(),
            encoding.encoder_backend().to_owned(),
        );
        drop(encoding);
        let response = match completed {
            Some(response) => response,
            None => update.await,
        };
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(observed, ("h264".into(), "nvenc".into()));
    }

    #[tokio::test]
    async fn webtransport_registration_rejects_duplicates_and_releases_on_abort() {
        let processor = Arc::new(VideoProcessor::new());
        let session = || {
            Arc::new(ActiveWebTransportSession::new(
                "vp8".into(),
                "software".into(),
                160,
                120,
                RawPixelFormat::Rgba,
            ))
        };
        let owner = processor
            .register_webtransport_connection("browser".into(), session())
            .await
            .unwrap();
        assert!(processor
            .register_webtransport_connection("browser".into(), session())
            .await
            .is_err());
        drop(owner);
        let owner = processor
            .register_webtransport_connection("browser".into(), session())
            .await
            .unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _owner = owner;
            ready_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(processor
            .webtransport_connections
            .lock()
            .unwrap()
            .is_empty());
        assert!(processor
            .register_webtransport_connection("browser".into(), session())
            .await
            .is_ok());
    }

    use super::*;

    #[tokio::test]
    async fn session_handoff_retains_its_lease_until_processor_cleanup() {
        use crate::bmc::feature::{
            config::{BackendParams, FeatureConfig, FeatureParams},
            connection::FeatureConnection,
        };
        let fc = FeatureConnection::new(
            "device".into(),
            "video".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: vec![],
            },
        );
        let processor = Arc::new(VideoProcessor::new());
        let session = Arc::new(
            GstWebRtcSession::new(
                "owned-id".into(),
                None,
                Codec::VP8,
                "software",
                64,
                64,
                1,
                128,
            )
            .unwrap(),
        );
        let owner = WebRtcSessionOwner::new(session.clone(), FeatureGuard::new(fc.clone()));
        session.start_main_loop().unwrap();
        processor.set_webrtc_connection(owner).await;
        assert!(processor.get_webrtc_connection("owned-id").await.is_some());
        assert_eq!(fc.ref_count(), 1);
        assert_eq!(processor.close_all_webrtc_connections().await, 1);
        assert_eq!(fc.ref_count(), 0);
        assert_eq!(
            *session.connection_state_rx().borrow(),
            gstreamer_webrtc::WebRTCPeerConnectionState::Closed
        );
    }

    #[tokio::test]
    async fn cancelled_session_owner_closes_a_negotiated_session_before_handoff() {
        use crate::bmc::feature::{
            config::{BackendParams, FeatureConfig, FeatureParams},
            connection::{FeatureConnection, FeatureGuard},
        };
        let fc = FeatureConnection::new(
            "device".into(),
            "video".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: vec![],
            },
        );
        let peer = GstWebRtcSession::new(
            "offer-peer".into(),
            None,
            Codec::VP8,
            "software",
            64,
            64,
            1,
            128,
        )
        .unwrap();
        peer.start_main_loop().unwrap();
        let offer = tokio::time::timeout(Duration::from_secs(5), peer.create_offer_sdp())
            .await
            .unwrap()
            .unwrap()
            .replace("a=sendonly", "a=recvonly");
        let session = Arc::new(
            GstWebRtcSession::new(
                "pending-owner".into(),
                None,
                Codec::VP8,
                "software",
                64,
                64,
                1,
                128,
            )
            .unwrap(),
        );
        let (tx, rx) = broadcast::channel(4);
        let owner = WebRtcSessionOwner::new(session.clone(), FeatureGuard::new(fc.clone()));
        session.set_frame_receiver(rx);
        session.start_main_loop().unwrap();
        tokio::time::timeout(Duration::from_secs(5), session.handle_offer(&offer))
            .await
            .unwrap()
            .unwrap();
        let (ready, started) = tokio::sync::oneshot::channel();
        let pending = tokio::spawn(async move {
            let _owner = owner;
            let _ = ready.send(());
            std::future::pending::<()>().await;
        });
        started.await.unwrap();
        pending.abort();
        let _ = pending.await;
        assert_eq!(fc.ref_count(), 0);
        assert_eq!(
            *session.connection_state_rx().borrow(),
            gstreamer_webrtc::WebRTCPeerConnectionState::Closed
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while tx.receiver_count() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        peer.close();
    }

    #[tokio::test]
    async fn publish_last_frame_replays_cached_frame_to_new_subscriber() {
        gst::init().unwrap_or(());
        let processor = VideoProcessor::new();
        let buf = gst::Buffer::with_size(4).unwrap();

        processor
            .finish_frame(buf, 1, 1, RawPixelFormat::Rgba)
            .await;

        let mut rx = processor.frame_tx().subscribe();
        assert!(processor.publish_last_frame().await);

        let frame = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.width, 1);
        assert_eq!(frame.height, 1);
        assert_eq!(frame.pixel_format, RawPixelFormat::Rgba);
    }

    #[tokio::test]
    async fn last_trace_id_tracks_new_raw_frames() {
        gst::init().unwrap_or(());
        let processor = VideoProcessor::new();
        assert_eq!(processor.last_trace_id(), 0);

        processor
            .finish_raw_frame(RawFrame {
                data: gst::Buffer::with_size(4).unwrap(),
                width: 1,
                height: 1,
                pixel_format: RawPixelFormat::Rgba,
                trace_id: 42,
                created_at: Instant::now(),
            })
            .await;

        assert_eq!(processor.last_trace_id(), 42);
    }

    #[tokio::test]
    async fn mjpeg_encode_timing_is_exposed_in_trace_snapshot() {
        let processor = VideoProcessor::new();
        processor.record_mjpeg_encode_time(Duration::from_millis(3));

        let trace = processor.trace_snapshot(None).await;
        assert_eq!(trace["mjpeg_encoder"]["encode_count"], 1);
        assert!(
            trace["mjpeg_encoder"]["encode_time_avg_ms"]
                .as_f64()
                .unwrap_or_default()
                >= 3.0
        );
    }
}
