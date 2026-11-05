use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::bmc::encoder::registry::probe_webrtc_codec_for_backend;
use crate::bmc::encoder::Codec;
use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::fault::{
    acquisition_fault_response, fault_response, record_fault_response, unavailable_fault_response,
    FeatureFault, FeatureFaultDomain,
};
use crate::bmc::feature::video::encoding::normalize_stream_backend;
use crate::bmc::feature::video::replay_initial_base_frame;
use crate::bmc::feature::video::runtime;
use crate::bmc::feature::{acquire_connected_feature, feature_connect_status};
use crate::bmc::stream::gst_webrtc::GstWebRtcSession;
use crate::bmc::stream::pipeline::RawPixelFormat;
use crate::web::state::AppState;

async fn get_existing_video_connection(
    state: &Arc<AppState>,
    bmc_id: &str,
    connection_id: Option<&str>,
) -> Result<Arc<FeatureConnection>, String> {
    let dev = state
        .registry
        .get_dev(bmc_id)
        .await
        .ok_or_else(|| "device not found".to_string())?;

    match connection_id {
        Some(id) => dev
            .get_instance("video", id)
            .await
            .ok_or_else(|| format!("video connection '{id}' not found")),
        None => dev
            .features
            .get("video")
            .cloned()
            .ok_or_else(|| "video not available".to_string()),
    }
}

#[derive(Deserialize)]
pub struct WebrtcOffer {
    sdp: String,
    connection_id: Option<String>,
    codec: Option<String>,
    encoder: Option<String>,
}

#[derive(Deserialize)]
pub struct IceCandidateInner {
    candidate: String,
    #[serde(rename = "sdpMLineIndex")]
    sdp_mline_index: Option<u16>,
}

#[derive(Deserialize)]
pub struct WebrtcIce {
    candidate: IceCandidateInner,
    connection_id: Option<String>,
}

#[derive(Deserialize)]
pub struct PipelineEncoderBody {
    encoder: String,
}

#[derive(Deserialize, Default)]
pub struct ConnectionQuery {
    connection_id: Option<String>,
}

fn webrtc_error_status(error: &str) -> StatusCode {
    if error.contains("instance limit") {
        StatusCode::TOO_MANY_REQUESTS
    } else if error.contains("not found") {
        StatusCode::NOT_FOUND
    } else if error.contains("not available") {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        feature_connect_status(error)
    }
}

fn webrtc_request_fault(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
    operation: &'static str,
) -> Response {
    fault_response(
        status,
        FeatureFault::operation(
            FeatureFaultDomain::BrowserTransport,
            code,
            message,
            retryable,
            operation,
        ),
    )
}

async fn webrtc_runtime_unavailable(fc: &FeatureConnection, operation: &'static str) -> Response {
    record_fault_response(
        fc,
        StatusCode::SERVICE_UNAVAILABLE,
        FeatureFault::operation(
            FeatureFaultDomain::Lifecycle,
            "backend_runtime_unavailable",
            "The video backend runtime is not available",
            true,
            operation,
        ),
    )
    .await
}

async fn resolve_initial_frame_metadata(
    processor: &crate::bmc::feature::video::processor::VideoProcessor,
) -> (u32, u32, RawPixelFormat) {
    if let Some(meta) = processor.first_frame_metadata().await {
        return meta;
    }

    let mut rx = processor.frame_tx().subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(meta) = processor.first_frame_metadata().await {
            return meta;
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let wait = (deadline - now).min(Duration::from_millis(50));
        if let Ok(Ok(frame)) = tokio::time::timeout(wait, rx.recv()).await {
            return (frame.width, frame.height, frame.pixel_format);
        }
    }

    (1920, 1080, RawPixelFormat::Rgba)
}

fn schedule_initial_base_frames(
    processor: Arc<crate::bmc::feature::video::processor::VideoProcessor>,
    session: Arc<GstWebRtcSession>,
) {
    tokio::spawn(async move {
        for delay_ms in [0u64, 100, 300, 800, 1500] {
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            if let Some(frame) = processor.last_frame().await {
                session.push_base_frame(frame);
                processor.publish_last_frame().await;
            } else {
                processor.publish_last_frame().await;
            }
        }
    });
}

