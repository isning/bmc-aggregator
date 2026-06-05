use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tracing::warn;

use crate::bmc::webrtc::config::WebRtcConfig;
use crate::bmc::webrtc::signaling::WebRtcIcePayload;
use crate::web::handlers::AppState;

#[derive(Deserialize)]
pub struct WebRtcClosePayload {
    pub session_id: String,
}

#[derive(Deserialize)]
pub struct WebRtcOfferPayload {
    pub sdp: String,
}

#[derive(Deserialize)]
pub struct PipelineEncoderPayload {
    pub encoder: String,
}

pub async fn webrtc_offer(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<WebRtcOfferPayload>,
) -> Response {
    tracing::info!("[{}] webrtc_offer: SDP length={}", bmc_id, payload.sdp.len());
    tracing::debug!("[{}] webrtc_offer SDP (first 300 chars): {}...", bmc_id, &payload.sdp[..payload.sdp.len().min(300)]);
    match state.orchestrator.webrtc_offer(&bmc_id, &payload.sdp).await {
        Ok(resp) => {
            tracing::info!("[{}] webrtc_offer success: session={}, candidates={}", bmc_id, resp.session_id, resp.candidates.len());
            (StatusCode::OK, Json(serde_json::json!(resp))).into_response()
        }
        Err(e) => {
            warn!("[{}] webrtc offer error: {}", bmc_id, e);
            (StatusCode::OK, Json(serde_json::json!({"success": false, "message": e}))).into_response()
        }
    }
}

pub async fn webrtc_ice(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<WebRtcIcePayload>,
) -> Response {
    tracing::debug!("[{}] webrtc_ice: session={}, candidate={}", bmc_id, payload.session_id, payload.candidate.candidate);
    match state.orchestrator.webrtc_candidate(&bmc_id, &payload.session_id, &payload.candidate).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true}))).into_response(),
        Err(e) => {
            warn!("[{}] webrtc ice error: {}", bmc_id, e);
            (StatusCode::OK, Json(serde_json::json!({"success": false, "message": e}))).into_response()
        }
    }
}

pub async fn webrtc_close(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<WebRtcClosePayload>,
) -> Response {
    match state.orchestrator.webrtc_close(&bmc_id, &payload.session_id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true, "message": "Session closed"}))).into_response(),
        Err(e) => {
            warn!("[{}] webrtc close error: {}", bmc_id, e);
            (StatusCode::OK, Json(serde_json::json!({"success": false, "message": e}))).into_response()
        }
    }
}

pub async fn webrtc_session_create() -> Json<serde_json::Value> {
    Json(serde_json::json!({"session_id": uuid::Uuid::new_v4().to_string()}))
}

pub async fn webrtc_status() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "sessions": [],
        "active": false,
    }))
}

pub async fn webrtc_ice_servers(
    State(_state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let cfg = WebRtcConfig::default();
    let servers: Vec<serde_json::Value> = cfg.ice_servers.iter().map(|s| {
        serde_json::json!({
            "urls": s.urls,
            "username": s.username,
            "credential": s.credential,
        })
    }).collect();
    Json(serde_json::json!({
        "ice_servers": servers,
        "mdns_mode": "disabled",
    }))
}

pub async fn pipeline_set_encoder(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<PipelineEncoderPayload>,
) -> Response {
    match state.orchestrator.set_pipeline_encoder(&bmc_id, &payload.encoder).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "ok", "encoder": payload.encoder}))).into_response(),
        Err(e) => {
            warn!("[{}] set pipeline encoder error: {}", bmc_id, e);
            (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e}))).into_response()
        }
    }
}
