use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;

use crate::bmc::feature::connection::FeatureGuard;
use crate::web::state::AppState;

pub async fn setup_status() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "setup_required": false,
        "setup_possible": true,
        "platform": {
            "mode": "linux",
            "mode_label": "Linux",
            "video_capture": {"available": false, "backends": []},
            "encoder": {"available": false, "backends": []},
            "hid": {"available": false, "backends": []},
            "atx": {"available": false, "backends": []},
            "msd": {"available": false, "backends": []},
            "audio": {"available": false, "backends": []},
            "diagnostics": {"available": false, "backends": []},
            "service_installation": {"available": false, "backends": []}
        }
    }))
}

pub async fn system_info(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "mode": "bmc_aggregator",
        "arch": std::env::consts::ARCH,
        "os": std::env::consts::OS,
        "memory": "",
        "cpu": ""
    }))
}

pub async fn setup_init(
    State(_state): State<Arc<AppState>>,
    Json(_params): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({"success": true, "message": "setup not required for aggregator"}))
}

pub async fn ws_event_upgrade(
    ws: axum::extract::ws::WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Path(bmc_id): Path<String>,
) -> axum::response::Response {
    if let Err(status) = crate::web::validate_websocket_origin(&headers) {
        return status.into_response();
    }
    ws.on_upgrade(move |socket| handle_event_socket(socket, state, bmc_id))
}