pub async fn webrtc_offer(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<WebrtcOffer>,
) -> Response {
    let connection_id = payload
        .connection_id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    if connection_id.len() > 128
        || !connection_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return webrtc_request_fault(
            StatusCode::BAD_REQUEST,
            "invalid_connection_id",
            "Connection ID must be a bounded ASCII identifier",
            false,
            "webrtc_offer",
        );
    }
    let fc = match state
        .registry
        .get_or_create_feature_instance(
            &bmc_id,
            "video",
            &connection_id,
            crate::bmc::feature::registered_backends(),
        )
        .await
    {
        Ok(fc) => fc,
        Err(e) => {
            return webrtc_request_fault(
                webrtc_error_status(&e),
                if e.contains("not found") {
                    "device_not_found"
                } else {
                    "video_unavailable"
                },
                if e.contains("not found") {
                    "The requested BMC device was not found"
                } else {
                    "The video feature is not available for this device"
                },
                false,
                "webrtc_offer",
            )
        }
    };

    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(30)).await {
        Ok(guard) => guard,
        Err(error) => {
            return acquisition_fault_response(&fc, feature_connect_status(&error), &error).await
        }
    };

    let processor = match runtime::processor(&fc).await {
        Ok(processor) => processor,
        Err(_) => return webrtc_runtime_unavailable(&fc, "webrtc_offer").await,
    };
    let frame_tx = processor.frame_tx();
    replay_initial_base_frame(
        &fc,
        &processor,
        &format!("[{bmc_id}:{connection_id}] webrtc"),
    )
    .await;
    let (default_codec, default_backend) =
        processor.streaming_selection_or_default(Codec::H264).await;
    let codec = match payload.codec.as_deref() {
        Some(value) => {
            match Codec::from_str(value.trim()).filter(|codec| codec.supports_webrtc()) {
                Some(codec) => codec,
                None => {
                    return webrtc_request_fault(
                        StatusCode::BAD_REQUEST,
                        "unsupported_codec",
                        "The requested streaming codec is unsupported",
                        false,
                        "webrtc_offer",
                    )
                }
            }
        }
        None => default_codec,
    };
    let backend = payload
        .encoder
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(normalize_stream_backend)
        .unwrap_or(default_backend.as_str())
        .to_string();
    let (w, h, pixel_format) = resolve_initial_frame_metadata(&processor).await;
    let framerate = 30u32;
    let bitrate = 5000u32;

    if !probe_webrtc_codec_for_backend(&backend, codec) {
        let fault = FeatureFault::child(
            FeatureFaultDomain::BrowserTransport,
            "encoder_unavailable",
            "The selected WebRTC encoder is unavailable",
            false,
            "webrtc",
            connection_id.clone(),
        );
        processor.record_fault(fault.clone()).await;
        return fault_response(StatusCode::SERVICE_UNAVAILABLE, fault);
    }
    let session = match GstWebRtcSession::new_with_pixel_format(
        connection_id.clone(),
        Some("stun:stun.l.google.com:19302".into()),
        codec,
        &backend,
        w,
        h,
        pixel_format,
        framerate,
        bitrate,
    ) {
        Ok(s) => Arc::new(s),
        Err(_) => {
            let fault = FeatureFault::child(
                FeatureFaultDomain::BrowserTransport,
                "webrtc_session_failed",
                "WebRTC video session creation failed",
                true,
                "webrtc",
                connection_id.clone(),
            );
            processor.record_fault(fault.clone()).await;
            return fault_response(StatusCode::SERVICE_UNAVAILABLE, fault);
        }
    };

    let owner = super::processor::WebRtcSessionOwner::new(session.clone(), guard);
    let rx = frame_tx.subscribe();
    session.set_frame_receiver(rx);
    if let Err(e) = session.start_main_loop() {
        // Keep the detailed GStreamer cause in server diagnostics only.  The
        // browser receives the stable sanitized fault below.
        let diagnostic = format!("start session: {e}");
        tracing::warn!(connection_id = %connection_id, error = %diagnostic, "WebRTC session startup failed");
        session.close();
        let fault = FeatureFault::child(
            FeatureFaultDomain::BrowserTransport,
            "webrtc_session_failed",
            "WebRTC video session startup failed",
            true,
            "webrtc",
            connection_id.clone(),
        );
        processor.record_fault(fault.clone()).await;
        return fault_response(StatusCode::SERVICE_UNAVAILABLE, fault);
    }

    let (answer_sdp, candidates) = match session.handle_offer(&payload.sdp).await {
        Ok(result) => result,
        Err(_) => {
            session.close();
            let fault = FeatureFault::child(
                FeatureFaultDomain::BrowserTransport,
                "webrtc_negotiation_failed",
                "WebRTC offer negotiation failed",
                true,
                "webrtc",
                connection_id.clone(),
            );
            processor.record_fault(fault.clone()).await;
            return fault_response(StatusCode::SERVICE_UNAVAILABLE, fault);
        }
    };
    if let Some(frame) = processor.last_frame().await {
        session.push_base_frame(frame);
    }
    processor.publish_last_frame().await;

    processor.set_webrtc_connection(owner).await;
    schedule_initial_base_frames(processor, session);

    let candidates_json: Vec<serde_json::Value> = candidates
        .iter()
        .map(|c| {
            serde_json::json!({
                "candidate": c.candidate,
                "sdpMid": c.sdp_mid,
                "sdpMLineIndex": c.sdp_mline_index,
            })
        })
        .collect();

    Json(serde_json::json!({
        "type": "answer",
        "connection_id": connection_id,
        "sdp": answer_sdp,
        "ice_candidates": candidates_json,
    }))
    .into_response()
}

