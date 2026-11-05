use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::bmc::feature::fault::{
    acquisition_fault_response, fault_response, record_fault_response, unavailable_fault_response,
    FeatureFault, FeatureFaultDomain,
};
use crate::bmc::feature::power::PowerBackend;
use crate::bmc::feature::{
    acquire_connected_feature, build_feature_status, feature_connect_status, retry_feature,
};
use crate::web::state::AppState;

#[derive(Deserialize)]
pub struct PowerActionParams {
    action: String,
    #[serde(default)]
    mutation_id: Option<String>,
}

#[derive(Deserialize)]
pub struct WolParams {
    mac_address: String,
}

async fn get_backend(
    fc: &Arc<crate::bmc::feature::connection::FeatureConnection>,
) -> Option<Arc<dyn PowerBackend>> {
    fc.backend_runtime::<Arc<dyn PowerBackend>>().await
}

fn device_not_found(operation: &'static str) -> Response {
    fault_response(
        StatusCode::NOT_FOUND,
        FeatureFault::operation(
            FeatureFaultDomain::Lifecycle,
            "device_not_found",
            "The requested BMC device was not found",
            false,
            operation,
        ),
    )
}

async fn backend_unavailable(
    fc: &crate::bmc::feature::connection::FeatureConnection,
    operation: &'static str,
) -> Response {
    record_fault_response(
        fc,
        StatusCode::SERVICE_UNAVAILABLE,
        FeatureFault::operation(
            FeatureFaultDomain::Lifecycle,
            "backend_runtime_unavailable",
            "The power backend runtime is not available",
            true,
            operation,
        ),
    )
    .await
}

pub async fn power_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return Json(serde_json::json!({
            "available": false,
            "online": false,
            "power_status": "unknown",
            "fault": FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "device_not_found",
                "The requested BMC device was not found",
                false,
            ),
        }));
    };
    let Some(fc) = dev.features.get("power").cloned() else {
        return Json(serde_json::json!({
            "available": false,
            "online": false,
            "power_status": "unknown",
            "fault": FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "feature_unavailable",
                "The power feature is not available for this device",
                false,
            ),
        }));
    };

    let runtime = crate::bmc::feature::power::feature_runtime(&fc).await;
    let mut stale = false;
    let snapshot = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => {
            let backend_generation = fc.backend_generation();
            let result = match get_backend(&fc).await {
                Some(backend) => {
                    runtime
                        .status
                        .refresh_at(&fc, backend_generation, || async move {
                            let capabilities = backend.capabilities().await;
                            let power_status = backend.status().await?;
                            Ok(crate::bmc::feature::power::PowerStatusSnapshot {
                                power_status,
                                capabilities,
                            })
                        })
                        .await
                }
                None => Err("backend runtime unavailable".to_string()),
            };
            drop(guard);
            match result {
                Ok(snapshot) => {
                    fc.faults.clear_operation("status").await;
                    Some(snapshot)
                }
                Err(_) => {
                    stale = true;
                    fc.faults
                        .record(FeatureFault::operation(
                            FeatureFaultDomain::Operation,
                            "status_failed",
                            "The power backend could not read the current power state",
                            true,
                            "status",
                        ))
                        .await;
                    // Keep the last observation's revision so clients accept
                    // updated stale/fault metadata instead of discarding it as old.
                    Some(runtime.status.snapshot().await)
                }
            }
        }
        Err(error) => {
            stale = true;
            // `acquisition_fault_response` is for HTTP actions; status is a
            // snapshot, so persist the same sanitized lifecycle mapping.
            let fault = fc
                .faults
                .active()
                .await
                .unwrap_or_else(|| crate::bmc::feature::fault::lifecycle_connect_fault(&error));
            fc.faults.record(fault).await;
            let cached = runtime.status.snapshot().await;
            if cached.value.is_some() {
                Some(cached)
            } else {
                None
            }
        }
    };

    let mut response = build_feature_status(&fc).await;
    if let serde_json::Value::Object(ref mut map) = response {
        let revision = snapshot
            .as_ref()
            .map(|snapshot| snapshot.revision)
            .unwrap_or(0);
        let value = snapshot.and_then(|snapshot| snapshot.value);
        map.insert(
            "power_status".into(),
            serde_json::json!(value
                .as_ref()
                .map(|value| value.power_status.as_str())
                .unwrap_or("unknown")),
        );
        map.insert(
            "capabilities".into(),
            serde_json::json!(value.map(|value| value.capabilities).unwrap_or_default()),
        );
        map.insert(
            "resource".into(),
            serde_json::json!({
                "epoch": fc.data_epoch,
                "revision": revision,
                "stale": stale,
            }),
        );
    }
    Json(response)
}

