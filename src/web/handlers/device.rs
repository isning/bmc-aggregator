use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bmc_feature_api::configuration::SecretMutation;

use crate::bmc::configuration::{
    apply_device_configuration_mutation, configuration_payload_digest, configuration_read_response,
    device_configuration_dto, materialize_unmentioned_features_as_disabled,
    ConfigurationReplayError, ConfigurationReplayResult, ConfigurationScope,
    DeviceConfigurationMutation,
};
use crate::config::{AuthConfig, DeviceConfig};
use crate::web::state::AppState;

const MUTATION_ID_HEADER: &str = "x-mutation-id";
const COMPAT_AUTH_PRINCIPAL: &str = "compat-auth-session";

fn device_scope(id: &str) -> ConfigurationScope {
    ConfigurationScope::Device { id: id.to_string() }
}

fn devices_scope() -> ConfigurationScope {
    ConfigurationScope::Global {
        section: "devices".to_string(),
    }
}

fn device_config_snapshot(
    state: &AppState,
    dev: &DeviceConfig,
) -> Result<(serde_json::Value, String), String> {
    let mut value = serde_json::to_value(device_configuration_dto(dev)?)
        .map_err(|error| format!("could not serialize device configuration: {error}"))?;
    let version = state.configuration.etag(&device_scope(&dev.id), &value)?;
    value["version"] = serde_json::Value::String(version.clone());
    Ok((value, version))
}

fn json_with_etag(value: serde_json::Value, etag: &str) -> Response {
    match header::HeaderValue::from_str(etag) {
        Ok(etag) => ([(header::ETAG, etag)], Json(value)).into_response(),
        Err(error) => save_error(format!("could not build configuration ETag: {error}")),
    }
}

fn required_header(headers: &HeaderMap, name: &'static str) -> Result<String, Box<Response>> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Box::new(
                (
                    StatusCode::PRECONDITION_REQUIRED,
                    Json(serde_json::json!({
                        "success": false,
                        "code": "configuration_precondition_required",
                        "message": format!("{name} header is required"),
                    })),
                )
                    .into_response(),
            )
        })
}

fn configuration_conflict(current_version: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "success": false,
            "code": "configuration_conflict",
            "current_version": current_version,
            "message": "The device configuration changed in another session",
        })),
    )
        .into_response()
}

fn mutation_reuse_conflict() -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "success": false,
            "code": "configuration_mutation_reused",
            "message": "The mutation ID was already used with a different payload",
        })),
    )
        .into_response()
}

fn replay_response(value: serde_json::Value) -> Response {
    let etag = value
        .get("device")
        .and_then(|device| device.get("version"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    match etag {
        Some(etag) => json_with_etag(value, &etag),
        None => Json(value).into_response(),
    }
}

fn validate_device_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("device id is required".to_string());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err("device id may only contain letters, numbers, '-' and '_'".to_string());
    }
    Ok(())
}

fn validate_device_mutation(
    patch: &DeviceConfigurationMutation,
    creating: bool,
) -> Result<(), String> {
    if creating {
        validate_device_id(patch.id.as_deref().unwrap_or_default())?;
    }
    if let Some(id) = &patch.id {
        validate_device_id(id)?;
    }
    if creating
        && patch
            .endpoint
            .as_ref()
            .and_then(|endpoint| endpoint.host.as_deref())
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        return Err("host is required".to_string());
    }
    if creating && patch.name.as_deref().unwrap_or_default().trim().is_empty() {
        return Err("name is required".to_string());
    }
    if matches!(
        patch
            .credentials
            .as_ref()
            .and_then(|credentials| credentials.password.as_ref()),
        Some(SecretMutation::Replace { value }) if value.is_empty()
    ) {
        return Err("replacement password must not be empty; use clear explicitly".to_string());
    }
    if patch.endpoint.as_ref().and_then(|endpoint| endpoint.port) == Some(0) {
        return Err("port must be between 1 and 65535".to_string());
    }
    Ok(())
}

fn bad_request(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"success": false, "message": message})),
    )
        .into_response()
}

fn save_error(message: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"success": false, "message": message})),
    )
        .into_response()
}

pub async fn device_info(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    if let Some(dev) = state.registry.get_dev(&bmc_id).await {
        // Use first feature's config for host info
        let host = dev
            .features
            .values()
            .next()
            .map(|fc| fc.config.backend_params.host.clone())
            .unwrap_or_default();
        let port = dev
            .features
            .values()
            .next()
            .and_then(|fc| fc.config.backend_params.port)
            .unwrap_or(0);
        Json(serde_json::json!({
            "remote": format!("{}:{}", host, port),
            "hostname": host,
            "device_id": bmc_id,
            "platform": "bmc_aggregator",
            "version": env!("CARGO_PKG_VERSION"),
            "cpu": "",
            "memory": "",
            "mac": ""
        }))
    } else {
        Json(serde_json::json!({"error": "device not found"}))
    }
}

