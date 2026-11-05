use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::{IdentifyBackend, IdentifyMode, IdentifyStatusSnapshot};
use crate::bmc::feature::fault::{
    acquisition_fault_response, record_fault_response, unavailable_fault_response, FeatureFault,
    FeatureFaultDomain,
};
use crate::bmc::feature::{
    acquire_connected_feature, build_feature_status, feature_connect_status, retry_feature,
};
use crate::web::state::AppState;

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
        return Err(unavailable_fault_response(
            StatusCode::NOT_FOUND,
            "identify",
        ));
    };
    let Some(fc) = device.features.get("identify").cloned() else {
        return Err(unavailable_fault_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "identify",
        ));
    };
    match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => Ok((guard, fc)),
        Err(error) => {
            Err(acquisition_fault_response(&fc, feature_connect_status(&error), &error).await)
        }
    }
}

async fn fault(
    fc: &crate::bmc::feature::connection::FeatureConnection,
    operation: &'static str,
) -> Response {
    record_fault_response(
        fc,
        StatusCode::BAD_GATEWAY,
        FeatureFault::operation(
            FeatureFaultDomain::Operation,
            "identify_operation_failed",
            "The identify provider could not read its state",
            true,
            operation,
        ),
    )
    .await
}

async fn set_fault(
    fc: &crate::bmc::feature::connection::FeatureConnection,
    denied: bool,
    failure: bmc_feature_api::mutation::MutationFailure,
) -> Response {
    record_fault_response(
        fc,
        if denied {
            StatusCode::FORBIDDEN
        } else if failure == bmc_feature_api::mutation::MutationFailure::Rejected {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::BAD_GATEWAY
        },
        FeatureFault::operation(
            FeatureFaultDomain::Operation,
            if denied {
                "identify_permission_denied"
            } else if failure == bmc_feature_api::mutation::MutationFailure::Rejected {
                "identify_rejected"
            } else {
                "identify_result_unknown"
            },
            if denied {
                "The BMC denied permission to change the identify light"
            } else if failure == bmc_feature_api::mutation::MutationFailure::Rejected {
                "The requested identify change was rejected"
            } else {
                "The identify operation result is unknown; inspect device state before retrying"
            },
            false,
            "set",
        ),
    )
    .await
}

pub async fn status(Path(id): Path<String>, State(app): State<Arc<AppState>>) -> Response {
    let (_guard, fc) = match acquire(&app, &id).await {
        Ok(acquired) => acquired,
        Err(response) => return response,
    };
    let backend_generation = fc.backend_generation();
    let Some(backend) = fc.backend_runtime::<Arc<dyn IdentifyBackend>>().await else {
        return fault(&fc, "status").await;
    };
    let runtime = super::feature_runtime(&fc).await;
    match runtime
        .status
        .refresh_at(&fc, backend_generation, || async move {
            let capabilities = backend.capabilities().await;
            let identify = backend.state().await?;
            Ok(IdentifyStatusSnapshot {
                capabilities,
                identify,
            })
        })
        .await
    {
        Ok(snapshot) => {
            let Some(status) = snapshot.value else {
                return fault(&fc, "status").await;
            };
            fc.faults.clear_operation("status").await;
            let mut value = build_feature_status(&fc).await;
            if let Some(map) = value.as_object_mut() {
                map.insert(
                    "capabilities".into(),
                    serde_json::json!(status.capabilities),
                );
                map.insert("identify".into(), serde_json::json!(status.identify));
                map.insert(
                    "resource".into(),
                    serde_json::json!({
                        "epoch": fc.data_epoch,
                        "revision": snapshot.revision,
                        "stale": false,
                    }),
                );
            }
            Json(value).into_response()
        }
        Err(_) => fault(&fc, "status").await,
    }
}

#[derive(Deserialize)]
pub struct SetIdentify {
    mode: IdentifyMode,
    duration_seconds: Option<u8>,
    #[serde(default)]
    mutation_id: Option<String>,
}

