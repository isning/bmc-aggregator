use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::bmc::msd_storage::MsdStorageProvider;
use crate::config::MsdDeviceConfig;
use crate::web::handlers::AppState;

pub async fn config_hid() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "keyset": [],
        "mouse_x_min": 0, "mouse_x_max": 32767,
        "mouse_y_min": 0, "mouse_y_max": 32767,
        "mouse_absolute": true,
        "jiggler": false,
    }))
}

async fn video_config_response(
    state: &AppState,
    bmc_id: &str,
) -> Json<serde_json::Value> {
    let info = state.orchestrator.device_info(bmc_id).await;
    let (cur_w, cur_h) = info.map(|i| (i.width, i.height)).unwrap_or((1920, 1080));
    let cfg = state.config.read().await;
    let device_cfg = cfg.devices.iter().find(|d| d.id == bmc_id);
    Json(serde_json::json!({
        "device": device_cfg.and_then(|d| d.video_device.as_deref()).unwrap_or("virtual"),
        "format": device_cfg.and_then(|d| d.video_format.as_deref()).unwrap_or("MJPEG"),
        "width": device_cfg.and_then(|d| d.video_width).unwrap_or(cur_w as u32),
        "height": device_cfg.and_then(|d| d.video_height).unwrap_or(cur_h as u32),
        "fps": device_cfg.and_then(|d| d.video_fps).unwrap_or(30),
        "quality": device_cfg.and_then(|d| d.video_quality).unwrap_or(80),
    }))
}

pub async fn config_video(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    video_config_response(&state, &bmc_id).await
}

#[derive(Deserialize)]
pub struct VideoConfigUpdate {
    pub device: Option<String>,
    pub format: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub quality: Option<u32>,
}

pub async fn config_video_update(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<VideoConfigUpdate>,
) -> Response {
    let mut cfg = state.config.write().await;
    let device = cfg.devices.iter_mut().find(|d| d.id == bmc_id);
    let device = match device {
        Some(d) => d,
        None => {
            drop(cfg);
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "device not found"}))).into_response();
        }
    };
    if payload.device.is_some() { device.video_device = payload.device; }
    if payload.format.is_some() { device.video_format = payload.format; }
    if payload.width.is_some() { device.video_width = payload.width; }
    if payload.height.is_some() { device.video_height = payload.height; }
    if payload.fps.is_some() { device.video_fps = payload.fps; }
    if payload.quality.is_some() { device.video_quality = payload.quality; }
    let _ = cfg.save(&state.config_path);
    drop(cfg);

    video_config_response(&state, &bmc_id).await.into_response()
}

pub async fn config_power() -> Json<serde_json::Value> {
    Json(serde_json::json!({"available": true, "type": "ipmi"}))
}

pub async fn config_msd(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let cfg = state.config.read().await;
    let msd_cfg = cfg.msd.get(&bmc_id).cloned().unwrap_or(MsdDeviceConfig {
        backend: "mahogany".to_string(),
        drive_size_mb: 1024,
        storage_dir: None,
    });
    let storage_dir = if let Some(d) = msd_cfg.storage_dir.clone() {
        d
    } else {
        state.msd_storage.storage_base(&bmc_id).await
    };
    Json(serde_json::json!({
        "available": true,
        "msd_enabled": true,
        "backends": ["mahogany", "virtual-drive"],
        "selected_backend": msd_cfg.backend,
        "msd_backend": msd_cfg.backend,
        "drive_size_mb": msd_cfg.drive_size_mb,
        "size_mb": msd_cfg.drive_size_mb,
        "msd_dir": storage_dir,
        "image_dir": storage_dir,
        "storage": {
            "directory": storage_dir,
            "upload_chunk_size": 1048576
        }
    }))
}

pub async fn config_msd_patch(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    let backend = payload.get("backend")
        .and_then(|value| value.as_str())
        .or_else(|| payload.get("selected_backend").and_then(|value| value.as_str()))
        .or_else(|| payload.get("msd_backend").and_then(|value| value.as_str()))
        .map(|value| value.to_string());
    let drive_size_mb = payload.get("drive_size_mb")
        .and_then(|value| value.as_u64())
        .or_else(|| payload.get("size_mb").and_then(|value| value.as_u64()));
    let storage_dir = payload.get("storage_dir")
        .and_then(|value| value.as_str())
        .or_else(|| payload.get("msd_dir").and_then(|value| value.as_str()))
        .or_else(|| payload.get("image_dir").and_then(|value| value.as_str()))
        .map(|value| value.to_string());

    let backend_c = backend.clone();
    let storage_dir_c = storage_dir.clone();
    let bmc_id_c = bmc_id.clone();
    if let Err(e) = state.update_config(|cfg| {
        let entry = cfg.msd.entry(bmc_id_c.clone()).or_insert_with(|| MsdDeviceConfig {
            backend: "mahogany".to_string(),
            drive_size_mb: 1024,
            storage_dir: None,
        });
        if let Some(ref b) = backend {
            entry.backend = b.clone();
        }
        if let Some(s) = drive_size_mb {
            entry.drive_size_mb = s;
        }
        if let Some(ref d) = storage_dir_c {
            entry.storage_dir = Some(d.clone());
        }
    }).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response();
    }
    if let Some(b) = backend_c {
        state.update_msd_runtime(&bmc_id, |runtime| runtime.backend = b).await;
    }
    Json(serde_json::json!({
        "status": "ok",
        "backend": backend,
        "drive_size_mb": drive_size_mb
    }))
    .into_response()
}

pub async fn config_auth() -> Json<serde_json::Value> {
    Json(serde_json::json!({"enabled": false}))
}

pub async fn config_audio() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "enabled": false,
        "device": "none",
        "quality": "balanced",
        "backend": "none"
    }))
}

pub async fn config_web() -> Json<serde_json::Value> {
    Json(serde_json::json!({"title": "BMC Aggregator"}))
}

pub async fn config_generic() -> Json<serde_json::Value> {
    Json(serde_json::json!({}))
}
