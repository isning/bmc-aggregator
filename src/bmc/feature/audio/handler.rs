use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;

use crate::bmc::feature::build_feature_status;
use crate::bmc::feature::fault::{fault_response, FeatureFault, FeatureFaultDomain};
use crate::bmc::feature::retry_feature;
use crate::web::state::AppState;

pub async fn audio_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    if let Some(dev) = state.registry.get_dev(&bmc_id).await {
        if let Some(fc) = dev.features.get("audio") {
            return Json(build_feature_status(fc).await);
        }
    }
    Json(serde_json::json!({
        "available": false,
        "online": false,
        "streaming": false,
        "fault": FeatureFault::feature(
            FeatureFaultDomain::Lifecycle,
            "feature_unavailable",
            "The audio feature is not available for this device",
            false,
        ),
    }))
}

fn audio_unavailable(operation: &'static str) -> Response {
    fault_response(
        StatusCode::NOT_IMPLEMENTED,
        FeatureFault::operation(
            FeatureFaultDomain::Operation,
            "feature_unavailable",
            "The audio feature is not available for this device",
            false,
            operation,
        ),
    )
}

pub async fn audio_start(
    Path(_bmc_id): Path<String>,
    State(_state): State<Arc<AppState>>,
) -> Response {
    audio_unavailable("start")
}

pub async fn audio_stop(
    Path(_bmc_id): Path<String>,
    State(_state): State<Arc<AppState>>,
) -> Response {
    audio_unavailable("stop")
}

pub async fn audio_retry(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    retry_feature(&state, &bmc_id, "audio").await
}

pub async fn audio_set_quality(
    Path(_bmc_id): Path<String>,
    State(_state): State<Arc<AppState>>,
    Json(_params): Json<serde_json::Value>,
) -> Response {
    audio_unavailable("set_quality")
}

pub async fn audio_select_device(
    Path(_bmc_id): Path<String>,
    State(_state): State<Arc<AppState>>,
    Json(_params): Json<serde_json::Value>,
) -> Response {
    audio_unavailable("select_device")
}
