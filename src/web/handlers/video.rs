use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tracing::warn;

use crate::bmc::encoder::registry::EncoderRegistry;
use crate::web::handlers::AppState;

pub async fn video_devices(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let info = state.orchestrator.device_info(&bmc_id).await;
    let (cur_w, cur_h) = info.map(|i| (i.width, i.height)).unwrap_or((1920, 1080));
    Json(serde_json::json!({
        "video": [
            {
                "path": "virtual",
                "name": "Virtual (Network)",
                "driver": "mahogany",
                "formats": [
                    {
                        "format": "MJPEG",
                        "description": "Motion-JPEG",
                        "resolutions": [
                            {"width": cur_w, "height": cur_h, "fps": [30.0]}
                        ]
                    }
                ],
                "usb_bus": null,
                "has_signal": true
            }
        ]
    }))
}

pub async fn encoder_self_check(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let encoder_error = state.orchestrator.encoder_error(&bmc_id).await;
    let (_, encoder_backend, _) = state
        .orchestrator
        .backend_info(&bmc_id)
        .await
        .unwrap_or_else(|| ("mahogany".to_string(), "software".to_string(), None));

    let info = state.orchestrator.device_info(&bmc_id).await;
    let (cur_w, cur_h) = info.map(|i| (i.width, i.height)).unwrap_or((1920, 1080));

    let backends = EncoderRegistry::discover();
    let codecs: Vec<serde_json::Value> = backends.iter().map(|b| serde_json::json!({
        "id": b.id,
        "name": b.name,
        "available": b.available,
        "is_hardware": b.is_hardware,
    })).collect();

    let rows: Vec<serde_json::Value> = backends.iter().map(|b| serde_json::json!({
        "resolution_id": format!("{}x{}", cur_w, cur_h),
        "resolution_label": format!("{}x{}", cur_w, cur_h),
        "width": cur_w,
        "height": cur_h,
        "cells": [{
            "codec_id": b.id,
            "ok": encoder_error.is_none() && b.available,
            "elapsed_ms": 8.0,
        }],
    })).collect();

    Json(serde_json::json!({
        "current_hardware_encoder": encoder_backend,
        "codecs": codecs,
        "rows": rows,
        "encoder_error": encoder_error,
    }))
}

#[derive(Deserialize)]
pub struct StreamConfigPatch {
    pub encoder: Option<String>,
}

pub async fn stream_config_patch(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(patch): Json<StreamConfigPatch>,
) -> Json<serde_json::Value> {
    if let Some(ref encoder) = patch.encoder {
        let _ = state.orchestrator.set_encoder_backend(&bmc_id, encoder.clone()).await;
        state.orchestrator.set_encoder_runtime(&bmc_id, encoder).await;
    }

    let (_video_backend, encoder_backend, _audio_backend) = state
        .orchestrator
        .backend_info(&bmc_id)
        .await
        .unwrap_or_else(|| ("mahogany".to_string(), "software".to_string(), None));

    Json(serde_json::json!({
        "encoder": encoder_backend,
    }))
}

pub async fn stream_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let info = state.orchestrator.device_info(&bmc_id).await;
    let encoder_error = state.orchestrator.encoder_error(&bmc_id).await;
    let (clients, fps) = if let Some(session) = state.orchestrator.get_session(&bmc_id).await {
        session.base.as_ref().map(|b| (b.clients(), b.fps())).unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    match info {
        Some(i) => Json(serde_json::json!({
            "online": i.online,
            "state": if i.online { "streaming" } else { "uninitialized" },
            "device": null,
            "format": "mjpeg",
            "resolution": [i.width, i.height],
            "clients": clients,
            "target_fps": 30,
            "fps": fps,
            "encoder_error": encoder_error,
        })),
        None => Json(serde_json::json!({
            "online": false,
            "state": "uninitialized",
            "device": null,
            "format": serde_json::Value::Null,
            "resolution": serde_json::Value::Null,
            "clients": 0,
            "target_fps": 0,
            "fps": 0,
            "encoder_error": serde_json::Value::Null,
        })),
    }
}