pub async fn webrtc_ice(
    Path(bmc_id): Path<String>,
    Query(query): Query<ConnectionQuery>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<WebrtcIce>,
) -> Response {
    let Some(connection_id) = payload.connection_id.or(query.connection_id) else {
        return webrtc_request_fault(
            StatusCode::BAD_REQUEST,
            "connection_id_required",
            "A WebRTC connection identifier is required",
            false,
            "webrtc_ice",
        );
    };
    let fc = match get_existing_video_connection(&state, &bmc_id, Some(&connection_id)).await {
        Ok(fc) => fc,
        Err(e) => {
            return webrtc_request_fault(
                webrtc_error_status(&e),
                "connection_not_found",
                "The requested WebRTC connection was not found",
                false,
                "webrtc_ice",
            )
        }
    };

    let processor = match runtime::processor(&fc).await {
        Ok(processor) => processor,
        Err(_) => return webrtc_runtime_unavailable(&fc, "webrtc_ice").await,
    };
    let session = processor.get_webrtc_connection(&connection_id).await;

    match session {
        Some(s) => {
            let _ = s.add_ice_candidate(
                &payload.candidate.candidate,
                payload.candidate.sdp_mline_index.unwrap_or(0),
            );
            Json(serde_json::json!({"ok": true, "success": true})).into_response()
        }
        None => {
            let fault = FeatureFault::child(
                FeatureFaultDomain::BrowserTransport,
                "webrtc_session_not_found",
                "The requested WebRTC session is not active",
                false,
                "webrtc",
                connection_id,
            );
            processor.record_fault(fault.clone()).await;
            fault_response(StatusCode::NOT_FOUND, fault)
        }
    }
}

pub async fn webrtc_close(
    Path(bmc_id): Path<String>,
    Query(query): Query<ConnectionQuery>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let connection_id = query.connection_id;
    let fc = match get_existing_video_connection(&state, &bmc_id, connection_id.as_deref()).await {
        Ok(fc) => fc,
        Err(_) => return Json(serde_json::json!({"ok": true, "success": true})).into_response(),
    };

    if let Ok(processor) = runtime::processor(&fc).await {
        if let Some(connection_id) = connection_id {
            if let Some(session) = processor.get_webrtc_connection(&connection_id).await {
                session.close();
                processor.clear_webrtc_connection(&connection_id).await;
            }
        } else {
            processor.close_all_webrtc_connections().await;
        }
    }

    Json(serde_json::json!({"ok": true, "success": true})).into_response()
}

pub async fn webrtc_ice_servers(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    use crate::bmc::feature::video::ice_config::WebRtcConfig;
    let config = WebRtcConfig::default();
    let ice_servers = config
        .ice_servers
        .iter()
        .map(|s| {
            serde_json::json!({
                "urls": s.urls,
                "username": s.username,
                "credential": s.credential,
            })
        })
        .collect::<Vec<_>>();
    Json(serde_json::json!({
        "ice_servers": ice_servers,
        "iceServers": ice_servers,
        "mdns_mode": "disabled",
    }))
}

pub async fn pipeline_set_encoder(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<PipelineEncoderBody>,
) -> Response {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return webrtc_request_fault(
            StatusCode::NOT_FOUND,
            "device_not_found",
            "The requested BMC device was not found",
            false,
            "set_encoder",
        );
    };
    let Some(fc) = dev.features.get("video").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
    };
    let processor = match runtime::processor(&fc).await {
        Ok(processor) => processor,
        Err(_) => return webrtc_runtime_unavailable(&fc, "set_encoder").await,
    };
    processor.set_encoder_backend(payload.encoder.clone()).await;
    Json(serde_json::json!({"ok": true, "encoder": payload.encoder})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gstreamer as gst;
    use std::time::Instant;

    use crate::bmc::feature::video::processor::VideoProcessor;
    use crate::bmc::stream::pipeline::RawFrame;

    #[tokio::test]
    async fn schedule_initial_base_frames_retries_cached_frame() {
        gst::init().unwrap_or(());
        let processor = Arc::new(VideoProcessor::new());
        let session = Arc::new(
            GstWebRtcSession::new(
                "retry-base".into(),
                None,
                Codec::H264,
                "software",
                16,
                16,
                30,
                500,
            )
            .expect("create h264 webrtc session"),
        );
        session.start_main_loop().expect("start main loop");
        tokio::time::sleep(Duration::from_millis(100)).await;

        processor
            .finish_raw_frame(RawFrame {
                data: gst::Buffer::from_mut_slice(vec![0u8; 16 * 16 * 4]),
                width: 16,
                height: 16,
                pixel_format: RawPixelFormat::Rgba,
                trace_id: 99,
                created_at: Instant::now(),
            })
            .await;

        schedule_initial_base_frames(processor, session.clone());
        tokio::time::sleep(Duration::from_millis(450)).await;

        let snap = session.trace_snapshot();
        assert!(
            snap["frames"]["pushed"].as_u64().unwrap_or(0) >= 3,
            "base frame should be pushed more than once: {snap}"
        );
        assert_eq!(snap["frames"]["last_trace_id"], 99);
        session.close();
    }
}
