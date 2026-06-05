use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use crate::web::handlers::{build_platform_capabilities, AppState};

#[derive(Serialize)]
pub struct BmcEntry {
    pub id: String,
    pub name: String,
    pub protocol: String,
}

pub async fn bmc_list_api(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<BmcEntry>> {
    let entries: Vec<BmcEntry> = state
        .config
        .read()
        .await
        .devices
        .iter()
        .map(|d| BmcEntry {
            id: d.id.clone(),
            name: d.name.clone(),
            protocol: d.protocol.clone(),
        })
        .collect();
    Json(entries)
}

pub async fn devices_info(
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
                            {"width": cur_w, "height": cur_h, "fps": [30.0]},
                        ],
                    },
                ],
                "usb_bus": null,
                "has_signal": true,
            },
        ],
        "serial": [],
        "audio": [],
        "udc": [],
        "extensions": {
            "ttyd_available": false,
            "rustdesk_available": false,
        },
    }))
}

pub async fn device_info(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let (video, encoder, audio) = state.orchestrator.backend_info(&bmc_id).await
        .unwrap_or(("mahogany".into(), "software".into(), None));
    let (platform, capabilities) = build_platform_capabilities(&video, &encoder, &audio);
    let info = state.orchestrator.device_info(&bmc_id).await;
    match info {
        Some(i) => Json(serde_json::json!({
            "id": i.id,
            "name": i.name,
            "width": i.width,
            "height": i.height,
            "online": i.online,
            "platform": platform,
            "capabilities": capabilities,
        })),
        None => Json(serde_json::json!({
            "id": bmc_id,
            "name": String::new(),
            "width": 0u16,
            "height": 0u16,
            "online": false,
            "platform": platform,
            "capabilities": capabilities,
        })),
    }
}

pub async fn devices_usb() -> Json<serde_json::Value> {
    super::unavailable_json()
}

pub async fn devices_atx() -> Json<serde_json::Value> {
    Json(serde_json::json!({"available": true}))
}
