use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{broadcast, Mutex, RwLock};
use uuid::Uuid;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::interceptor::registry::Registry;

use super::config::WebRtcConfig;
use super::signaling::*;
use super::video_track::VideoTrack;
use crate::bmc::encoder::Codec;
use crate::bmc::stream::pipeline::EncodedFrame;
use crate::bmc::traits::BmcInputSession;

use crate::bmc::hid_protocol::{parse_hid_message, HidEvent};

#[derive(Clone)]
pub struct WebRtcSession {
    pub id: String,
    _device_id: String,
    pc: Arc<RwLock<Option<Arc<RTCPeerConnection>>>>,
    pending_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>>,
    pipeline_tx: Option<broadcast::Sender<EncodedFrame>>,
    codec: Option<Codec>,
    keyframe_fn: Option<Arc<dyn Fn() + Send + Sync>>,
    config: WebRtcConfig,
    input_session: Option<Arc<dyn BmcInputSession>>,
}

impl WebRtcSession {
    pub fn new(device_id: String, config: WebRtcConfig) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            _device_id: device_id,
            pc: Arc::new(RwLock::new(None)),
            pending_candidates: Arc::new(Mutex::new(Vec::new())),
            pipeline_tx: None,
            codec: None,
            keyframe_fn: None,
            config,
            input_session: None,
        }
    }

    pub fn with_pipeline(mut self, tx: broadcast::Sender<EncodedFrame>, codec: Codec) -> Self {
        self.pipeline_tx = Some(tx);
        self.codec = Some(codec);
        self
    }

    pub fn with_keyframe_fn(mut self, f: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.keyframe_fn = Some(f);
        self
    }

    pub fn with_input_session(mut self, input: Arc<dyn BmcInputSession>) -> Self {
        self.input_session = Some(input);
        self
    }

    pub async fn handle_offer(
        &self,
        offer: &WebRtcOfferRequest,
    ) -> Result<WebRtcOfferResponse, String> {
        let mut m = MediaEngine::default();
        m.register_default_codecs()
            .map_err(|e| format!("register codecs: {e}"))?;

        let registry = Registry::new();
        let registry = register_default_interceptors(registry, &mut m)
            .map_err(|e| format!("interceptors: {e}"))?;

        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_interceptor_registry(registry)
            .build();

        let ice_servers: Vec<RTCIceServer> = self
            .config
            .ice_servers
            .iter()
            .map(|s| RTCIceServer {
                urls: s.urls.clone(),
                username: s.username.clone().unwrap_or_default(),
                credential: s.credential.clone().unwrap_or_default(),
                ..Default::default()
            })
            .collect();

        let config = RTCConfiguration {
            ice_servers,
            ..Default::default()
        };

        let pc = Arc::new(
            api.new_peer_connection(config)
                .await
                .map_err(|e| format!("pc new: {e}"))?,
        );
        *self.pc.write().await = Some(pc.clone());

        // Collect server-side ICE candidates; None signals gathering complete
        let server_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>> =
            Arc::new(Mutex::new(Vec::new()));
        let gather_done = Arc::new(tokio::sync::Notify::new());

        let sc = server_candidates.clone();
        let gd = gather_done.clone();
        pc.on_ice_candidate(Box::new(move |candidate| {
            if let Some(c) = candidate {
                if let Ok(init) = c.to_json() {
                    let sc = sc.clone();
                    tokio::spawn(async move {
                        sc.lock().await.push(init);
                    });
                }
            } else {
                gd.notify_one();
            }
            Box::pin(async {})
        }));

        // Set up data channel handler for keyboard/mouse input
        let input_session = self.input_session.clone();
        let mouse_buttons: Arc<AtomicU8> = Arc::new(AtomicU8::new(0));
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let input = input_session.clone();
            let buttons = mouse_buttons.clone();
            dc.on_message(Box::new(move |msg: DataChannelMessage| {
                if let Some(ref session) = input {
                    let data = msg.data.clone();
                    let s = session.clone();
                    let btns = buttons.clone();
                    tokio::spawn(async move {
                        handle_data_channel_message(s.as_ref(), &data, &btns).await;
                    });
                }
                Box::pin(async {})
            }));
            Box::pin(async {})
        }));

        // Add video track only if a pipeline/codec is available.
        // Errors (unsupported codec, bad encoder, etc.) are logged but
        // never fatal — the data channel (HID) still works.
        if let (Some(tx), Some(codec)) = (self.pipeline_tx.as_ref(), self.codec) {
            if !codec.supports_webrtc() {
                tracing::warn!(
                    "webrtc session {}: {} pipeline ignored (WebRTC requires H.264/H.265/VP8/VP9)",
                    self.id, codec.as_str()
                );
            } else {
                let video_track = VideoTrack::new(tx.subscribe(), codec, &self.config);
                let track = video_track.track();

                match pc.add_track(track).await {
                    Ok(rtp_sender) => {
                        // VideoTrack must not start before add_track — without a bound
                        // track, write_sample returns Ok(()) and silently drops frames.
                        tokio::spawn(video_track.run());

                        // Request a keyframe immediately so the first encoder output
                        // after SRTP becomes ready is a decodable keyframe.
                        if let Some(ref f) = self.keyframe_fn {
                            f();
                        }

                        let kf = self.keyframe_fn.clone();
                        tokio::spawn(async move {
                            loop {
                                match rtp_sender.read_rtcp().await {
                                    Ok((packets, _)) => {
                                        for p in &packets {
                                            let h = p.header();
                                            // PayloadSpecificFeedback (PSFB) = 206
                                            // count=1 → PictureLossIndication, count=4 → FullIntraRequest
                                            if h.packet_type as u8 == 206 && (h.count == 1 || h.count == 4) {
                                                if let Some(ref f) = kf {
                                                    f();
                                                }
                                            }
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!(
                            "webrtc session {}: failed to add video track: {e} — data channel still available",
                            self.id
                        );
                    }
                }
            }
        } else {
            tracing::info!(
                "webrtc session {}: no video pipeline, data channel only",
                self.id
            );
        }

        let remote_desc = RTCSessionDescription::offer(offer.sdp.clone())
            .map_err(|e| format!("invalid offer SDP: {e}"))?;
        pc.set_remote_description(remote_desc)
            .await
            .map_err(|e| format!("set remote desc: {e}"))?;

        let answer = pc
            .create_answer(None)
            .await
            .map_err(|e| format!("create answer: {e}"))?;

        let answer_desc = RTCSessionDescription::answer(answer.sdp.clone())
            .map_err(|e| format!("invalid answer SDP: {e}"))?;
        pc.set_local_description(answer_desc)
            .await
            .map_err(|e| format!("set local desc: {e}"))?;

        // Wait for ICE gathering to finish (up to 3 s)
        tokio::time::timeout(Duration::from_secs(3), gather_done.notified())
            .await
            .ok();

        let candidates: Vec<IceCandidate> = server_candidates
            .lock()
            .await
            .drain(..)
            .map(|c| IceCandidate {
                candidate: c.candidate,
                sdp_mid: c.sdp_mid,
                sdp_mline_index: c.sdp_mline_index,
                username_fragment: c.username_fragment,
            })
            .collect();

        tracing::info!(
            "negotiated WebRTC session {}, gathered {} ICE candidate(s): {:?}",
            self.id,
            candidates.len(),
            candidates.iter().map(|c| &c.candidate).collect::<Vec<_>>()
        );
        tracing::debug!("answer SDP (first 500 chars): {}...", &answer.sdp[..answer.sdp.len().min(500)]);

        Ok(WebRtcOfferResponse {
            session_id: self.id.clone(),
            sdp: answer.sdp,
            candidates,
        })
    }

    pub async fn add_ice_candidate(&self, c: &IceCandidate) -> Result<(), String> {
        let init = RTCIceCandidateInit {
            candidate: c.candidate.clone(),
            sdp_mid: c.sdp_mid.clone(),
            sdp_mline_index: c.sdp_mline_index,
            username_fragment: c.username_fragment.clone(),
        };

        tracing::debug!(
            "add_ice_candidate [{}]: candidate={}, sdp_mid={:?}, sdpMLineIndex={:?}",
            self.id, c.candidate, c.sdp_mid, c.sdp_mline_index
        );

        if let Some(pc) = self.pc.read().await.as_ref() {
            pc.add_ice_candidate(init)
                .await
                .map_err(|e| format!("add ice: {e}"))
        } else {
            tracing::info!(
                "queuing pending candidate for session {} (pc not ready yet)",
                self.id
            );
            self.pending_candidates.lock().await.push(init);
            Ok(())
        }
    }

    pub async fn close(&self) {
        if let Some(pc) = self.pc.read().await.as_ref() {
            let _ = pc.close().await;
        }
    }
}

/// Parse a binary data-channel message and forward it to the BMC input session.
async fn handle_data_channel_message(
    session: &dyn BmcInputSession,
    data: &Bytes,
    mouse_buttons: &AtomicU8,
) {
    let Some(event) = parse_hid_message(data) else {
        if !data.is_empty() {
            tracing::debug!("webrtc unknown data channel message: len={}, type={}", data.len(), data[0]);
        }
        return;
    };

    match event {
        HidEvent::Keyboard { usage, pressed } => {
            if let Err(e) = session.send_keyboard(usage, pressed).await {
                tracing::warn!("webrtc keyboard: {e}");
            }
        }
        HidEvent::MouseMove { x, y } => {
            let mask = mouse_buttons.load(Ordering::Relaxed);
            if let Err(e) = session.send_mouse(x, y, mask, 0).await {
                tracing::warn!("webrtc mouse move: {e}");
            }
        }
        HidEvent::MouseDown { x, y, button } => {
            loop {
                let prev = mouse_buttons.load(Ordering::Relaxed);
                let next = prev | (1u8 << button);
                if mouse_buttons.compare_exchange_weak(prev, next, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                    break;
                }
            }
            let mask = mouse_buttons.load(Ordering::Relaxed);
            if let Err(e) = session.send_mouse(x, y, mask, 0).await {
                tracing::warn!("webrtc mouse button: {e}");
            }
        }
        HidEvent::MouseUp { x, y, button } => {
            loop {
                let prev = mouse_buttons.load(Ordering::Relaxed);
                let next = prev & !(1u8 << button);
                if mouse_buttons.compare_exchange_weak(prev, next, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                    break;
                }
            }
            let mask = mouse_buttons.load(Ordering::Relaxed);
            if let Err(e) = session.send_mouse(x, y, mask, 0).await {
                tracing::warn!("webrtc mouse button: {e}");
            }
        }
        HidEvent::MouseScroll { x, y, delta } => {
            if let Err(e) = session.send_mouse(x, y, 0, delta).await {
                tracing::warn!("webrtc mouse scroll: {e}");
            }
        }
        HidEvent::Consumer { code } => {
            tracing::debug!("webrtc consumer event: usage={}", code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmc::error::SessionError;
    use std::sync::Mutex;

    struct MockSession {
        calls: Arc<Mutex<Vec<String>>>,
    }
    impl MockSession {
        fn new(calls: Arc<Mutex<Vec<String>>>) -> Self {
            Self { calls }
        }
    }

    #[async_trait::async_trait]
    impl BmcInputSession for MockSession {
        async fn send_keyboard(&self, scancode: u16, pressed: bool) -> Result<(), SessionError> {
            self.calls.lock().unwrap().push(format!(
                "kbd scancode=0x{scancode:04X} pressed={pressed}"
            ));
            Ok(())
        }
        async fn send_mouse(
            &self, x: i16, y: i16, button_mask: u8, wheel_delta: i8,
        ) -> Result<(), SessionError> {
            self.calls.lock().unwrap().push(format!(
                "mouse x={x} y={y} buttons=0x{button_mask:02X} wheel={wheel_delta}"
            ));
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_keyboard_down() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        // [0x01, 0x00, usage=0x04 (A), modifier=0]
        let data = Bytes::from_static(&[0x01, 0x00, 0x04, 0x00]);
        handle_data_channel_message(&session, &data, &buttons).await;
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0], "kbd scancode=0x0004 pressed=true");
    }

    #[tokio::test]
    async fn test_keyboard_up() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        // [0x01, KB_EVENT_UP=0x01, usage=0x04 (A), modifier=0]
        let data = Bytes::from_static(&[0x01, 0x01, 0x04, 0x00]);
        handle_data_channel_message(&session, &data, &buttons).await;
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0], "kbd scancode=0x0004 pressed=false");
    }

    #[tokio::test]
    async fn test_mouse_move() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        // [0x02, MS_EVENT_MOVE=0x00, x=100 (i16 LE), y=-50 (i16 LE), button=0]
        let mut buf = vec![0x02, 0x00];
        buf.extend_from_slice(&(100i16).to_le_bytes());
        buf.extend_from_slice(&(-50i16).to_le_bytes());
        buf.push(0x00);
        let data = Bytes::from(buf);
        handle_data_channel_message(&session, &data, &buttons).await;
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0], "mouse x=100 y=-50 buttons=0x00 wheel=0");
    }

    #[tokio::test]
    async fn test_mouse_move_abs() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        // [0x02, MS_EVENT_MOVE_ABS=0x01, x=100, y=200, button=0]
        let mut buf = vec![0x02, 0x01];
        buf.extend_from_slice(&(250i16).to_le_bytes());
        buf.extend_from_slice(&(180i16).to_le_bytes());
        buf.push(0x00);
        let data = Bytes::from(buf);
        handle_data_channel_message(&session, &data, &buttons).await;
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0], "mouse x=250 y=180 buttons=0x00 wheel=0");
    }

    #[tokio::test]
    async fn test_mouse_button_down_tracks_state() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);

        // Left button down (button=0 → bit 0 = 0x01)
        let mut buf = vec![0x02, 0x02]; // MS_EVENT_DOWN
        buf.extend_from_slice(&(10i16).to_le_bytes());
        buf.extend_from_slice(&(20i16).to_le_bytes());
        buf.push(0x00); // button 0
        let data = Bytes::from(buf);
        handle_data_channel_message(&session, &data, &buttons).await;
        assert_eq!(buttons.load(Ordering::Relaxed), 0x01);

        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0], "mouse x=10 y=20 buttons=0x01 wheel=0");
    }

    #[tokio::test]
    async fn test_mouse_button_up_clears_state() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        buttons.store(0x01, Ordering::Relaxed); // Pre-press left button

        // Left button up (button=0 → clear bit 0)
        let mut buf = vec![0x02, 0x03]; // MS_EVENT_UP
        buf.extend_from_slice(&(10i16).to_le_bytes());
        buf.extend_from_slice(&(20i16).to_le_bytes());
        buf.push(0x00); // button 0
        let data = Bytes::from(buf);
        handle_data_channel_message(&session, &data, &buttons).await;
        assert_eq!(buttons.load(Ordering::Relaxed), 0x00);

        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0], "mouse x=10 y=20 buttons=0x00 wheel=0");
    }

    #[tokio::test]
    async fn test_mouse_multiple_buttons_cumulative() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);

        // Left down → mask = 0x01
        let buf = vec![0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00];
        handle_data_channel_message(&session, &Bytes::from(buf), &buttons).await;
        assert_eq!(buttons.load(Ordering::Relaxed), 0x01);

        // Right down → mask = 0x01 | 0x04 = 0x05
        let buf = vec![0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
        handle_data_channel_message(&session, &Bytes::from(buf), &buttons).await;
        assert_eq!(buttons.load(Ordering::Relaxed), 0x05);

        // Left up → mask = 0x05 & ~0x01 = 0x04
        let buf = vec![0x02, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00];
        handle_data_channel_message(&session, &Bytes::from(buf), &buttons).await;
        assert_eq!(buttons.load(Ordering::Relaxed), 0x04);

        // Right up → mask = 0x04 & ~0x04 = 0x00
        let buf = vec![0x02, 0x03, 0x00, 0x00, 0x00, 0x00, 0x02];
        handle_data_channel_message(&session, &Bytes::from(buf), &buttons).await;
        assert_eq!(buttons.load(Ordering::Relaxed), 0x00);

        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 4);
        assert_eq!(log[0], "mouse x=0 y=0 buttons=0x01 wheel=0");
        assert_eq!(log[1], "mouse x=0 y=0 buttons=0x05 wheel=0");
        assert_eq!(log[2], "mouse x=0 y=0 buttons=0x04 wheel=0");
        assert_eq!(log[3], "mouse x=0 y=0 buttons=0x00 wheel=0");
    }

    #[tokio::test]
    async fn test_mouse_scroll() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        // [0x02, MS_EVENT_SCROLL=0x04, x=0, y=0, scroll=3]
        let mut buf = vec![0x02, 0x04];
        buf.extend_from_slice(&(0i16).to_le_bytes());
        buf.extend_from_slice(&(0i16).to_le_bytes());
        buf.push(3u8); // scroll delta = 3
        let data = Bytes::from(buf);
        handle_data_channel_message(&session, &data, &buttons).await;
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0], "mouse x=0 y=0 buttons=0x00 wheel=3");
    }

    #[tokio::test]
    async fn test_scroll_negative_delta() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        let mut buf = vec![0x02, 0x04];
        buf.extend_from_slice(&(5i16).to_le_bytes());
        buf.extend_from_slice(&(10i16).to_le_bytes());
        buf.push(0xFEu8); // scroll delta = -2 as i8
        let data = Bytes::from(buf);
        handle_data_channel_message(&session, &data, &buttons).await;
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0], "mouse x=5 y=10 buttons=0x00 wheel=-2");
    }

    #[tokio::test]
    async fn test_empty_data_is_noop() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        handle_data_channel_message(&session, &Bytes::new(), &buttons).await;
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_short_mouse_message_is_noop() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        // Only 3 bytes for a mouse message — too short
        let data = Bytes::from_static(&[0x02, 0x00, 0x01]);
        handle_data_channel_message(&session, &data, &buttons).await;
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_short_keyboard_message_is_noop() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        // Only 2 bytes for keyboard — too short
        let data = Bytes::from_static(&[0x01, 0x00]);
        handle_data_channel_message(&session, &data, &buttons).await;
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_unknown_message_type_is_noop() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        let data = Bytes::from_static(&[0xFF, 0x01, 0x02, 0x03]);
        handle_data_channel_message(&session, &data, &buttons).await;
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_keyboard_with_modifier() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        // A key with Left Ctrl modifier (byte 3 = 0x01)
        let data = Bytes::from_static(&[0x01, 0x00, 0x04, 0x01]);
        handle_data_channel_message(&session, &data, &buttons).await;
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0], "kbd scancode=0x0004 pressed=true");
        // Modifier byte is handled as separate key event, not embedded
    }

    #[tokio::test]
    async fn test_consumer_message_is_noop() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = MockSession::new(calls.clone());
        let buttons = AtomicU8::new(0);
        let data = Bytes::from_static(&[0x03, 0xCD]);
        handle_data_channel_message(&session, &data, &buttons).await;
        // Consumer events are logged but not forwarded
        assert!(calls.lock().unwrap().is_empty());
    }

    // ── HID error resilience ──────────────────────────────────────

    struct FailingSession {
        fail_keyboard: bool,
        fail_mouse: bool,
        calls: Arc<Mutex<Vec<String>>>,
    }
    impl FailingSession {
        fn fail_all(calls: Arc<Mutex<Vec<String>>>) -> Self {
            Self { fail_keyboard: true, fail_mouse: true, calls }
        }
    }

    #[async_trait::async_trait]
    impl BmcInputSession for FailingSession {
        async fn send_keyboard(&self, scancode: u16, pressed: bool) -> Result<(), SessionError> {
            self.calls.lock().unwrap().push(format!(
                "kbd scancode=0x{scancode:04X} pressed={pressed}"
            ));
            if self.fail_keyboard {
                Err(SessionError::Other("mock keyboard error".into()))
            } else {
                Ok(())
            }
        }
        async fn send_mouse(
            &self, x: i16, y: i16, button_mask: u8, wheel_delta: i8,
        ) -> Result<(), SessionError> {
            self.calls.lock().unwrap().push(format!(
                "mouse x={x} y={y} buttons=0x{button_mask:02X} wheel={wheel_delta}"
            ));
            if self.fail_mouse {
                Err(SessionError::Other("mock mouse error".into()))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn test_hid_keyboard_error_does_not_panic() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = FailingSession::fail_all(calls.clone());
        let buttons = AtomicU8::new(0);
        // Two keyboard events — first should log error but second must still execute
        let d1 = Bytes::from_static(&[0x01, 0x00, 0x04, 0x00]);
        let d2 = Bytes::from_static(&[0x01, 0x00, 0x05, 0x00]);
        handle_data_channel_message(&session, &d1, &buttons).await;
        handle_data_channel_message(&session, &d2, &buttons).await;
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0], "kbd scancode=0x0004 pressed=true");
        assert_eq!(log[1], "kbd scancode=0x0005 pressed=true");
    }

    #[tokio::test]
    async fn test_hid_mouse_error_does_not_panic() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = FailingSession::fail_all(calls.clone());
        let buttons = AtomicU8::new(0);
        // Two mouse moves — first should log error but second must still execute
        let mut b1 = vec![0x02, 0x00];
        b1.extend_from_slice(&(10i16).to_le_bytes());
        b1.extend_from_slice(&(20i16).to_le_bytes());
        b1.push(0x00);
        let mut b2 = vec![0x02, 0x00];
        b2.extend_from_slice(&(30i16).to_le_bytes());
        b2.extend_from_slice(&(40i16).to_le_bytes());
        b2.push(0x00);
        handle_data_channel_message(&session, &Bytes::from(b1), &buttons).await;
        handle_data_channel_message(&session, &Bytes::from(b2), &buttons).await;
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0], "mouse x=10 y=20 buttons=0x00 wheel=0");
        assert_eq!(log[1], "mouse x=30 y=40 buttons=0x00 wheel=0");
    }

    #[tokio::test]
    async fn test_hid_error_mixed_keyboard_mouse_continues() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = FailingSession::fail_all(calls.clone());
        let buttons = AtomicU8::new(0);
        // Interleave keyboard and mouse — all should be attempted
        let k = Bytes::from_static(&[0x01, 0x00, 0x04, 0x00]);
        let m = vec![0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        handle_data_channel_message(&session, &k, &buttons).await;
        handle_data_channel_message(&session, &Bytes::from(m.clone()), &buttons).await;
        handle_data_channel_message(&session, &k, &buttons).await;
        let log = calls.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0], "kbd scancode=0x0004 pressed=true");
        assert_eq!(log[1], "mouse x=0 y=0 buttons=0x00 wheel=0");
        assert_eq!(log[2], "kbd scancode=0x0004 pressed=true");
    }

    // ── Encoder / pipeline resilience (WebRtcSession struct) ──────

    #[test]
    fn test_session_without_pipeline_has_no_video() {
        let session = WebRtcSession::new("no-video".into(), WebRtcConfig::default());
        assert!(!session.id.is_empty());
        assert!(session.pipeline_tx.is_none());
        assert!(session.codec.is_none());
        assert!(session.input_session.is_none());
    }

    #[test]
    fn test_session_with_input_only_works() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let session = WebRtcSession::new("input-only".into(), WebRtcConfig::default())
            .with_input_session(Arc::new(MockSession::new(calls.clone())));
        assert!(session.input_session.is_some());
        assert!(session.pipeline_tx.is_none());
    }

    #[test]
    fn test_session_with_frame_codec_pipeline_stores_it() {
        let (tx, _) = broadcast::channel(16);
        let session = WebRtcSession::new("frame-codec-test".into(), WebRtcConfig::default())
            .with_pipeline(tx, Codec::Mjpeg);
        assert_eq!(session.codec, Some(Codec::Mjpeg));
        assert!(session.pipeline_tx.is_some());
    }

    #[test]
    fn test_session_clone_works() {
        let (tx, _) = broadcast::channel(16);
        let a = WebRtcSession::new("a".into(), WebRtcConfig::default())
            .with_pipeline(tx, Codec::H264);
        let b = a.clone();
        assert_eq!(b.id, a.id);
        assert_eq!(b.codec, a.codec);
    }

    #[test]
    fn test_session_close_no_panic() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let session = WebRtcSession::new("close-test".into(), WebRtcConfig::default());
            // close() with no peer connection should not panic
            session.close().await;
        });
    }
}
