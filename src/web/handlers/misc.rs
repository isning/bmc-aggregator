use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::SinkExt;
use tokio::sync::RwLock;
use tracing::warn;

use crate::bmc::connection::event::ConnectionEvent;
use crate::bmc::connection::state::ConnectionState;
use crate::web::handlers::{build_platform_capabilities, AppState};

pub async fn setup_status() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "initialized": true,
        "needs_setup": false,
    }))
}

pub async fn system_info(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let (video, encoder, audio) = state.orchestrator.backend_info(&bmc_id).await
        .unwrap_or(("mahogany".into(), "software".into(), None));
    let (platform, capabilities) = build_platform_capabilities(&video, &encoder, &audio);
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "build_date": "unknown",
        "platform": platform,
        "capabilities": capabilities,
    }))
}

pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

pub async fn setup_init() -> Json<serde_json::Value> {
    Json(serde_json::json!({"success": true, "message": "setup complete"}))
}

pub async fn system_restart() -> Json<serde_json::Value> {
    Json(serde_json::json!({"success": true, "message": "restart initiated"}))
}

pub async fn snapshot() -> Response {
    (StatusCode::NOT_IMPLEMENTED, Json(serde_json::json!({"status": "unavailable"}))).into_response()
}

// ── Audio stubs (aggregator does not implement audio yet) ──

pub async fn audio_status() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "enabled": false,
        "streaming": false,
        "device": null,
        "sample_rate": 0,
        "channels": 0,
        "quality": "balanced",
        "subscriber_count": 0,
        "frames_encoded": 0,
        "bytes_output": 0,
        "error": "audio not available in aggregator mode"
    }))
}

pub async fn audio_start() -> Json<serde_json::Value> {
    Json(serde_json::json!({"success": false, "message": "audio not available in aggregator mode"}))
}

pub async fn audio_stop() -> Json<serde_json::Value> {
    Json(serde_json::json!({"success": false, "message": "audio not available in aggregator mode"}))
}

pub async fn audio_set_quality() -> Json<serde_json::Value> {
    Json(serde_json::json!({"success": false, "message": "audio not available in aggregator mode"}))
}

pub async fn audio_select_device() -> Json<serde_json::Value> {
    Json(serde_json::json!({"success": false, "message": "audio not available in aggregator mode"}))
}

pub async fn extensions_list() -> Json<serde_json::Value> {
    Json(serde_json::json!({"extensions": []}))
}

pub async fn ws_event_upgrade(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_event_socket(socket, state, bmc_id))
}

