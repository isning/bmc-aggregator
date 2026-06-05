use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;

use crate::web::handlers::AppState;

pub async fn device_features(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let config = state.config.read().await;
    let protocol = config.devices.iter()
        .find(|d| d.id == bmc_id)
        .map(|d| d.protocol.as_str())
        .unwrap_or("unknown");

    let features = crate::bmc::connector::features_for_protocol(protocol);

    Json(serde_json::json!({
        "device_id": bmc_id,
        "protocol": protocol,
        "features": features.iter().map(|f| serde_json::json!({
            "id": f,
            "name": f,
            "config_route": format!("/bmc/{}/api/feature/{}/config", bmc_id, f),
        })).collect::<Vec<_>>(),
    }))
}
