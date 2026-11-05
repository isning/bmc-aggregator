use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;

use crate::bmc::configuration::{
    configuration_read_response, mutate_configuration_scope, ConfigurationHttpError,
    ConfigurationScope,
};
use crate::bmc::feature::config_edit::{optional_string, set_device_feature_backend};
use crate::web::state::AppState;

fn scope(id: &str) -> ConfigurationScope {
    ConfigurationScope::Feature {
        device_id: id.to_string(),
        feature_id: "audio".to_string(),
    }
}

fn snapshot(
    config: &crate::config::Config,
    id: &str,
) -> Result<serde_json::Value, ConfigurationHttpError> {
    let dev = config
        .devices
        .iter()
        .find(|dev| dev.id == id)
        .ok_or_else(|| ConfigurationHttpError::not_found(format!("device '{id}' not found")))?;
    Ok(serde_json::json!({
        "backend": dev.feature_backend("audio"),
        "enabled": dev.audio_backend.is_some(),
    }))
}

pub async fn config_audio(Path(id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.config.read().await;
    match snapshot(&cfg, &id) {
        Ok(value) => configuration_read_response(&state, &scope(&id), value),
        Err(error) => error.into_response(),
    }
}

pub async fn config_audio_update(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<serde_json::Value>,
) -> Response {
    mutate_configuration_scope(
        &state,
        &headers,
        scope(&id),
        &patch,
        vec!["features.audio".to_string()],
        |config| snapshot(config, &id),
        |config| {
            use crate::bmc::feature::config_edit::{validate_patch, PatchValue};
            validate_patch(
                &patch,
                &[
                    ("backend", PatchValue::NullableText),
                    ("enabled", PatchValue::Boolean),
                ],
            )?;
            if patch.get("enabled").and_then(serde_json::Value::as_bool) == Some(true) {
                return Err(ConfigurationHttpError::bad_request(
                    "audio_backend_unavailable",
                    "audio support is not implemented",
                ));
            }
            let dev = config
                .devices
                .iter_mut()
                .find(|dev| dev.id == id)
                .ok_or_else(|| {
                    ConfigurationHttpError::not_found(format!("device '{id}' not found"))
                })?;
            if patch.get("enabled").and_then(|value| value.as_bool()) == Some(false) {
                set_device_feature_backend(dev, "audio", "none").map_err(|error| {
                    ConfigurationHttpError::bad_request("backend_unregistered", error)
                })?;
            }
            if let Some(backend) = optional_string(&patch, "backend") {
                set_device_feature_backend(dev, "audio", backend.as_deref().unwrap_or("none"))
                    .map_err(|error| {
                        ConfigurationHttpError::bad_request("backend_unregistered", error)
                    })?;
            }
            Ok(())
        },
    )
    .await
}