async fn handle_event_socket(socket: WebSocket, state: Arc<AppState>, bmc_id: String) {
    use futures::StreamExt;

    let (mut sender, mut receiver) = socket.split();

    // Track which features the frontend wants lifecycle events for.
    // Start with all features subscribed; frontend can narrow via subscribe message.
    let subscribed_features: Arc<RwLock<HashSet<String>>> = Arc::new(RwLock::new(
        ["video", "hid", "msd", "power"].iter().map(|s| s.to_string()).collect()
    ));

    // Mark device as in-use (non-blocking). The connect_task starts
    // immediately so HID/video handlers find the connection ready.
    let _connect_guard = state.orchestrator.acquire(&bmc_id, "video").await
        .map_err(|e| warn!("[{}] event WS: acquire failed: {}", bmc_id, e))
        .ok();

    // ── Send initial device_info event (extended with per-feature state) ──
    let info = state.orchestrator.device_info(&bmc_id).await;
    let encoder_error = state.orchestrator.encoder_error(&bmc_id).await;
    let dev = state.orchestrator.registry().get_dev(&bmc_id).await;

    let mut feature_states: HashMap<String, serde_json::Value> = HashMap::new();
    for fid in &["video", "hid", "msd", "power"] {
        let state_str = match dev.as_ref().and_then(|d| d.features.get(*fid)) {
            Some(fc) => match *fc.state.read().await {
                ConnectionState::Disconnected => "disconnected",
                ConnectionState::Connecting => "connecting",
                ConnectionState::Connected => "connected",
                ConnectionState::Disconnecting => "disconnecting",
            },
            None => "disconnected",
        };
        feature_states.insert(fid.to_string(), serde_json::json!({ "state": state_str }));
    }

    let feature_state = |fid: &str| -> serde_json::Value {
        feature_states.get(fid).cloned().unwrap_or(serde_json::json!({ "state": "disconnected" }))
    };

    if let Some(i) = info {
        let device_event = serde_json::json!({
            "event": "system.device_info",
            "data": {
                "video": {
                    "available": true,
                    "device": null,
                    "format": "mjpeg",
                    "resolution": [i.width, i.height],
                    "fps": 30,
                    "online": i.online,
                    "stream_mode": "mjpeg",
                    "error": encoder_error,
                    "connection": feature_state("video"),
                },
                "hid": {
                    "available": true,
                    "backend": "rust",
                    "initialized": true,
                    "online": i.online,
                    "supports_absolute_mouse": true,
                    "keyboard_leds_enabled": false,
                    "led_state": {
                        "num_lock": false,
                        "caps_lock": false,
                        "scroll_lock": false,
                        "compose": false,
                        "kana": false,
                    },
                    "device": null,
                    "error": null,
                    "error_code": null,
                    "connection": feature_state("hid"),
                },
                "msd": {
                    "available": true,
                    "connection": feature_state("msd"),
                },
                "atx": {
                    "available": true,
                    "connection": feature_state("power"),
                },
            }
        });
        let _ = sender.send(Message::Text(serde_json::to_string(&device_event).unwrap())).await;
    }

    // Periodic stats update
    let mut stats_interval = tokio::time::interval(Duration::from_secs(2));
    stats_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Subscribe to broadcast events (e.g., download progress)
    let mut event_rx = state.event_tx.subscribe();

    // ── Subscribe to ALL features' connection events ──
    let feature_ids = ["video", "hid", "msd", "power"];
    let (merge_tx, mut merge_rx) = tokio::sync::mpsc::unbounded_channel::<ConnectionEvent>();
    for &fid in &feature_ids {
        if let Some(rx) = state.orchestrator.registry()
            .subscribe_feature_events(&bmc_id, fid).await
        {
            let tx = merge_tx.clone();
            tokio::spawn(async move {
                let mut rx = rx;
                loop {
                    match rx.recv().await {
                        Ok(ev) => { if tx.send(ev).is_err() { break; } }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
        }
    }
    drop(merge_tx);

    // Echo/ping/stats/broadcast/lifecycle loop
    loop {
        tokio::select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&t) {
                            match val.get("type").and_then(|v| v.as_str()) {
                                Some("ping") => {
                                    let pong = serde_json::json!({"type": "pong"});
                                    let _ = sender.send(Message::Text(pong.to_string())).await;
                                }
                                Some("subscribe") => {
                                    // Allow frontend to narrow which features it wants events for
                                    if let Some(features) = val.pointer("/payload/features").and_then(|v| v.as_array()) {
                                        let mut set = subscribed_features.write().await;
                                        set.clear();
                                        for f in features {
                                            if let Some(s) = f.as_str() {
                                                set.insert(s.to_string());
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Ping(d))) => {
                        let _ = sender.send(Message::Pong(d)).await;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Binary(_))) => {}
                    Some(Err(e)) => {
                        tracing::debug!("[{}] event WS error: {}", bmc_id, e);
                        break;
                    }
                    None => break,
                }
            }
            _ = stats_interval.tick() => {
                if let Some(session) = state.orchestrator.get_session(&bmc_id).await {
                    let Some(base) = session.base.as_ref() else { continue; };
                    let fps = base.fps();
                    let clients = base.clients();

                    let mut perf = base.perf_snapshot();
                    if let Some(pipeline_perf) = state.orchestrator.pipeline_perf_snapshot(&bmc_id).await {
                        if let Some(obj) = perf.as_object_mut() {
                            if let Some(pipeline_obj) = pipeline_perf.as_object() {
                                for (k, v) in pipeline_obj {
                                    obj.insert(k.clone(), v.clone());
                                }
                            }
                        }
                    }

                    let bmc_info = base.info().await;
                    let stats_event = serde_json::json!({
                        "event": "stream.stats_update",
                        "data": {
                            "online": bmc_info.online,
                            "resolution": [bmc_info.width, bmc_info.height],
                            "target_fps": 30,
                            "fps": fps,
                            "clients": clients,
                            "clients_stat": {
                                "all": { "fps": fps }
                            },
                            "performance": perf
                        }
                    });
                    let _ = sender.send(Message::Text(stats_event.to_string())).await;
                }
            }
            result = event_rx.recv() => {
                match result {
                    Ok(msg) => {
                        let _ = sender.send(Message::Text(msg)).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            result = merge_rx.recv() => {
                match result {
                    Some(ev) => {
                        let feature = match &ev {
                            ConnectionEvent::StateChanged { feature, .. } => feature.clone(),
                            ConnectionEvent::Health { feature, .. } => feature.clone(),
                            ConnectionEvent::StageChanged { feature, .. } => feature.clone(),
                        };
                        {
                            let subs = subscribed_features.read().await;
                            if !subs.contains(&feature) {
                                continue;
                            }
                        }
                        match ev {
                            ConnectionEvent::StateChanged { feature, state } => {
                                let state_str = match state {
                                    ConnectionState::Disconnected => "disconnected",
                                    ConnectionState::Connecting => "connecting",
                                    ConnectionState::Connected => "connected",
                                    ConnectionState::Disconnecting => "disconnecting",
                                };
                                let json = serde_json::json!({
                                    "event": "connection.state_changed",
                                    "data": {
                                        "feature": feature,
                                        "state": state_str,
                                    }
                                });
                                let _ = sender.send(Message::Text(json.to_string())).await;
                            }
                            ConnectionEvent::Health { feature, online, fps, width, height } => {
                                let json = serde_json::json!({
                                    "event": "connection.health",
                                    "data": {
                                        "feature": feature,
                                        "online": online,
                                        "fps": fps,
                                        "resolution": [width, height],
                                    }
                                });
                                let _ = sender.send(Message::Text(json.to_string())).await;
                            }
                            _ => {}
                        }
                    }
                    None => break,
                }
            }
        }
    }
}
