use std::sync::Arc;

use axum::extract::Path;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::Json;

use crate::bmc::configuration::{
    configuration_read_response, mutate_configuration_scope, ConfigurationHttpError,
    ConfigurationScope,
};
use crate::bmc::feature::config_edit::set_device_feature_param;
use crate::web::state::AppState;

fn video_scope(id: &str) -> ConfigurationScope {
    ConfigurationScope::Feature {
        device_id: id.to_string(),
        feature_id: "video".to_string(),
    }
}

fn video_input_json(dev: &crate::config::DeviceConfig) -> serde_json::Value {
    serde_json::json!({
        "device": dev.canonical_video_device().unwrap_or_default(),
        "format": dev.canonical_video_format().unwrap_or_default(),
        "width": dev.canonical_video_width().unwrap_or(1920),
        "height": dev.canonical_video_height().unwrap_or(1080),
        "fps": dev.canonical_video_fps().unwrap_or_else(|| dev.canonical_max_fps()),
        "quality": dev.canonical_video_quality().unwrap_or(80),
    })
}

fn video_input_snapshot(
    config: &crate::config::Config,
    id: &str,
) -> Result<serde_json::Value, ConfigurationHttpError> {
    config
        .devices
        .iter()
        .find(|dev| dev.id == id)
        .map(video_input_json)
        .ok_or_else(|| ConfigurationHttpError::not_found(format!("device '{id}' not found")))
}

fn stream_config_snapshot(
    config: &crate::config::Config,
    id: &str,
) -> Result<serde_json::Value, ConfigurationHttpError> {
    let dev = config
        .devices
        .iter()
        .find(|dev| dev.id == id)
        .ok_or_else(|| ConfigurationHttpError::not_found(format!("device '{id}' not found")))?;
    let encoder = dev.canonical_encoder_backend();
    Ok(serde_json::json!({
        "mode": "mjpeg",
        "encoder": encoder,
        "bitrate_preset": { "type": "Balanced" },
        "has_public_ice_servers": false,
        "using_public_ice_servers": false,
    }))
}

pub async fn video_input_config_get(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let cfg = state.config.read().await;
    match video_input_snapshot(&cfg, &bmc_id) {
        Ok(value) => configuration_read_response(&state, &video_scope(&bmc_id), value),
        Err(error) => error.into_response(),
    }
}

pub async fn video_input_config_patch(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<serde_json::Value>,
) -> Response {
    mutate_configuration_scope(
        &state,
        &headers,
        video_scope(&bmc_id),
        &patch,
        vec!["features.video.input".to_string()],
        |config| video_input_snapshot(config, &bmc_id),
        |config| {
            use crate::bmc::feature::config_edit::{validate_patch, PatchValue};
            validate_patch(
                &patch,
                &[
                    ("device", PatchValue::NullableText),
                    ("format", PatchValue::NullableText),
                    (
                        "width",
                        PatchValue::Integer {
                            min: 1,
                            max: u64::from(super::MAX_INPUT_DIMENSION),
                        },
                    ),
                    (
                        "height",
                        PatchValue::Integer {
                            min: 1,
                            max: u64::from(super::MAX_INPUT_DIMENSION),
                        },
                    ),
                    (
                        "fps",
                        PatchValue::Integer {
                            min: 1,
                            max: u64::from(super::MAX_INPUT_FPS),
                        },
                    ),
                    ("quality", PatchValue::Integer { min: 1, max: 100 }),
                ],
            )?;
            let dev = config
                .devices
                .iter_mut()
                .find(|dev| dev.id == bmc_id)
                .ok_or_else(|| {
                    ConfigurationHttpError::not_found(format!("device '{bmc_id}' not found"))
                })?;

            for key in ["device", "format"] {
                if let Some(value) = patch.get(key) {
                    let stored_key = match key {
                        "device" => "video_device",
                        "format" => "video_format",
                        _ => unreachable!(),
                    };
                    let stored = if value.is_null() {
                        serde_json::Value::Null
                    } else if let Some(value) = value.as_str() {
                        serde_json::json!(value)
                    } else {
                        return Err(ConfigurationHttpError::bad_request(
                            "configuration_value_invalid",
                            format!("video {key} must be a string or null"),
                        ));
                    };
                    set_device_feature_param(dev, "video", stored_key, stored);
                }
            }

            for key in ["width", "height", "fps", "quality"] {
                if let Some(raw) = patch.get(key) {
                    let Some(value) = raw.as_u64() else {
                        return Err(ConfigurationHttpError::bad_request(
                            "configuration_value_invalid",
                            format!("video {key} must be a positive integer"),
                        ));
                    };
                    if value == 0 || (key == "quality" && value > 100) {
                        return Err(ConfigurationHttpError::bad_request(
                            "configuration_value_invalid",
                            format!("video {key} is outside the supported range"),
                        ));
                    }
                    let stored_key = match key {
                        "width" => "video_width",
                        "height" => "video_height",
                        "fps" => "video_fps",
                        "quality" => "video_quality",
                        _ => unreachable!(),
                    };
                    set_device_feature_param(dev, "video", stored_key, serde_json::json!(value));
                }
            }
            Ok(())
        },
    )
    .await
}

