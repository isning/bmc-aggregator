use std::sync::Arc;

use crate::bmc::session::pipeline_manager::PipelineManager;
use crate::bmc::session::registry::DeviceRegistry;
use crate::bmc::stream::pipeline::SharedVideoPipeline;
use crate::bmc::webrtc::config::WebRtcConfig;
use crate::bmc::webrtc::session::WebRtcSession;
use crate::bmc::webrtc::signaling::{IceCandidate, WebRtcOfferRequest, WebRtcOfferResponse};

pub struct WebRtcOrchestrator;

impl WebRtcOrchestrator {
    pub async fn handle_offer(
        registry: &DeviceRegistry,
        pipeline: &PipelineManager,
        device_id: &str,
        offer_sdp: &str,
    ) -> Result<WebRtcOfferResponse, String> {
        let dev = registry.get_dev(device_id).await
            .ok_or_else(|| "device not found".to_string())?;
        let _trigger = dev.acquire("video")?;

        let current_mode = dev.mode.read().await.clone();
        let http_modes = crate::bmc::encoder::registry::EncoderRegistry::http_mode_ids();
        if http_modes.iter().any(|m| **m == *current_mode) {
            return Err(format!("device {} is in HTTP mode ({}), cannot start WebRTC", device_id, current_mode));
        }

        let cfg = WebRtcConfig::default();
        let input_session = dev.get_input().await;

        let pipeline_ref: Option<Arc<SharedVideoPipeline>> = match dev.pipeline.read().await.as_ref().cloned() {
            Some(p) => Some(p),
            None => {
                if let Err(e) = pipeline.set_pipeline_encoder(device_id, &current_mode).await {
                    tracing::error!(
                        "failed to create encoder for device {}, WebRTC data channel still available: {e}",
                        device_id
                    );
                    None
                } else {
                    dev.pipeline.read().await.as_ref().cloned()
                }
            }
        };

        let mut session = WebRtcSession::new(device_id.to_string(), cfg);
        if let Some(ref inp) = input_session {
            session = session.with_input_session(inp.clone());
        }

        if let Some(ref p) = pipeline_ref {
            let pipeline = p.clone();
            session = session
                .with_pipeline(p.tx(), p.codec())
                .with_keyframe_fn(Arc::new(move || pipeline.request_keyframe()));
        }

        let session = Arc::new(session);

        let offer_req = WebRtcOfferRequest {
            sdp: offer_sdp.to_string(),
        };
        let response = session.handle_offer(&offer_req).await?;

        let sid = session.id.clone();

        // Keep the device alive while WebRTC is active
        let hold = dev.acquire("video")?;

        dev.webrtc_sessions.write().await.insert(sid, session);

        // Store the hold guard in the registry's keepalive map
        registry.keepalive_guards.write().await.insert(device_id.to_string(), hold);

        Ok(response)
    }

    pub async fn handle_candidate(
        registry: &DeviceRegistry,
        device_id: &str,
        session_id: &str,
        candidate: &IceCandidate,
    ) -> Result<(), String> {
        let dev = registry.get_dev(device_id).await
            .ok_or_else(|| "device not found".to_string())?;
        let sessions = dev.webrtc_sessions.read().await;
        let session = sessions.get(session_id)
            .ok_or_else(|| "session not found".to_string())?;
        session.add_ice_candidate(candidate).await
    }

    pub async fn handle_close(
        registry: &DeviceRegistry,
        device_id: &str,
        session_id: &str,
    ) -> Result<(), String> {
        let dev = registry.get_dev(device_id).await
            .ok_or_else(|| "device not found".to_string())?;
        {
            let sessions = dev.webrtc_sessions.read().await;
            if let Some(s) = sessions.get(session_id) {
                s.close().await;
            }
        }
        dev.webrtc_sessions.write().await.remove(session_id);

        // Drop the keepalive guard
        registry.keepalive_guards.write().await.remove(device_id);

        Ok(())
    }
}
