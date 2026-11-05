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
        feature_id: "power".to_string(),
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
    let backend = dev.feature_backend("power");
    let params = backend.map(|backend| {
        crate::bmc::feature::device_registry::management_backend_params(dev, "power", backend)
    });
    Ok(serde_json::json!({
        "backend": backend,
        "enabled": backend.is_some(),
        "host": params.as_ref().map(|params| params.host.as_str()),
        "port": params.and_then(|params| params.port),
    }))
}

pub async fn config_power(Path(id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.config.read().await;
    match snapshot(&cfg, &id) {
        Ok(value) => configuration_read_response(&state, &scope(&id), value),
        Err(error) => error.into_response(),
    }
}

pub async fn config_power_update(
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
        vec!["features.power".to_string()],
        |config| snapshot(config, &id),
        |config| {
            use crate::bmc::feature::config_edit::{validate_patch, PatchValue};
            validate_patch(
                &patch,
                &[
                    ("backend", PatchValue::Text),
                    ("host", PatchValue::Text),
                    ("ipmi_host", PatchValue::Text),
                    ("port", PatchValue::Integer { min: 1, max: 65535 }),
                    ("ipmi_port", PatchValue::Integer { min: 1, max: 65535 }),
                ],
            )?;
            crate::bmc::feature::config_edit::validate_aliases(&patch, &["host", "ipmi_host"])?;
            crate::bmc::feature::config_edit::validate_aliases(&patch, &["port", "ipmi_port"])?;
            let dev = config
                .devices
                .iter_mut()
                .find(|dev| dev.id == id)
                .ok_or_else(|| {
                    ConfigurationHttpError::not_found(format!("device '{id}' not found"))
                })?;
            if let Some(backend) = patch.get("backend") {
                let backend = backend.as_str().ok_or_else(|| {
                    ConfigurationHttpError::bad_request(
                        "configuration_value_invalid",
                        "backend must be a string",
                    )
                })?;
                set_device_feature_backend(dev, "power", backend).map_err(|error| {
                    ConfigurationHttpError::bad_request("backend_unregistered", error)
                })?;
            }
            let params = &mut dev
                .features
                .entry("power".to_owned())
                .or_default()
                .backend_params;
            for (canonical, alias) in [("host", "ipmi_host"), ("port", "ipmi_port")] {
                if let Some(value) = patch.get(canonical).or_else(|| patch.get(alias)) {
                    let valid = if canonical == "host" {
                        value.as_str().is_some_and(|host| !host.trim().is_empty())
                    } else {
                        value
                            .as_u64()
                            .is_some_and(|port| (1..=65535).contains(&port))
                    };
                    if !valid {
                        return Err(ConfigurationHttpError::bad_request(
                            "configuration_value_invalid",
                            format!("invalid power {canonical}"),
                        ));
                    }
                    params.insert(canonical.to_owned(), value.clone());
                }
            }
            Ok(())
        },
    )
    .await
}