async fn handle_event_socket(
    mut socket: axum::extract::ws::WebSocket,
    state: Arc<AppState>,
    bmc_id: String,
) {
    use axum::extract::ws::Message;
    use tracing::info;

    info!("[{}] event ws connected", bmc_id);
    let mut stopped = state.registry.stopped_rx();
    let mut rx = state.registry.event_tx().subscribe();
    let mut configurations = state.configuration.subscribe();
    let mut requested_features = HashSet::new();
    let mut feature_leases: HashMap<String, FeatureGuard> = HashMap::new();
    loop {
        if *stopped.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            change = configurations.recv() => {
                use crate::bmc::configuration::ConfigurationScope;
                let refresh = match change {
                    Ok(change) => match change.scope {
                        ConfigurationScope::Device { id } => id == bmc_id,
                        ConfigurationScope::Feature { device_id, .. } => device_id == bmc_id,
                        ConfigurationScope::Global { .. } | ConfigurationScope::Options { .. } => false,
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if refresh {
                    reconcile_feature_leases(&state, &bmc_id, &requested_features, &mut feature_leases).await;
                }
            }
            event = rx.recv() => {
                match event {
                    Ok((dev_id, feat_id, ev)) => {
                        if dev_id != bmc_id {
                            continue;
                        }
                        let payload = connection_event_to_json(&feat_id, &ev);
                        let text = serde_json::to_string(&payload).unwrap_or_else(|_| r#"{}"#.to_string());
                        if crate::web::send_websocket(&mut socket, Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        let payload = serde_json::json!({
                            "type": "feature.data_resync_required",
                            "skipped": skipped,
                        });
                        if crate::web::send_websocket(&mut socket, Message::Text(payload.to_string())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(Message::Text(text))) => {
                        if let Some(requested) = subscribed_features(text.as_str()) {
                            requested_features = requested;
                            reconcile_feature_leases(&state, &bmc_id, &requested_features, &mut feature_leases).await;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    info!("[{}] event ws disconnected", bmc_id);
}

fn subscribed_features(message: &str) -> Option<HashSet<String>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(message) else {
        return None;
    };
    if value.get("type").and_then(serde_json::Value::as_str) != Some("subscribe") {
        return None;
    }
    Some(
        value
            .pointer("/payload/features")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect(),
    )
}

async fn reconcile_feature_leases(
    state: &Arc<AppState>,
    bmc_id: &str,
    requested: &HashSet<String>,
    leases: &mut HashMap<String, FeatureGuard>,
) {
    let Some(device) = state.registry.get_dev(bmc_id).await else {
        leases.clear();
        return;
    };
    leases.retain(|feature, guard| {
        requested.contains(feature)
            && device.features.get(feature).is_some_and(|connection| {
                !connection.is_stopped() && Arc::ptr_eq(connection, guard.fc())
            })
    });
    for feature in requested {
        if leases.contains_key(feature) {
            continue;
        }
        if let Some(connection) = device
            .features
            .get(feature)
            .filter(|connection| !connection.is_stopped())
        {
            leases.insert(feature.clone(), FeatureGuard::new(connection.clone()));
        }
    }
}

fn connection_event_to_json(
    _feat_id: &str,
    ev: &crate::bmc::feature::connection::ConnectionEvent,
) -> serde_json::Value {
    match ev {
        crate::bmc::feature::connection::ConnectionEvent::StateChanged {
            feature: ref f,
            instance: ref i,
            ref state,
        } => {
            serde_json::json!({
                "type": "connection.state_changed",
                "feature": f,
                "connection_id": i,
                "state": format!("{:?}", state),
            })
        }
        crate::bmc::feature::connection::ConnectionEvent::StageChanged {
            feature: ref f,
            instance: ref i,
            ref stage,
        } => {
            serde_json::json!({
                "type": "connection.stage_changed",
                "feature": f,
                "connection_id": i,
                "stage": stage,
            })
        }
        crate::bmc::feature::connection::ConnectionEvent::Health {
            feature: ref f,
            instance: ref i,
            online,
            fps,
            width,
            height,
        } => {
            serde_json::json!({
                "type": "connection.health",
                "feature": f,
                "connection_id": i,
                "online": online,
                "fps": fps,
                "width": width,
                "height": height,
            })
        }
        crate::bmc::feature::connection::ConnectionEvent::FaultChanged {
            feature: ref f,
            instance: ref i,
            ref fault,
        } => {
            serde_json::json!({
                "type": "feature.fault_changed",
                "feature": f,
                "connection_id": i,
                "fault": fault,
            })
        }
        crate::bmc::feature::connection::ConnectionEvent::StreamStateChanged {
            feature: ref f,
            instance: ref i,
            ref state,
            ref reason,
        } => serde_json::json!({
            "type": "stream.state_changed",
            "feature": f,
            "connection_id": i,
            "state": state,
            "reason": reason,
        }),
        crate::bmc::feature::connection::ConnectionEvent::DataChanged {
            feature: ref f,
            instance: ref i,
            ref epoch,
            ref resource,
            revision,
            ref cause,
            ref mutation_id,
            ref data,
        } => serde_json::json!({
            "type": "feature.data_changed",
            "feature": f,
            "connection_id": i,
            "epoch": epoch,
            "resource": resource,
            "revision": revision,
            "cause": cause,
            "mutation_id": mutation_id,
            "data": data,
        }),
        crate::bmc::feature::connection::ConnectionEvent::DataResyncRequired {
            feature: ref f,
            instance: ref i,
            ref epoch,
            ref resource,
            skipped,
        } => serde_json::json!({
            "type": "feature.data_resync_required",
            "feature": f,
            "connection_id": i,
            "epoch": epoch,
            "resource": resource,
            "skipped": skipped,
        }),
    }
}

use axum::extract::Path;

#[cfg(test)]
mod tests {
    use super::{connection_event_to_json, subscribed_features};
    use crate::bmc::feature::connection::ConnectionEvent;

    #[test]
    fn event_subscription_extracts_deduplicated_feature_leases() {
        let features = subscribed_features(
            r#"{"type":"subscribe","payload":{"topics":["stream.state_changed"],"features":["video","video"]}}"#,
        )
        .unwrap();
        assert_eq!(features.len(), 1);
        assert!(features.contains("video"));
    }

    #[test]
    fn unrelated_or_malformed_messages_do_not_release_feature_leases() {
        assert!(subscribed_features("not-json").is_none());
        assert!(subscribed_features(r#"{"type":"ping"}"#).is_none());
    }

    #[test]
    fn data_events_expose_the_resource_epoch() {
        let value = connection_event_to_json(
            "power",
            &ConnectionEvent::DataChanged {
                feature: "power".into(),
                instance: "default".into(),
                epoch: "epoch-2".into(),
                resource: "status".into(),
                revision: 1,
                cause: "refresh".into(),
                mutation_id: None,
                data: serde_json::json!({"power_status": "on"}),
            },
        );
        assert_eq!(value["epoch"], "epoch-2");
        assert_eq!(value["revision"], 1);
    }
}