pub async fn stream_codecs(
    State(_state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let backends: Vec<serde_json::Value> = EncoderRegistry::discover()
        .iter()
        .map(|b| {
            let formats: Vec<&str> = crate::bmc::encoder::codec::codecs_for_backend(b.id);
            serde_json::json!({
                "id": b.id,
                "name": b.name,
                "is_hardware": b.is_hardware,
                "available": b.available,
                "supported_formats": formats,
            })
        })
        .collect();
    let codecs: Vec<serde_json::Value> = EncoderRegistry::codecs()
        .iter()
        .map(|c| serde_json::json!({
            "id": c.id,
            "name": c.name,
            "protocol": c.protocol,
            "hardware": false,
            "backend": "software",
            "mime": c.mime,
            "available": true,
        }))
        .collect();
    Json(serde_json::json!({
        "success": true,
        "backends": backends,
        "codecs": codecs,
    }))
}

pub async fn stream_config_get(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let info = state.orchestrator.device_info(&bmc_id).await;
    let mode = state.orchestrator.device_mode(&bmc_id).await.unwrap_or_else(|| "mjpeg".to_string());
    let protocol = EncoderRegistry::find_codec(&mode)
        .map(|c| c.protocol)
        .unwrap_or("http");
    let (video_backend, encoder_backend, audio_backend) = state.orchestrator.backend_info(&bmc_id).await
        .unwrap_or(("mahogany".into(), "software".into(), None));
    Json(serde_json::json!({
        "mode": mode,
        "protocol": protocol,
        "encoder": encoder_backend,
        "video_backend": video_backend,
        "audio_backend": audio_backend,
        "bitrate": 2000,
        "online": info.as_ref().map(|i| i.online).unwrap_or(false),
        "width": info.as_ref().map(|i| i.width).unwrap_or(0u16),
        "height": info.as_ref().map(|i| i.height).unwrap_or(0u16),
        "webrtc_available": true,
    }))
}

pub async fn stream_mode(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let mode = state.orchestrator.device_mode(&bmc_id).await.unwrap_or_else(|| "mjpeg".to_string());
    let protocol = EncoderRegistry::find_codec(&mode)
        .map(|c| c.protocol)
        .unwrap_or("http");
    Json(serde_json::json!({
        "success": true,
        "mode": mode,
        "protocol": protocol,
    }))
}

#[derive(Deserialize)]
pub struct StreamModePayload {
    pub mode: String,
}

pub async fn stream_mode_post(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<StreamModePayload>,
) -> Response {
    let is_http = EncoderRegistry::find_codec(&payload.mode)
        .map_or(false, |c| c.protocol == "http");

    let transition_id = if !is_http {
        Some(uuid::Uuid::new_v4().to_string())
    } else {
        None
    };

    match state.orchestrator.configure_stream(&bmc_id, &payload.mode).await {
        Ok(()) => {
            let protocol = if is_http { "http" } else { "webrtc" };
            let mut resp = serde_json::json!({
                "success": true,
                "mode": payload.mode,
                "protocol": protocol,
            });
            if let Some(ref tid) = transition_id {
                resp["transition_id"] = serde_json::json!(tid);

                tracing::info!("[{}] emitting stream.mode_ready and stream.webrtc_ready for {}", bmc_id, payload.mode);

                let mode_ready = serde_json::json!({
                    "event": "stream.mode_ready",
                    "data": {
                        "transition_id": tid,
                        "mode": payload.mode,
                    },
                });
                let _ = state.event_tx.send(mode_ready.to_string());

                let webrtc_ready = serde_json::json!({
                    "event": "stream.webrtc_ready",
                    "data": {
                        "codec": payload.mode,
                        "hardware": false,
                        "transition_id": tid,
                    },
                });
                let _ = state.event_tx.send(webrtc_ready.to_string());
            }
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => {
            warn!("[{}] set stream mode error: {}", bmc_id, e);
            (StatusCode::OK, Json(serde_json::json!({"success": false, "message": e}))).into_response()
        }
    }
}

pub async fn stream_start(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    match state.orchestrator.acquire(&bmc_id, "video").await {
        Ok(_) => Json(serde_json::json!({"status": "ok", "message": "stream started"})),
        Err(e) => {
            warn!("[{}] stream_start: failed: {}", bmc_id, e);
            Json(serde_json::json!({"status": "error", "message": e}))
        }
    }
}
pub async fn stream_stop(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let _ = state.orchestrator.stop_pipeline(&bmc_id).await;
    Json(serde_json::json!({"status": "ok"}))
}

pub async fn stream_constraints(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let info = state.orchestrator.device_info(&bmc_id).await;
    let (width, height) = info.map(|i| (i.width, i.height)).unwrap_or((1024, 768));
    let current_mode = state.orchestrator.device_mode(&bmc_id).await.unwrap_or_else(|| "mjpeg".to_string());
    Json(serde_json::json!({
        "success": true,
        "allowed_codecs": EncoderRegistry::codec_ids(),
        "http_modes": EncoderRegistry::http_mode_ids(),
        "locked_codec": null,
        "disallow_mjpeg": false,
        "sources": {"rustdesk": false, "rtsp": false},
        "reason": null,
        "current_mode": current_mode,
        "current_resolution": [width, height]
    }))
}

// TODO: Implement real bitrate/flow control. Remove this no-op endpoint when
// actual stream quality control is implemented. Keeping the route registered
// so the frontend doesn't break during the transition.
pub async fn stream_bitrate() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

// Exported for testing — constructs and sends stream.mode_ready and stream.webrtc_ready
// WebSocket events through the app-level broadcast channel.
pub fn emit_webrtc_mode_events(
    event_tx: &tokio::sync::broadcast::Sender<String>,
    tid: &str,
    mode: &str,
) {
    let mode_ready = serde_json::json!({
        "event": "stream.mode_ready",
        "data": {
            "transition_id": tid,
            "mode": mode,
        },
    });
    let _ = event_tx.send(mode_ready.to_string());

    let webrtc_ready = serde_json::json!({
        "event": "stream.webrtc_ready",
        "data": {
            "codec": mode,
            "hardware": false,
            "transition_id": tid,
        },
    });
    let _ = event_tx.send(webrtc_ready.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emit_webrtc_mode_events_format() {
        let (tx, mut rx) = tokio::sync::broadcast::channel::<String>(16);

        emit_webrtc_mode_events(&tx, "test-tid-123", "vp8");

        // First event: stream.mode_ready
        let msg1 = rx.try_recv().expect("expected stream.mode_ready event");
        let parsed1: serde_json::Value =
            serde_json::from_str(&msg1).expect("stream.mode_ready must be valid JSON");
        assert_eq!(parsed1["event"], "stream.mode_ready");
        assert_eq!(parsed1["data"]["transition_id"], "test-tid-123");
        assert_eq!(parsed1["data"]["mode"], "vp8");

        // The data must be an object (not flat — frontend calls handler(payload.data))
        assert!(parsed1["data"].is_object(), "stream.mode_ready must have 'data' as an object");

        // Second event: stream.webrtc_ready
        let msg2 = rx.try_recv().expect("expected stream.webrtc_ready event");
        let parsed2: serde_json::Value =
            serde_json::from_str(&msg2).expect("stream.webrtc_ready must be valid JSON");
        assert_eq!(parsed2["event"], "stream.webrtc_ready");
        assert_eq!(parsed2["data"]["codec"], "vp8");
        assert_eq!(parsed2["data"]["hardware"], false);
        assert_eq!(parsed2["data"]["transition_id"], "test-tid-123");

        assert!(parsed2["data"].is_object(), "stream.webrtc_ready must have 'data' as an object");
    }
}
