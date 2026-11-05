use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;

use crate::bmc::configuration::{
    configuration_read_response, mutate_configuration_scope, ConfigurationHttpError,
    ConfigurationScope,
};
use crate::bmc::feature::config_edit::set_device_feature_backend;
use crate::web::state::AppState;

fn scope(id: &str) -> ConfigurationScope {
    ConfigurationScope::Feature {
        device_id: id.to_string(),
        feature_id: "hid".to_string(),
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
    let backend = dev.feature_backend("hid").map(str::to_string);
    let mouse_absolute = dev
        .features
        .get("hid")
        .and_then(|feature| feature.feature_params.get("mouse_absolute"))
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    Ok(serde_json::json!({
        "backend": backend,
        "mouse_absolute": mouse_absolute,
        "ch9329_port": null,
        "ch9329_baudrate": null
    }))
}

pub async fn config_hid(Path(id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.config.read().await;
    match snapshot(&cfg, &id) {
        Ok(value) => configuration_read_response(&state, &scope(&id), value),
        Err(error) => error.into_response(),
    }
}

pub async fn config_hid_update(
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
        vec!["features.hid".to_string()],
        |config| snapshot(config, &id),
        |config| {
            use crate::bmc::feature::config_edit::{validate_patch, PatchValue};
            validate_patch(
                &patch,
                &[
                    ("backend", PatchValue::Text),
                    ("mouse_absolute", PatchValue::Boolean),
                ],
            )?;
            let dev = config
                .devices
                .iter_mut()
                .find(|dev| dev.id == id)
                .ok_or_else(|| {
                    ConfigurationHttpError::not_found(format!("device '{id}' not found"))
                })?;
            if let Some(backend) = patch.get("backend").and_then(|value| value.as_str()) {
                set_device_feature_backend(dev, "hid", backend).map_err(|error| {
                    ConfigurationHttpError::bad_request("backend_unregistered", error)
                })?;
            }
            if let Some(mouse_absolute) = patch
                .get("mouse_absolute")
                .and_then(|value| value.as_bool())
            {
                crate::bmc::feature::config_edit::set_device_feature_param(
                    dev,
                    "hid",
                    "mouse_absolute",
                    serde_json::Value::Bool(mouse_absolute),
                );
            }
            Ok(())
        },
    )
    .await
}
