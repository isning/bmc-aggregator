use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use super::{
    configuration_payload_digest, ConfigurationReplayError, ConfigurationReplayResult,
    ConfigurationScope,
};
use crate::config::Config;
use crate::web::state::AppState;

const MUTATION_ID_HEADER: &str = "x-mutation-id";
const COMPAT_AUTH_PRINCIPAL: &str = "compat-auth-session";
const REPLAY_ETAG_FIELD: &str = "configuration_etag";
const REPLAY_BODY_FIELD: &str = "configuration_response";

#[derive(Debug)]
pub struct ConfigurationHttpError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ConfigurationHttpError {
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "configuration_resource_not_found",
            message: message.into(),
        }
    }

    pub fn into_response(self) -> Response {
        configuration_error(self.status, self.code, self.message)
    }
}

fn configuration_error(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
) -> Response {
    (
        status,
        Json(serde_json::json!({
            "success": false,
            "code": code,
            "message": message.into(),
        })),
    )
        .into_response()
}

fn required_header(headers: &HeaderMap, name: &'static str) -> Result<String, Box<Response>> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Box::new(configuration_error(
                StatusCode::PRECONDITION_REQUIRED,
                "configuration_precondition_required",
                format!("{name} header is required"),
            ))
        })
}

fn json_with_etag(value: serde_json::Value, etag: &str) -> Response {
    match header::HeaderValue::from_str(etag) {
        Ok(etag) => ([(header::ETAG, etag)], Json(value)).into_response(),
        Err(error) => configuration_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_etag_invalid",
            format!("could not build configuration ETag: {error}"),
        ),
    }
}

fn replay_response(value: serde_json::Value) -> Response {
    let etag = value
        .get(REPLAY_ETAG_FIELD)
        .and_then(serde_json::Value::as_str);
    let body = value.get(REPLAY_BODY_FIELD).cloned();
    match (etag, body) {
        (Some(etag), Some(body)) => json_with_etag(body, etag),
        _ => Json(value).into_response(),
    }
}

fn response_body(
    snapshot: serde_json::Value,
    mutation_id: &str,
    revision: u64,
    affected_features: Vec<String>,
) -> Result<serde_json::Value, Box<Response>> {
    let Some(fields) = snapshot.as_object() else {
        return Err(Box::new(configuration_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_snapshot_invalid",
            "configuration snapshots must be JSON objects",
        )));
    };
    let mut fields = fields.clone();
    fields.insert("success".to_string(), serde_json::Value::Bool(true));
    fields.insert(
        "mutation_id".to_string(),
        serde_json::Value::String(mutation_id.to_string()),
    );
    fields.insert("revision".to_string(), serde_json::json!(revision));
    fields.insert(
        "affected_features".to_string(),
        serde_json::json!(affected_features),
    );
    Ok(serde_json::Value::Object(fields))
}

pub fn configuration_read_response(
    state: &AppState,
    scope: &ConfigurationScope,
    snapshot: serde_json::Value,
) -> Response {
    match state.configuration.etag(scope, &snapshot) {
        Ok(etag) => json_with_etag(snapshot, &etag),
        Err(error) => configuration_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_etag_failed",
            error,
        ),
    }
}

fn owned_config_digest(config: &Config) -> Result<String, String> {
    let mut normalized = config.clone();
    normalized.normalize_feature_config();
    configuration_payload_digest(&normalized)
}

