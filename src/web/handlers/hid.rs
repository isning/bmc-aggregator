use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use futures::{SinkExt, StreamExt};
use tracing::{debug, warn};

use crate::bmc::hid_protocol::{parse_hid_message, HidEvent};
use crate::web::handlers::AppState;

pub async fn hid_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let (_guard, session) = match state.orchestrator.acquire_wait(&bmc_id, "hid").await {
        Ok(v) => v,
        Err(e) => {
            warn!("[{}] hid_status: acquire_wait failed: {}", bmc_id, e);
            return Json(serde_json::json!({
                "available": true,
                "connected": false,
                "initialized": false,
                "absolute_mode": true,
                "supports_absolute_mouse": true,
                "protocol": "mahogany",
            }));
        }
    };
    let online = session.input.is_some();
    Json(serde_json::json!({
        "available": true,
        "connected": online,
        "initialized": online,
        "absolute_mode": true,
        "supports_absolute_mouse": true,
        "protocol": "mahogany",
    }))
}

pub async fn hid_otg_self_check() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "overall_ok": true,
        "error_count": 0,
        "warning_count": 0,
        "hid_backend": "mahogany",
        "selected_udc": null,
        "bound_udc": null,
        "udc_state": null,
        "udc_speed": null,
        "available_udcs": [],
        "other_gadgets": [],
        "checks": [{"id": "status", "ok": true, "level": "info", "message": "HID backend available"}]
    }))
}

pub async fn hid_reset() -> Json<serde_json::Value> {
    Json(serde_json::json!({"success": true}))
}

pub async fn ws_hid_upgrade(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_hid_socket(socket, state, bmc_id))
}

async fn handle_hid_socket(socket: WebSocket, state: Arc<AppState>, bmc_id: String) {
    let (_guard, session) = match state.orchestrator.acquire_wait(&bmc_id, "hid").await {
        Ok(v) => v,
        Err(e) => {
            warn!("[{}] HID WS: acquire_wait failed: {}", bmc_id, e);
            return;
        }
    };

    let input = match session.input.as_ref() {
        Some(i) => i.clone(),
        None => {
            warn!("[{}] HID WS: no input session available", bmc_id);
            return;
        }
    };

    let (mut sender, mut receiver) = socket.split();

    if sender.send(Message::Binary(vec![0x00])).await.is_err() {
        return;
    }

    let mut mouse_buttons: u8 = 0;

    while let Some(msg) = receiver.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                debug!("[{}] HID WS error: {}", bmc_id, e);
                break;
            }
        };

        let data = match msg {
            Message::Binary(d) => d,
            Message::Close(_) => break,
            Message::Ping(d) => {
                let _ = sender.send(Message::Pong(d)).await;
                continue;
            }
            Message::Pong(_) => continue,
            Message::Text(_) => continue,
        };

        let Some(event) = parse_hid_message(&data) else {
            if !data.is_empty() {
                debug!("[{}] unknown HID message type: {}", bmc_id, data[0]);
            }
            continue;
        };

        match event {
            HidEvent::Keyboard { usage, pressed } => {
                if let Err(e) = input.send_keyboard(usage, pressed).await {
                    warn!("[{}] keyboard send error: {}", bmc_id, e);
                }
            }
            HidEvent::MouseMove { x, y } => {
                if let Err(e) = input.send_mouse(x, y, mouse_buttons, 0).await {
                    warn!("[{}] mouse move error: {}", bmc_id, e);
                }
            }
            HidEvent::MouseDown { x, y, button } => {
                if button <= 4 {
                    mouse_buttons |= 1 << button;
                }
                if let Err(e) = input.send_mouse(x, y, mouse_buttons, 0).await {
                    warn!("[{}] mouse down error: {}", bmc_id, e);
                }
            }
            HidEvent::MouseUp { x, y, button } => {
                if button <= 4 {
                    mouse_buttons &= !(1 << button);
                }
                if let Err(e) = input.send_mouse(x, y, mouse_buttons, 0).await {
                    warn!("[{}] mouse up error: {}", bmc_id, e);
                }
            }
            HidEvent::MouseScroll { x, y, delta } => {
                if let Err(e) = input.send_mouse(x, y, mouse_buttons, delta).await {
                    warn!("[{}] mouse scroll error: {}", bmc_id, e);
                }
            }
            HidEvent::Consumer { .. } => {}
        }
    }
}
