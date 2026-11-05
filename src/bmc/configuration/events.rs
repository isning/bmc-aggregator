use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use super::{ConfigurationChange, ConfigurationCoordinator};
use crate::web::state::AppState;

pub async fn configuration_ws_upgrade(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Response {
    if let Err(status) = crate::web::validate_websocket_origin(&headers) {
        return status.into_response();
    }
    ws.on_upgrade(move |socket| handle_configuration_socket(socket, state))
}

fn ready_event(coordinator: &ConfigurationCoordinator) -> serde_json::Value {
    serde_json::json!({
        "type": "configuration.ready",
        "epoch": coordinator.epoch(),
        "revision": coordinator.current_revision(),
    })
}

fn changed_event(change: ConfigurationChange) -> serde_json::Value {
    let mut value = serde_json::to_value(change).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(fields) = value.as_object_mut() {
        fields.insert(
            "type".to_string(),
            serde_json::Value::String("configuration.changed".to_string()),
        );
    }
    value
}

fn resync_event(coordinator: &ConfigurationCoordinator, skipped: u64) -> serde_json::Value {
    serde_json::json!({
        "type": "configuration.resync_required",
        "epoch": coordinator.epoch(),
        "revision": coordinator.current_revision(),
        "skipped": skipped,
    })
}

async fn send_json(socket: &mut WebSocket, value: serde_json::Value) -> Result<(), ()> {
    crate::web::send_websocket(socket, Message::Text(value.to_string()))
        .await
        .map_err(|_| ())
}

async fn handle_configuration_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let mut stopped = state.registry.stopped_rx();
    if *stopped.borrow() {
        return;
    }
    let mut events = state.configuration.subscribe();
    if send_json(&mut socket, ready_event(&state.configuration))
        .await
        .is_err()
    {
        return;
    }

    loop {
        if *stopped.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            event = events.recv() => {
                let payload = match event {
                    Ok(change) => changed_event(change),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        resync_event(&state.configuration, skipped)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if send_json(&mut socket, payload).await.is_err() {
                    break;
                }
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(payload))) => {
                        let result = crate::web::send_websocket(&mut socket, Message::Pong(payload)).await;
                        if result.is_err() {
                            break;
                        }
                    }
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmc::configuration::ConfigurationScope;
    use axum::http::{header, HeaderMap, StatusCode};

    #[test]
    fn event_payloads_expose_ordering_without_configuration_values() {
        let coordinator = ConfigurationCoordinator::new();
        let change = coordinator.publish(
            ConfigurationScope::Feature {
                device_id: "server".to_string(),
                feature_id: "video".to_string(),
            },
            "mutation-1",
            vec!["features.video.input".to_string()],
            vec!["video".to_string()],
        );
        let payload = changed_event(change);

        assert_eq!(payload["type"], "configuration.changed");
        assert_eq!(payload["scope"]["kind"], "feature");
        assert_eq!(payload["mutation_id"], "mutation-1");
        assert!(payload.get("configuration").is_none());
        assert_eq!(ready_event(&coordinator)["revision"], 1);
        assert_eq!(resync_event(&coordinator, 7)["skipped"], 7);
    }

    #[test]
    fn websocket_origin_policy_rejects_cross_site_and_accepts_same_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:8400".parse().unwrap());
        headers.insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        assert_eq!(
            crate::web::validate_websocket_origin(&headers),
            Err(StatusCode::FORBIDDEN)
        );
        headers.insert(header::ORIGIN, "http://127.0.0.1:8400".parse().unwrap());
        assert_eq!(crate::web::validate_websocket_origin(&headers), Ok(()));
    }
}