pub async fn power_atx_devices(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    if state.registry.get_dev(&bmc_id).await.is_none() {
        return device_not_found("atx_devices");
    }
    Json(serde_json::json!({
        "atx_version": "1.0",
        "voltage": [],
        "power": {"supported": true}
    }))
    .into_response()
}

pub async fn power_control(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<PowerActionParams>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return device_not_found("control");
    };
    let Some(fc) = dev.features.get("power").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "power");
    };
    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => guard,
        Err(error) => {
            return acquisition_fault_response(&fc, feature_connect_status(&error), &error).await
        }
    };
    let backend_generation = fc.backend_generation();
    let Some(backend) = get_backend(&fc).await else {
        return backend_unavailable(&fc, "control").await;
    };
    let Ok(_backend_effect) = fc.backend_effect_guard(backend_generation).await else {
        return backend_unavailable(&fc, "control").await;
    };
    let runtime = crate::bmc::feature::power::feature_runtime(&fc).await;
    let action = params.action.clone();
    let mutation_id = params.mutation_id.clone();
    let mutation_backend = backend.clone();
    let result = runtime
        .status
        .mutate_at(&fc, backend_generation, mutation_id.clone(), move || async move {
            mutation_backend.control(&action).await?;
            let capabilities = mutation_backend.capabilities().await;
            // Dispatch acceptance remains valid when immediate observation fails.
            let power_status = mutation_backend
                .status()
                .await
                .unwrap_or_else(|_| "unknown".to_owned());
            Ok(crate::bmc::feature::power::PowerStatusSnapshot {
                power_status,
                capabilities,
            })
        })
        .await;
    match result {
        Ok(snapshot) => {
            let fc_for_refresh = fc.clone();
            let expected_state = match params.action.as_str() {
                "on" => Some("on"),
                "off" | "graceful_shutdown" => Some("off"),
                _ => None,
            };
            tokio::spawn(async move {
                let _guard = guard;
                for delay_ms in [500, 500, 1000, 1000, 1000] {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    if fc_for_refresh.is_stopped() {
                        break;
                    }
                    let Some(current_backend) = get_backend(&fc_for_refresh).await else {
                        break;
                    };
                    if !Arc::ptr_eq(&backend, &current_backend) {
                        break;
                    }
                    let capabilities = backend.capabilities().await;
                    let refreshed = runtime
                        .status
                        .refresh_at(&fc_for_refresh, backend_generation, || async {
                            let power_status = backend.status().await?;
                            Ok(crate::bmc::feature::power::PowerStatusSnapshot {
                                power_status,
                                capabilities,
                            })
                        })
                        .await;
                    if refreshed
                        .ok()
                        .and_then(|snapshot| snapshot.value)
                        .is_some_and(|value| expected_state == Some(value.power_status.as_str()))
                    {
                        break;
                    }
                }
            });
            Json(serde_json::json!({
                "ok": true,
                "success": true,
                "mutation_id": params.mutation_id,
                "outcome": "accepted",
                "power_status": snapshot.value.as_ref().map(|value| value.power_status.as_str()).unwrap_or("unknown"),
                "revision": snapshot.revision,
            }))
            .into_response()
        }
        Err(_) => {
            record_fault_response(
                &fc,
                StatusCode::BAD_GATEWAY,
                FeatureFault::operation(
                    FeatureFaultDomain::Operation,
                    "control_failed",
                    "The power action result is unknown; inspect device state before retrying",
                    false,
                    "control",
                ),
            )
            .await
        }
    }
    })).await
}

pub async fn power_wol_send(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<WolParams>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        let Some(dev) = state.registry.get_dev(&bmc_id).await else {
            return device_not_found("wake");
        };
        let Some(fc) = dev.features.get("power").cloned() else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "power");
        };
        let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
            Ok(guard) => guard,
            Err(error) => {
                return acquisition_fault_response(&fc, feature_connect_status(&error), &error)
                    .await
            }
        };
        let backend_generation = fc.backend_generation();
        let Some(backend) = get_backend(&fc).await else {
            return backend_unavailable(&fc, "wake").await;
        };
        let Ok(_backend_effect) = fc.backend_effect_guard(backend_generation).await else {
            return backend_unavailable(&fc, "wake").await;
        };
        let result = backend.wake(&params.mac_address).await;
        drop(guard);
        match result {
            Ok(()) => Json(serde_json::json!({"success": true})).into_response(),
            Err(_) => {
                record_fault_response(
                    &fc,
                    StatusCode::BAD_GATEWAY,
                    FeatureFault::operation(
                        FeatureFaultDomain::Operation,
                        "wake_failed",
                        "The power backend could not send the wake request",
                        true,
                        "wake",
                    ),
                )
                .await
            }
        }
    }))
    .await
}