pub async fn set(
    Path(id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(params): Json<SetIdentify>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
    let (_guard, fc) = match acquire(&app, &id).await {
        Ok(acquired) => acquired,
        Err(response) => return response,
    };
    let backend_generation = fc.backend_generation();
    let Some(backend) = fc.backend_runtime::<Arc<dyn IdentifyBackend>>().await else {
        return fault(&fc, "set").await;
    };
    let Ok(_backend_effect) = fc.backend_effect_guard(backend_generation).await else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "identify");
    };
    let caps = backend.capabilities().await;
    if !caps.writable {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"success": false, "fault": FeatureFault::operation(FeatureFaultDomain::Operation, "identify_read_only", "The BMC reports identify state but does not permit this account to change it", false, "set")})),
        )
            .into_response();
    }
    if !caps.modes.contains(&params.mode)
        || params
            .duration_seconds
            .is_some_and(|value| value == 0 || value > caps.max_temporary_seconds)
        || (params.mode != IdentifyMode::Temporary && params.duration_seconds.is_some())
    {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"success": false, "fault": FeatureFault::operation(FeatureFaultDomain::Operation, "invalid_identify_request", "The requested identify mode or duration is invalid", false, "set")}))).into_response();
    }
    let runtime = super::feature_runtime(&fc).await;
    match runtime
        .status
        .try_mutate_at::<bmc_feature_api::mutation::MutationError, _, _>(&fc, backend_generation, params.mutation_id.clone(), || async {
            backend
                .set_outcome(params.mode, params.duration_seconds)
                .await?;
            let identify = backend.state().await.map_err(bmc_feature_api::mutation::MutationError::unknown)?;
            Ok(IdentifyStatusSnapshot {
                capabilities: caps.clone(),
                identify,
            })
        })
        .await
    {
        Ok(snapshot) => {
            fc.faults.clear_operation("set").await;
            Json(serde_json::json!({"success": true, "mutation_id": params.mutation_id, "revision": snapshot.revision})).into_response()
        }
        Err(error) => set_fault(&fc, !backend.capabilities().await.writable, error.failure).await,
    }
    })).await
}

pub async fn retry(Path(id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    retry_feature(&state, &id, "identify").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmc::feature::{
        config::{FeatureConfig, FeatureConfigs},
        connection::FeatureConnection,
        device_registry::DeviceRegistry,
        managed::ManagedDevice,
    };
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn acquisition_errors_preserve_status_and_fault_for_reads_and_mutations() {
        let directory = tempfile::tempdir().unwrap();
        let config = Arc::new(RwLock::new(serde_yaml::from_str::<crate::config::Config>(
            "server:\n  listen: 127.0.0.1:0\n  frontend_dir: frontend/dist\ndevices: []\nmsd: {}\n").unwrap()));
        let registry = DeviceRegistry::new();
        let state = Arc::new(AppState {
            registry: registry.clone(),
            config: config.clone(),
            config_path: directory
                .path()
                .join("config.yaml")
                .to_string_lossy()
                .into_owned(),
            configuration: crate::bmc::configuration::ConfigurationCoordinator::new(),
            msd_storage: crate::bmc::msd_storage::FsMsdStorage::new(
                directory.path().to_string_lossy().into_owned(),
                config,
            ),
            active_downloads: RwLock::new(Default::default()),
            webtransport: RwLock::new(None),
        });
        let fc = FeatureConnection::new(
            "busy".into(),
            "identify".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        fc.lifecycle_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        fc.faults
            .record(FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "session_in_use",
                "Backend is busy",
                true,
            ))
            .await;
        registry
            .insert(
                "busy".into(),
                Arc::new(ManagedDevice::new(
                    "busy".into(),
                    FeatureConfigs {
                        features: Default::default(),
                    },
                    [("identify".into(), fc)].into(),
                )),
            )
            .await;
        for (id, expected, fault) in [
            ("missing", StatusCode::NOT_FOUND, "feature_unavailable"),
            ("busy", StatusCode::CONFLICT, "session_in_use"),
        ] {
            for response in [
                status(Path(id.into()), State(state.clone())).await,
                set(
                    Path(id.into()),
                    State(state.clone()),
                    Json(SetIdentify {
                        mode: IdentifyMode::Off,
                        duration_seconds: None,
                        mutation_id: None,
                    }),
                )
                .await,
            ] {
                assert_eq!(response.status(), expected);
                let body = axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap();
                let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(json["fault"]["code"], fault);
            }
        }
    }
}
