use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tracing::warn;

use crate::bmc::traits::PowerAction;
use crate::web::handlers::AppState;

#[derive(Deserialize)]
pub struct PowerActionParams {
    action: String,
}

pub async fn power_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let (_guard, session) = match state.orchestrator.acquire_wait(&bmc_id, "power").await {
        Ok(v) => v,
        Err(e) => {
            warn!("[{}] power_status: acquire_wait failed: {}", bmc_id, e);
            return Json(serde_json::json!({
                "available": false,
                "backend": "rust",
                "initialized": false,
                "power_status": "unknown",
                "led_supported": false,
            }));
        }
    };

    let power_result = match &session.power {
        Some(power) => power.power_status().await.map_err(|e| e.to_string()),
        None => Err("IPMI not configured".to_string()),
    };

    match power_result {
        Ok(power_state) => {
            let power_status = match power_state {
                crate::bmc::backends::general::ipmi::PowerState::On => "on",
                crate::bmc::backends::general::ipmi::PowerState::Off => "off",
                crate::bmc::backends::general::ipmi::PowerState::Unknown => "unknown",
            };
            Json(serde_json::json!({
                "available": true,
                "backend": "rust",
                "initialized": true,
                "power_status": power_status,
                "led_supported": false,
            }))
        }
        Err(e) => {
            warn!("power status check failed: {}", e);
            Json(serde_json::json!({
                "available": false,
                "backend": "rust",
                "initialized": false,
                "power_status": "unknown",
                "led_supported": false,
            }))
        }
    }
}

pub async fn power_control(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<PowerActionParams>,
) -> Response {
    let action = match params.action.as_str() {
        "short" | "on" | "press" => PowerAction::PressPower,
        "long" | "hold" => PowerAction::PressPowerLong,
        "reset" => PowerAction::PressReset,
        _ => {
            return (StatusCode::BAD_REQUEST, "invalid action").into_response();
        }
    };

    let (_guard, session) = match state.orchestrator.acquire_wait(&bmc_id, "power").await {
        Ok(v) => v,
        Err(e) => {
            warn!("[{}] power_control: acquire_wait failed: {}", bmc_id, e);
            return (StatusCode::SERVICE_UNAVAILABLE, e).into_response();
        }
    };

    let result = match &session.power {
        Some(power) => power.power_control(action).await.map_err(|e| e.to_string()),
        None => Err("IPMI not configured".to_string()),
    };

    match result {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => {
            warn!("power control error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}