pub async fn snapshot(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    if let Some(dev) = state.registry.get_dev(&bmc_id).await {
        let host = dev
            .features
            .values()
            .next()
            .map(|fc| fc.config.backend_params.host.clone())
            .unwrap_or_default();
        Json(serde_json::json!({
            "device_id": bmc_id,
            "host": host,
            "available": dev.features.values().any(|fc| fc.healthy.load(std::sync::atomic::Ordering::Relaxed))
        }))
    } else {
        Json(serde_json::json!({"available": false}))
    }
}

pub async fn health(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    if let Some(dev) = state.registry.get_dev(&bmc_id).await {
        let healthy_features: Vec<_> = dev
            .features
            .iter()
            .filter_map(|(id, fc)| {
                fc.healthy
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .then_some(id)
            })
            .collect();
        Json(serde_json::json!({
            "healthy": !healthy_features.is_empty(),
            "configured": !dev.features.is_empty(),
            "healthy_features": healthy_features,
        }))
    } else {
        Json(serde_json::json!({"healthy": false}))
    }
}

pub async fn devices_info(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    if let Some(dev) = state.registry.get_dev(&bmc_id).await {
        let host = dev
            .features
            .values()
            .next()
            .map(|fc| fc.config.backend_params.host.clone())
            .unwrap_or_default();
        Json(serde_json::json!({
            "video": [],
            "serial": [],
            "audio": [],
            "udc": [],
            "hostname": host
        }))
    } else {
        Json(serde_json::json!({"error": "device not found"}))
    }
}

pub async fn bmc_list_api(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let cfg = state.config.read().await;
    let mut list = Vec::new();
    for dev in &cfg.devices {
        let online = state
            .registry
            .get_dev(&dev.id)
            .await
            .is_some_and(|managed| {
                managed
                    .features
                    .values()
                    .any(|fc| fc.healthy.load(std::sync::atomic::Ordering::Relaxed))
            });
        list.push(serde_json::json!({
            "id": dev.id,
            "name": dev.name,
            "host": dev.host,
            "port": dev.port,
            "protocol": dev.protocol,
            "online": online
        }));
    }
    Json(serde_json::json!(list))
}

pub async fn devices_config_list(State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.config.read().await;
    let devices: Result<Vec<_>, _> = cfg
        .devices
        .iter()
        .map(|device| device_config_snapshot(&state, device).map(|(value, _)| value))
        .collect();
    match devices {
        Ok(devices) => configuration_read_response(
            &state,
            &devices_scope(),
            serde_json::json!({ "devices": devices }),
        ),
        Err(message) => save_error(message),
    }
}

pub async fn devices_config_get(
    Path(device_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let cfg = state.config.read().await;
    let Some(device) = cfg.devices.iter().find(|device| device.id == device_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "success": false,
                "message": format!("device '{device_id}' not found"),
            })),
        )
            .into_response();
    };
    match device_config_snapshot(&state, device) {
        Ok((device, etag)) => json_with_etag(serde_json::json!({"device": device}), &etag),
        Err(message) => save_error(message),
    }
}