pub async fn power_wol_history() -> Json<serde_json::Value> {
    Json(serde_json::json!({"history": []}))
}

pub async fn power_retry(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    retry_feature(&state, &bmc_id, "power").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmc::feature::config::{FeatureConfig, FeatureConfigs};
    use crate::bmc::feature::connection::{ConnectionState, FeatureConnection};
    use crate::bmc::feature::device_registry::DeviceRegistry;
    use crate::bmc::feature::managed::ManagedDevice;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn failed_power_read_preserves_revision_and_exposes_fault_until_recovery() {
        struct ToggleRead(std::sync::atomic::AtomicBool);
        #[async_trait::async_trait]
        impl PowerBackend for ToggleRead {
            async fn capabilities(&self) -> super::super::PowerCapabilities {
                Default::default()
            }
            async fn status(&self) -> Result<String, String> {
                if self.0.load(Ordering::SeqCst) {
                    Err("remote unavailable".into())
                } else {
                    Ok("on".into())
                }
            }
            async fn control(&self, _: &str) -> Result<(), String> {
                Err("unsupported".into())
            }
            async fn wake(&self, _: &str) -> Result<(), String> {
                Err("unsupported".into())
            }
        }
        let backend = Arc::new(ToggleRead(std::sync::atomic::AtomicBool::new(false)));
        let fc = FeatureConnection::new(
            "device".into(),
            "power".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        fc.lifecycle_started.store(true, Ordering::SeqCst);
        fc.set_backend_runtime(backend.clone() as Arc<dyn PowerBackend>)
            .await;
        fc.transition(ConnectionState::Connected).await;
        let registry = DeviceRegistry::new();
        registry
            .insert(
                "device".into(),
                Arc::new(ManagedDevice::new(
                    "device".into(),
                    FeatureConfigs {
                        features: Default::default(),
                    },
                    [("power".into(), fc)].into(),
                )),
            )
            .await;
        let config = Arc::new(tokio::sync::RwLock::new(serde_json::from_value::<crate::config::Config>(
            serde_json::json!({"server":{"listen":"127.0.0.1:0","frontend_dir":"frontend/dist"},"devices":[]})
        ).unwrap()));
        let state = Arc::new(AppState {
            registry,
            config: config.clone(),
            config_path: String::new(),
            configuration: Default::default(),
            msd_storage: crate::bmc::msd_storage::FsMsdStorage::new(
                ".local/quality/power-status-test".into(),
                config,
            ),
            active_downloads: Default::default(),
            webtransport: Default::default(),
        });
        let Json(first) = power_status(Path("device".into()), State(state.clone())).await;
        assert_eq!(first["power_status"], "on");
        assert_eq!(first["resource"]["revision"], 1);
        backend.0.store(true, Ordering::SeqCst);
        let Json(failed) = power_status(Path("device".into()), State(state.clone())).await;
        assert_eq!(
            failed["resource"]["revision"], first["resource"]["revision"],
            "a failure observation must not be discarded as an older resource response"
        );
        assert_eq!(failed["resource"]["stale"], true);
        assert_eq!(failed["power_status"], "on");
        assert_eq!(failed["fault"]["code"], "status_failed");
        backend.0.store(false, Ordering::SeqCst);
        let Json(recovered) = power_status(Path("device".into()), State(state)).await;
        assert_eq!(recovered["resource"]["stale"], false);
        assert_eq!(
            recovered["resource"]["revision"],
            first["resource"]["revision"]
        );
        assert!(recovered["fault"].is_null());
    }

    #[derive(Default)]
    struct PendingWake {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    #[async_trait::async_trait]
    impl PowerBackend for PendingWake {
        async fn capabilities(&self) -> super::super::PowerCapabilities {
            Default::default()
        }
        async fn status(&self) -> Result<String, String> {
            Ok("unknown".into())
        }
        async fn control(&self, _: &str) -> Result<(), String> {
            PowerBackend::wake(self, "").await
        }
        async fn wake(&self, _: &str) -> Result<(), String> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl bmc_feature_api::identify::IdentifyBackend for PendingWake {
        async fn capabilities(&self) -> bmc_feature_api::identify::IdentifyCapabilities {
            bmc_feature_api::identify::IdentifyCapabilities {
                modes: vec![bmc_feature_api::identify::IdentifyMode::Off],
                max_temporary_seconds: 0,
                writable: true,
            }
        }
        async fn state(&self) -> Result<bmc_feature_api::identify::IdentifyState, String> {
            Ok(bmc_feature_api::identify::IdentifyState {
                mode: bmc_feature_api::identify::IdentifyMode::Off,
            })
        }
        async fn set(
            &self,
            _: bmc_feature_api::identify::IdentifyMode,
            _: Option<u8>,
        ) -> Result<(), String> {
            PowerBackend::wake(self, "").await
        }
    }

    async fn assert_replacement_waits(operation: &'static str) {
        let feature = if operation == "identify" {
            "identify"
        } else {
            "power"
        };
        let fc = FeatureConnection::new(
            "device".into(),
            feature.into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        fc.lifecycle_started.store(true, Ordering::SeqCst);
        fc.transition(ConnectionState::Connected).await;
        let backend = Arc::new(PendingWake::default());
        if operation == "identify" {
            fc.set_backend_runtime(
                backend.clone() as Arc<dyn bmc_feature_api::identify::IdentifyBackend>
            )
            .await;
        } else {
            fc.set_backend_runtime(backend.clone() as Arc<dyn PowerBackend>)
                .await;
        }
        let registry = DeviceRegistry::new();
        registry
            .insert(
                "device".into(),
                Arc::new(ManagedDevice::new(
                    "device".into(),
                    FeatureConfigs {
                        features: Default::default(),
                    },
                    [(feature.into(), fc.clone())].into(),
                )),
            )
            .await;
        let config = Arc::new(tokio::sync::RwLock::new(serde_json::from_value::<crate::config::Config>(
            serde_json::json!({"server":{"listen":"127.0.0.1:0","frontend_dir":"frontend/dist"},"devices":[]})
        ).unwrap()));
        let state = Arc::new(AppState {
            registry,
            config: config.clone(),
            config_path: String::new(),
            configuration: Default::default(),
            msd_storage: crate::bmc::msd_storage::FsMsdStorage::new(
                ".local/quality/wake-test".into(),
                config,
            ),
            active_downloads: Default::default(),
            webtransport: Default::default(),
        });
        let request = tokio::spawn(async move {
            match operation {
                "wake" => {
                    power_wol_send(
                        Path("device".into()),
                        State(state),
                        Json(WolParams {
                            mac_address: "00:11:22:33:44:55".into(),
                        }),
                    )
                    .await
                }
                "control" => {
                    power_control(
                        Path("device".into()),
                        State(state),
                        Json(PowerActionParams {
                            action: "on".into(),
                            mutation_id: None,
                        }),
                    )
                    .await
                }
                "identify" => {
                    crate::bmc::feature::identify::handler::set(
                        Path("device".into()),
                        State(state),
                        Json(serde_json::from_value(serde_json::json!({"mode":"off"})).unwrap()),
                    )
                    .await
                }
                _ => unreachable!(),
            }
        });
        tokio::time::timeout(Duration::from_secs(5), backend.started.notified())
            .await
            .unwrap();
        let replacement =
            fc.set_backend_runtime(Arc::new(PendingWake::default()) as Arc<dyn PowerBackend>);
        tokio::pin!(replacement);
        let replacement_waited = futures::poll!(&mut replacement).is_pending();
        backend.release.notify_one();
        let response = request.await.unwrap();
        if replacement_waited {
            replacement.await;
        }
        assert!(
            replacement_waited,
            "backend replacement overtook a pending {operation} effect"
        );
        assert_eq!(response.status(), StatusCode::OK);
    }
    #[tokio::test]
    async fn backend_replacement_waits_for_an_admitted_wake() {
        assert_replacement_waits("wake").await;
    }
    #[tokio::test]
    async fn backend_replacement_waits_for_an_admitted_power_control() {
        assert_replacement_waits("control").await;
    }
    #[tokio::test]
    async fn backend_replacement_waits_for_an_admitted_identify() {
        assert_replacement_waits("identify").await;
    }
}
