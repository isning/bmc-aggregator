use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::encoding::parse_stream_backend;
use crate::bmc::encoder::Codec;
use crate::bmc::feature::fault::{fault_response, FeatureFault, FeatureFaultDomain};
use crate::web::state::AppState;

pub async fn get_mode(Path(bmc_id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    match super::handler::control_processor(&state, &bmc_id).await {
        Ok(processor) => {
            Json(serde_json::json!({"success": true, "mode": processor.stream_mode().await}))
                .into_response()
        }
        Err(response) => response,
    }
}

pub async fn post_mode(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<serde_json::Value>,
) -> Response {
    let processor = match super::handler::control_processor(&state, &bmc_id).await {
        Ok(processor) => processor,
        Err(response) => return response,
    };

    let Some(mode) = params.get("mode").and_then(|v| v.as_str()) else {
        return fault_response(
            StatusCode::BAD_REQUEST,
            FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "mode_required",
                "A video mode is required",
                false,
                "set_mode",
            ),
        );
    };
    let Some(codec) = Codec::from_str(mode) else {
        return fault_response(
            StatusCode::BAD_REQUEST,
            FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "unsupported_mode",
                "The requested video mode is unsupported",
                false,
                "set_mode",
            ),
        );
    };
    if codec.supports_webrtc() {
        let backend = match params.get("backend") {
            None => Some("software"),
            Some(serde_json::Value::String(value)) => parse_stream_backend(value),
            Some(_) => None,
        };
        let Some(backend) = backend.filter(|backend| {
            crate::bmc::encoder::codec::codecs_for_backend(backend).contains(&codec.as_str())
        }) else {
            return fault_response(
                StatusCode::BAD_REQUEST,
                FeatureFault::operation(
                    FeatureFaultDomain::Operation,
                    "unsupported_encoder_selection",
                    "The requested encoder backend and codec combination is unsupported",
                    false,
                    "set_mode",
                ),
            );
        };
        processor.set_encoding_selection(codec, backend).await;
    } else {
        processor.set_stream_codec(codec).await;
    }

    Json(serde_json::json!({
        "success": true,
        "mode": codec.as_str(),
    }))
    .into_response()
}