pub async fn devices_config_create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<DeviceConfigurationMutation>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        let mutation_id = match required_header(&headers, MUTATION_ID_HEADER) {
            Ok(value) => value,
            Err(response) => return *response,
        };
        if let Err(message) = validate_device_mutation(&patch, true) {
            return bad_request(message);
        }

        let _transaction = state.configuration.lock().await;
        let mut cfg = state.config.read().await.clone();
        let id = patch.id.clone().unwrap_or_default();
        let scope = device_scope(&id);
        let payload_digest = match configuration_payload_digest(&patch) {
            Ok(value) => value,
            Err(message) => return bad_request(message),
        };
        match state
            .configuration
            .replay(COMPAT_AUTH_PRINCIPAL, &scope, &mutation_id, &payload_digest)
            .await
        {
            Ok(ConfigurationReplayResult::Committed(value)) => return replay_response(value),
            Err(ConfigurationReplayError::MutationIdReusedWithDifferentPayload) => {
                return mutation_reuse_conflict();
            }
            Ok(ConfigurationReplayResult::Miss) => {}
        }
        if cfg.devices.iter().any(|dev| dev.id == id) {
            return bad_request(format!("device '{id}' already exists"));
        }

        let mut dev = DeviceConfig {
            id: id.clone(),
            name: String::new(),
            protocol: "explicit".to_string(),
            host: String::new(),
            port: 443,
            kvm_port: None,
            ipmi_host: None,
            ipmi_port: Some(623),
            auth: AuthConfig {
                username: "admin".to_string(),
                password: String::new(),
            },
            max_fps: 60,
            video_backend: "mahogany".to_string(),
            encoder_backend: "software".to_string(),
            audio_backend: None,
            features: HashMap::new(),
            video_device: None,
            video_format: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            video_quality: None,
        };
        if let Err(message) = apply_device_configuration_mutation(&mut dev, patch) {
            return bad_request(message);
        }
        if let Err(message) = materialize_unmentioned_features_as_disabled(&mut dev) {
            return save_error(message);
        }
        dev.normalize_feature_config();
        if let Err(message) = device_configuration_dto(&dev) {
            return bad_request(message);
        }
        cfg.devices.push(dev);
        let changed_scopes = [scope.clone(), devices_scope()];
        match state.commit_config_scoped(cfg, &changed_scopes).await {
            Ok(report) => {
                let affected_features = report.features_for(&id);
                let change = state.configuration.publish(
                    scope.clone(),
                    mutation_id.clone(),
                    vec!["device".to_string()],
                    affected_features,
                );
                let list_change = state.configuration.publish(
                    devices_scope(),
                    mutation_id.clone(),
                    vec!["devices".to_string()],
                    Vec::new(),
                );
                let cfg = state.config.read().await;
                let Some(device) = cfg.devices.iter().find(|device| device.id == id) else {
                    return save_error("created device was not published".to_string());
                };
                let (device, etag) = match device_config_snapshot(&state, device) {
                    Ok(value) => value,
                    Err(message) => return save_error(message),
                };
                let response = serde_json::json!({
                    "success": true,
                    "mutation_id": mutation_id,
                    "revision": list_change.revision.max(change.revision),
                    "device": device,
                });
                state
                    .configuration
                    .remember(
                        COMPAT_AUTH_PRINCIPAL,
                        &scope,
                        &change.mutation_id,
                        payload_digest,
                        response.clone(),
                    )
                    .await;
                json_with_etag(response, &etag)
            }
            Err(message) => save_error(message),
        }
    }))
    .await
}

pub async fn devices_config_update(
    Path(device_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<DeviceConfigurationMutation>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {

    let mutation_id = match required_header(&headers, MUTATION_ID_HEADER) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let expected_version = match required_header(&headers, header::IF_MATCH.as_str()) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    if let Err(message) = validate_device_mutation(&patch, false) {
        return bad_request(message);
    }

    let _transaction = state.configuration.lock().await;
    let scope = device_scope(&device_id);
    let payload_digest = match configuration_payload_digest(&patch) {
        Ok(value) => value,
        Err(message) => return bad_request(message),
    };
    match state
        .configuration
        .replay(COMPAT_AUTH_PRINCIPAL, &scope, &mutation_id, &payload_digest)
        .await
    {
        Ok(ConfigurationReplayResult::Committed(value)) => return replay_response(value),
        Err(ConfigurationReplayError::MutationIdReusedWithDifferentPayload) => {
            return mutation_reuse_conflict();
        }
        Ok(ConfigurationReplayResult::Miss) => {}
    }
    let mut cfg = state.config.read().await.clone();
    let Some(index) = cfg.devices.iter().position(|dev| dev.id == device_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"success": false, "message": format!("device '{device_id}' not found")})),
        )
            .into_response();
    };
    let (_, current_version) = match device_config_snapshot(&state, &cfg.devices[index]) {
        Ok(value) => value,
        Err(message) => return save_error(message),
    };
    if expected_version != current_version {
        return configuration_conflict(&current_version);
    }
    cfg.devices[index].normalize_feature_config();
    let previous_owned = match configuration_payload_digest(&cfg.devices[index]) {
        Ok(value) => value,
        Err(message) => return save_error(message),
    };
    if let Some(new_id) = patch.id.as_deref() {
        if new_id != device_id && cfg.devices.iter().any(|dev| dev.id == new_id) {
            return bad_request(format!("device '{new_id}' already exists"));
        }
    }

    if let Err(message) = apply_device_configuration_mutation(&mut cfg.devices[index], patch) {
        return bad_request(message);
    }
    cfg.devices[index].normalize_feature_config();
    if let Err(message) = device_configuration_dto(&cfg.devices[index]) {
        return bad_request(message);
    }
    let new_id = cfg.devices[index].id.clone();
    let current_owned = match configuration_payload_digest(&cfg.devices[index]) {
        Ok(value) => value,
        Err(message) => return save_error(message),
    };
    if previous_owned == current_owned {
        let (device, etag) = match device_config_snapshot(&state, &cfg.devices[index]) {
            Ok(value) => value,
            Err(message) => return save_error(message),
        };
        let response = serde_json::json!({
            "success": true,
            "mutation_id": mutation_id,
            "revision": state.configuration.current_revision(),
            "affected_features": [],
            "device": device,
        });
        state
            .configuration
            .remember(
                COMPAT_AUTH_PRINCIPAL,
                &scope,
                response["mutation_id"].as_str().unwrap_or_default(),
                payload_digest,
                response.clone(),
            )
            .await;
        return json_with_etag(response, &etag);
    }
    if device_id != new_id {
        if let Some(msd) = cfg.msd.remove(&device_id) {
            cfg.msd.insert(new_id.clone(), msd);
        }
    }
    let published_scope = device_scope(&new_id);
    let mut changed_scopes = vec![published_scope.clone(), devices_scope()];
    if device_id != new_id {
        changed_scopes.push(scope.clone());
    }
    match state.commit_config_scoped(cfg, &changed_scopes).await {
        Ok(report) => {
            let affected_features = report.features_for(&new_id);
            let old_change = (device_id != new_id).then(|| {
                state.configuration.publish(
                    scope.clone(),
                    mutation_id.clone(),
                    vec!["device.id".to_string()],
                    Vec::new(),
                )
            });
            let change = state.configuration.publish(
                published_scope,
                mutation_id.clone(),
                vec!["device".to_string()],
                affected_features,
            );
            let list_change = state.configuration.publish(
                devices_scope(),
                mutation_id.clone(),
                vec!["devices".to_string()],
                Vec::new(),
            );
            let cfg = state.config.read().await;
            let Some(device) = cfg.devices.iter().find(|device| device.id == new_id) else {
                return save_error("updated device was not published".to_string());
            };
            let (device, etag) = match device_config_snapshot(&state, device) {
                Ok(value) => value,
                Err(message) => return save_error(message),
            };
            let response = serde_json::json!({
                "success": true,
                "mutation_id": mutation_id,
                "revision": old_change
                    .as_ref()
                    .map_or(change.revision, |old| old.revision.max(change.revision))
                    .max(list_change.revision),
                "device": device,
            });
            state
                .configuration
                .remember(
                    COMPAT_AUTH_PRINCIPAL,
                    &scope,
                    &change.mutation_id,
                    payload_digest,
                    response.clone(),
                )
                .await;
            json_with_etag(response, &etag)
        }
        Err(message) => save_error(message),
    }

    })).await
}

