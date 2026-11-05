use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::provider::BootProviderRegistry;
use super::{BootMode, BootOptionEntry, BootOptionsSnapshot, BootSelection};
use crate::bmc::feature::fault::{
    acquisition_fault_response, record_fault_response, unavailable_fault_response, FeatureFault,
    FeatureFaultDomain,
};
use crate::bmc::feature::{
    acquire_connected_feature, build_feature_status, feature_connect_status, retry_feature,
};
use crate::web::state::AppState;

async fn backend(
    fc: &crate::bmc::feature::connection::FeatureConnection,
) -> Option<Arc<BootProviderRegistry>> {
    fc.backend_runtime::<Arc<BootProviderRegistry>>().await
}

async fn operation_fault(
    fc: &crate::bmc::feature::connection::FeatureConnection,
    operation: &'static str,
) -> Response {
    record_fault_response(
        fc,
        StatusCode::BAD_GATEWAY,
        FeatureFault::operation(
            FeatureFaultDomain::Operation,
            "boot_operation_failed",
            if operation == "set_selection" {
                "The boot operation result is unknown; inspect device state before retrying"
            } else {
                "The boot provider could not read its state"
            },
            operation != "set_selection",
            operation,
        ),
    )
    .await
}

async fn acquire(
    state: &Arc<AppState>,
    id: &str,
) -> Result<
    (
        crate::bmc::feature::connection::FeatureGuard,
        Arc<crate::bmc::feature::connection::FeatureConnection>,
    ),
    Response,
> {
    let Some(device) = state.registry.get_dev(id).await else {
        return Err(unavailable_fault_response(StatusCode::NOT_FOUND, "boot"));
    };
    let Some(fc) = device.features.get("boot").cloned() else {
        return Err(unavailable_fault_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "boot",
        ));
    };
    match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => Ok((guard, fc)),
        Err(error) => {
            Err(acquisition_fault_response(&fc, feature_connect_status(&error), &error).await)
        }
    }
}

pub async fn status(Path(id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    let (_guard, fc) = match acquire(&state, &id).await {
        Ok(acquired) => acquired,
        Err(response) => return response,
    };
    let Some(registry) = backend(&fc).await else {
        return operation_fault(&fc, "status").await;
    };
    let runtime = super::feature_runtime(&fc).await;
    registry.request_snapshot_refresh(fc.clone(), runtime.clone());
    let snapshot = runtime.options.snapshot().await;
    let options = snapshot.value.unwrap_or(BootOptionsSnapshot {
        entries: Vec::new(),
        selection: None,
        providers: Vec::new(),
    });
    let mut value = build_feature_status(&fc).await;
    if let Some(map) = value.as_object_mut() {
        map.insert("entries".into(), serde_json::json!(options.entries));
        map.insert("selection".into(), serde_json::json!(options.selection));
        map.insert("providers".into(), serde_json::json!(options.providers));
        map.insert(
            "resource".into(),
            serde_json::json!({
                "epoch": fc.data_epoch,
                "revision": snapshot.revision,
                "name": "options",
                "stale": false,
            }),
        );
    }
    Json(value).into_response()
}

#[derive(Deserialize)]
pub struct SetSelection {
    entry_id: String,
    mode: BootMode,
    #[serde(default)]
    mutation_id: Option<String>,
}

fn selection_supported<'a>(
    entries: &'a [BootOptionEntry],
    params: &SetSelection,
) -> Option<&'a BootOptionEntry> {
    let entry = entries.iter().find(|entry| entry.id == params.entry_id)?;
    if !entry.enabled || !entry.selectable || !entry.supported_modes.contains(&params.mode) {
        return None;
    }
    Some(entry)
}

pub async fn set_selection(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<SetSelection>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
    let (_guard, fc) = match acquire(&state, &id).await {
        Ok(acquired) => acquired,
        Err(response) => return response,
    };
    let Some(registry) = backend(&fc).await else {
        return operation_fault(&fc, "set_selection").await;
    };
    let runtime = super::feature_runtime(&fc).await;
    let options = runtime
        .options
        .snapshot()
        .await
        .value
        .unwrap_or(BootOptionsSnapshot {
            entries: Vec::new(),
            selection: None,
            providers: Vec::new(),
        });
    if selection_supported(&options.entries, &params).is_none() {
        return axum::Json(serde_json::json!({"success": false, "fault": FeatureFault::operation(FeatureFaultDomain::Operation, "unsupported_boot_selection", "The requested boot selection is not supported", false, "set_selection")})).into_response();
    }
    let selection = BootSelection {
        mode: params.mode,
        entry_id: Some(params.entry_id.clone()),
    };
    let _operation = runtime.operation.lock().await;
    match registry
        .set_selection(&fc, &runtime, selection.clone(), params.mutation_id.clone())
        .await
    {
        Ok(()) => {
            fc.faults.clear_operation("set_selection").await;
            let revision = runtime.options.snapshot().await.revision;
            Json(serde_json::json!({"success": true, "selection": {"entry_id": params.entry_id, "mode": params.mode}, "mutation_id": params.mutation_id, "revision": revision})).into_response()
        }
        Err(error) => {
            tracing::warn!(device_id = %id, %error, "boot selection operation failed");
            if error.failure == bmc_feature_api::mutation::MutationFailure::Rejected {
                record_fault_response(&fc, StatusCode::BAD_REQUEST, FeatureFault::operation(FeatureFaultDomain::Operation, "boot_selection_rejected", "The requested boot selection was rejected before dispatch", false, "set_selection")).await
            } else {
                operation_fault(&fc, "set_selection").await
            }
        }
    }
    })).await
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    fn concrete_options() -> Vec<BootOptionEntry> {
        vec![BootOptionEntry {
            id: "firmware-entry".to_string(),
            provider: "wsman".to_string(),
            label: Some("Firmware entry".to_string()),
            group: Some("Specific boot entries".to_string()),
            badges: vec!["UEFI".to_string()],
            supported_modes: vec![BootMode::Once],
            enabled: true,
            selectable: true,
        }]
    }

    #[test]
    fn advertised_entry_is_accepted() {
        let params = SetSelection {
            mode: BootMode::Once,
            entry_id: "firmware-entry".to_string(),
            mutation_id: None,
        };
        assert!(selection_supported(&concrete_options(), &params).is_some());
    }

    #[test]
    fn unknown_entry_is_rejected() {
        let params = SetSelection {
            mode: BootMode::Once,
            entry_id: "missing".to_string(),
            mutation_id: None,
        };
        assert!(selection_supported(&concrete_options(), &params).is_none());
    }
}

pub async fn retry(Path(id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    retry_feature(&state, &id, "boot").await
}
