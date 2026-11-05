use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;

use super::handler::DEFAULT_MAX_IMAGE_BYTES;
use crate::bmc::configuration::{
    configuration_read_response, mutate_configuration_scope, ConfigurationHttpError,
    ConfigurationScope,
};
use crate::bmc::feature::config_edit::{optional_string, set_device_feature_backend};
use crate::web::state::AppState;

fn scope(id: &str) -> ConfigurationScope {
    ConfigurationScope::Feature {
        device_id: id.to_string(),
        feature_id: "msd".to_string(),
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
    let msd = config.msd.get(id).cloned().unwrap_or_default();
    let max_image_bytes = dev
        .feature_u64_param("msd", "max_image_bytes")
        .unwrap_or(DEFAULT_MAX_IMAGE_BYTES);
    Ok(serde_json::json!({
        "enabled": true,
        "backend": dev.feature_backend("msd"),
        "drive_size_mb": msd.drive_size_mb,
        "msd_dir": msd.storage_dir,
        "storage_dir": msd.storage_dir,
        "storage_path": msd.storage_dir,
        "max_image_bytes": max_image_bytes,
    }))
}

pub async fn config_msd(Path(id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.config.read().await;
    match snapshot(&cfg, &id) {
        Ok(value) => configuration_read_response(&state, &scope(&id), value),
        Err(error) => error.into_response(),
    }
}

pub async fn config_msd_update(
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
        vec!["features.msd".to_string()],
        |config| snapshot(config, &id),
        |config| {
            use crate::bmc::feature::config_edit::{validate_patch, PatchValue};
            validate_patch(
                &patch,
                &[
                    ("backend", PatchValue::Text),
                    (
                        "drive_size_mb",
                        PatchValue::Integer {
                            min: 1,
                            max: u64::MAX / (1024 * 1024),
                        },
                    ),
                    (
                        "max_image_bytes",
                        PatchValue::Integer {
                            min: 1,
                            max: u64::MAX,
                        },
                    ),
                    ("storage_dir", PatchValue::NullableText),
                    ("storage_path", PatchValue::NullableText),
                    ("msd_dir", PatchValue::NullableText),
                ],
            )?;
            crate::bmc::feature::config_edit::validate_aliases(
                &patch,
                &["storage_dir", "storage_path", "msd_dir"],
            )?;
            let Some(dev_index) = config.devices.iter().position(|dev| dev.id == id) else {
                return Err(ConfigurationHttpError::not_found(format!(
                    "device '{id}' not found"
                )));
            };
            if let Some(backend) = patch.get("backend").and_then(|value| value.as_str()) {
                set_device_feature_backend(&mut config.devices[dev_index], "msd", backend)
                    .map_err(|error| {
                        ConfigurationHttpError::bad_request("backend_unregistered", error)
                    })?;
            }
            if let Some(value) = patch.get("max_image_bytes") {
                let Some(max_image_bytes) = value.as_u64().filter(|value| *value > 0) else {
                    return Err(ConfigurationHttpError::bad_request(
                        "invalid_image_size_limit",
                        "The maximum image size must be a positive integer",
                    ));
                };
                config.devices[dev_index]
                    .features
                    .entry("msd".to_string())
                    .or_default()
                    .feature_params
                    .insert(
                        "max_image_bytes".to_string(),
                        serde_json::json!(max_image_bytes),
                    );
            }
            let msd = config.msd.entry(id.clone()).or_default();
            if let Some(drive_size_mb) = patch.get("drive_size_mb").and_then(|v| v.as_u64()) {
                msd.drive_size_mb = drive_size_mb;
            }
            if let Some(storage_dir) = optional_string(&patch, "storage_dir")
                .or_else(|| optional_string(&patch, "storage_path"))
            {
                msd.storage_dir = storage_dir;
            }
            if let Some(msd_dir) = optional_string(&patch, "msd_dir") {
                msd.storage_dir = msd_dir;
            }
            Ok(())
        },
    )
    .await
}