pub async fn devices_config_delete(
    Path(device_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {

    let mutation_id = match required_header(&headers, MUTATION_ID_HEADER) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let expected_version = match required_header(&headers, header::IF_MATCH.as_str()) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let _transaction = state.configuration.lock().await;
    let scope = device_scope(&device_id);
    let payload_digest = match configuration_payload_digest(&serde_json::json!({"delete": true})) {
        Ok(value) => value,
        Err(message) => return bad_request(message),
    };
    match state
        .configuration
        .replay(COMPAT_AUTH_PRINCIPAL, &scope, &mutation_id, &payload_digest)
        .await
    {
        Ok(ConfigurationReplayResult::Committed(value)) => return replay_response(value),
        Err(ConfigurationReplayError::MutationIdReusedWithDifferentPayload) => {
            return mutation_reuse_conflict();
        }
        Ok(ConfigurationReplayResult::Miss) => {}
    }
    let mut cfg = state.config.read().await.clone();
    let Some(device) = cfg.devices.iter().find(|device| device.id == device_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"success": false, "message": format!("device '{device_id}' not found")})),
        )
            .into_response();
    };
    let (_, current_version) = match device_config_snapshot(&state, device) {
        Ok(value) => value,
        Err(message) => return save_error(message),
    };
    if expected_version != current_version {
        return configuration_conflict(&current_version);
    }
    cfg.devices.retain(|dev| dev.id != device_id);
    cfg.msd.remove(&device_id);
    let changed_scopes = [scope.clone(), devices_scope()];
    match state.commit_config_scoped(cfg, &changed_scopes).await {
        Ok(report) => {
            let affected_features = report.features_for(&device_id);
            let change = state.configuration.publish(
                scope.clone(),
                mutation_id.clone(),
                vec!["device".to_string()],
                affected_features,
            );
            let list_change = state.configuration.publish(
                devices_scope(),
                mutation_id.clone(),
                vec!["devices".to_string()],
                Vec::new(),
            );
            let response = serde_json::json!({
                "success": true,
                "mutation_id": mutation_id,
                "revision": list_change.revision.max(change.revision),
            });
            state
                .configuration
                .remember(
                    COMPAT_AUTH_PRINCIPAL,
                    &scope,
                    &change.mutation_id,
                    payload_digest,
                    response.clone(),
                )
                .await;
            Json(response).into_response()
        }
        Err(message) => save_error(message),
    }

    })).await
}