pub async fn mutate_configuration_scope<P, Snapshot, Apply>(
    state: &std::sync::Arc<AppState>,
    headers: &HeaderMap,
    scope: ConfigurationScope,
    payload: &P,
    changed_paths: Vec<String>,
    snapshot: Snapshot,
    apply: Apply,
) -> Response
where
    P: Serialize + ?Sized,
    Snapshot: Fn(&Config) -> Result<serde_json::Value, ConfigurationHttpError>,
    Apply: FnOnce(&mut Config) -> Result<(), ConfigurationHttpError>,
{
    let mutation_id = match required_header(headers, MUTATION_ID_HEADER) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let expected_version = match required_header(headers, header::IF_MATCH.as_str()) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let payload_digest = match configuration_payload_digest(payload) {
        Ok(value) => value,
        Err(error) => {
            return configuration_error(
                StatusCode::BAD_REQUEST,
                "configuration_payload_invalid",
                error,
            )
        }
    };

    let _transaction = state.configuration.lock().await;
    match state
        .configuration
        .replay(COMPAT_AUTH_PRINCIPAL, &scope, &mutation_id, &payload_digest)
        .await
    {
        Ok(ConfigurationReplayResult::Committed(value)) => return replay_response(value),
        Err(ConfigurationReplayError::MutationIdReusedWithDifferentPayload) => {
            return configuration_error(
                StatusCode::CONFLICT,
                "configuration_mutation_reused",
                "The mutation ID was already used with a different payload",
            );
        }
        Ok(ConfigurationReplayResult::Miss) => {}
    }

    let mut candidate = state.config.read().await.clone();
    // Public snapshots intentionally hide secret contents and cannot establish equality.
    let previous_owned = match owned_config_digest(&candidate) {
        Ok(value) => value,
        Err(error) => {
            return configuration_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_snapshot_failed",
                error,
            )
        }
    };
    let previous = match snapshot(&candidate) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let current_version = match state.configuration.etag(&scope, &previous) {
        Ok(value) => value,
        Err(error) => {
            return configuration_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_etag_failed",
                error,
            )
        }
    };
    if expected_version != current_version {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "success": false,
                "code": "configuration_conflict",
                "current_version": current_version,
                "message": "The configuration changed in another session",
            })),
        )
            .into_response();
    }

    if let Err(error) = apply(&mut candidate) {
        return error.into_response();
    }
    let next = match snapshot(&candidate) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };

    let current_owned = match owned_config_digest(&candidate) {
        Ok(value) => value,
        Err(error) => {
            return configuration_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_snapshot_failed",
                error,
            )
        }
    };
    if previous_owned == current_owned {
        let body = match response_body(
            next,
            &mutation_id,
            state.configuration.current_revision(),
            Vec::new(),
        ) {
            Ok(body) => body,
            Err(response) => return *response,
        };
        let replay = serde_json::json!({
            REPLAY_ETAG_FIELD: current_version,
            REPLAY_BODY_FIELD: body,
        });
        state
            .configuration
            .remember(
                COMPAT_AUTH_PRINCIPAL,
                &scope,
                &mutation_id,
                payload_digest,
                replay,
            )
            .await;
        return json_with_etag(body, &current_version);
    }

    let state = state.clone();
    crate::web::state::mutation_response(tokio::spawn(async move {
        let _transaction = _transaction;
        let report = match state
            .commit_config_scoped(candidate, std::slice::from_ref(&scope))
            .await
        {
            Ok(report) => report,
            Err(error) => {
                return configuration_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "configuration_save_failed",
                    error,
                )
            }
        };
        let affected_features = match &scope {
            ConfigurationScope::Device { id } => report.features_for(id),
            ConfigurationScope::Feature { device_id, .. } => report.features_for(device_id),
            ConfigurationScope::Global { .. } | ConfigurationScope::Options { .. } => Vec::new(),
        };
        let change = state.configuration.publish(
            scope.clone(),
            mutation_id.clone(),
            changed_paths,
            affected_features.clone(),
        );
        let etag = match state.configuration.etag(&scope, &next) {
            Ok(value) => value,
            Err(error) => {
                return configuration_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "configuration_etag_failed",
                    error,
                )
            }
        };
        let body = match response_body(next, &mutation_id, change.revision, affected_features) {
            Ok(body) => body,
            Err(response) => return *response,
        };
        state
            .configuration
            .remember(
                COMPAT_AUTH_PRINCIPAL,
                &scope,
                &mutation_id,
                payload_digest,
                serde_json::json!({
                    REPLAY_ETAG_FIELD: etag,
                    REPLAY_BODY_FIELD: body,
                }),
            )
            .await;
        json_with_etag(body, &etag)
    }))
    .await
}