pub async fn stream_config_get(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let cfg = state.config.read().await;
    match stream_config_snapshot(&cfg, &bmc_id) {
        Ok(value) => configuration_read_response(&state, &video_scope(&bmc_id), value),
        Err(error) => error.into_response(),
    }
}

pub async fn stream_bitrate_get(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    match super::handler::control_processor(&state, &bmc_id).await {
        Ok(processor) => Json(processor.bitrate_response().await).into_response(),
        Err(response) => response,
    }
}

pub async fn stream_bitrate(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<serde_json::Value>,
) -> Response {
    match super::handler::control_processor(&state, &bmc_id).await {
        Ok(processor) => match processor.update_bitrate(params).await {
            Ok(value) => Json(value).into_response(),
            Err(message) => crate::bmc::feature::fault::fault_response(
                axum::http::StatusCode::BAD_REQUEST,
                crate::bmc::feature::fault::FeatureFault::operation(
                    crate::bmc::feature::fault::FeatureFaultDomain::Operation,
                    "invalid_bitrate",
                    message,
                    false,
                    "set_bitrate",
                ),
            ),
        },
        Err(response) => response,
    }
}

pub async fn stream_config_patch(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<serde_json::Value>,
) -> Response {
    mutate_configuration_scope(
        &state,
        &headers,
        video_scope(&bmc_id),
        &patch,
        vec!["features.video.encoder".to_string()],
        |config| stream_config_snapshot(config, &bmc_id),
        |config| {
            let fields = patch.as_object().ok_or_else(|| ConfigurationHttpError::bad_request("configuration_value_invalid", "stream patch must be an object"))?;
            if fields.keys().any(|key| key != "encoder") {
                return Err(ConfigurationHttpError::bad_request("configuration_field_unsupported", "Only encoder is persisted here; use video runtime controls for mode and bitrate"));
            }

            let dev = config
                .devices
                .iter_mut()
                .find(|dev| dev.id == bmc_id)
                .ok_or_else(|| {
                    ConfigurationHttpError::not_found(format!("device '{bmc_id}' not found"))
                })?;
            if let Some(value) = patch.get("encoder") {
                let Some(encoder) = value.as_str().filter(|value| !value.trim().is_empty()) else {
                    return Err(ConfigurationHttpError::bad_request(
                        "configuration_value_invalid",
                        "video encoder must be a non-empty string",
                    ));
                };
                set_device_feature_param(
                    dev,
                    "video",
                    "encoder_backend",
                    serde_json::json!(encoder),
                );
            }
            Ok(())
        },
    )
    .await
}
