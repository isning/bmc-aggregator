use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::bmc::feature::fault::{fault_response, FeatureFault, FeatureFaultDomain};
use crate::bmc::feature::{acquire_connected_feature, feature_connect_status, registered_backends};
use crate::web::state::AppState;

const INSPECTION_TIMEOUT: Duration = Duration::from_secs(15);

pub use bmc_feature_api::requirements::{
    BackendRequirement, ConfirmationLevel, Remediation, RemediationAction, RemediationResult,
    RequirementCategory, RequirementId, RequirementImpact, RequirementState, RestartRequirement,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RequirementRecord {
    pub feature: String,
    pub backend: String,
    #[serde(flatten)]
    pub requirement: BackendRequirement,
}

#[derive(Deserialize)]
pub struct RemediateRequest {
    feature: String,
    requirement_id: RequirementId,
    action: RemediationAction,
}

fn advertised_remediation(
    requirements: &[BackendRequirement],
    requirement_id: RequirementId,
    action: RemediationAction,
) -> Option<&BackendRequirement> {
    requirements.iter().find(|requirement| {
        requirement.id == requirement_id
            && requirement.state == RequirementState::ActionRequired
            && requirement
                .remediation
                .as_ref()
                .is_some_and(|remediation| remediation.action == action)
    })
}

fn requirement_fault(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
    operation: &'static str,
) -> Response {
    fault_response(
        status,
        FeatureFault::operation(
            FeatureFaultDomain::Operation,
            code,
            message,
            retryable,
            operation,
        ),
    )
}

async fn acquire_provider(
    state: &Arc<AppState>,
    device_id: &str,
    feature_id: &str,
) -> Result<
    (
        crate::bmc::feature::connection::FeatureGuard,
        Arc<crate::bmc::feature::connection::FeatureConnection>,
        Arc<crate::bmc::feature::registry::BackendRequirementHooks>,
    ),
    Response,
> {
    let device = state.registry.get_dev(device_id).await.ok_or_else(|| {
        requirement_fault(
            StatusCode::NOT_FOUND,
            "device_not_found",
            "The requested BMC device was not found",
            false,
            "requirements",
        )
    })?;
    let fc = device.features.get(feature_id).cloned().ok_or_else(|| {
        requirement_fault(
            StatusCode::NOT_FOUND,
            "requirement_provider_not_found",
            "The requested requirement provider was not found",
            false,
            "requirements",
        )
    })?;
    let hooks = requirement_hooks(&fc).ok_or_else(|| {
        requirement_fault(
            StatusCode::NOT_FOUND,
            "requirement_provider_not_found",
            "The requested feature backend exposes no requirements",
            false,
            "requirements",
        )
    })?;
    let guard = acquire_connected_feature(fc.clone(), Duration::from_secs(15))
        .await
        .map_err(|error| {
            requirement_fault(
                feature_connect_status(&error),
                "requirement_provider_unavailable",
                "The requirement provider could not connect",
                true,
                "requirements",
            )
        })?;
    Ok((guard, fc, hooks))
}

fn requirement_hooks(
    fc: &Arc<crate::bmc::feature::connection::FeatureConnection>,
) -> Option<Arc<crate::bmc::feature::registry::BackendRequirementHooks>> {
    if fc.id == "boot" && !fc.config.providers.is_empty() {
        return Some(Arc::new(
            crate::bmc::feature::registry::BackendRequirementHooks {
                inspect: Arc::new(|fc| crate::bmc::providers::boot::requirements(fc)),
                remediate: Arc::new(|fc, id, action| {
                    crate::bmc::providers::boot::remediate_requirement(fc, id, action)
                }),
            },
        ));
    }
    registered_backends()
        .iter()
        .find(|template| template.backend == fc.backend && template.feature == fc.id)
        .and_then(|template| template.requirements.as_ref())
        .map(|hooks| {
            Arc::new(crate::bmc::feature::registry::BackendRequirementHooks {
                inspect: hooks.inspect.clone(),
                remediate: hooks.remediate.clone(),
            })
        })
}

pub async fn list(Path(device_id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    let Some(device) = state.registry.get_dev(&device_id).await else {
        return requirement_fault(
            StatusCode::NOT_FOUND,
            "device_not_found",
            "The requested BMC device was not found",
            false,
            "requirements",
        );
    };
    let mut feature_ids: Vec<_> = device.features.keys().cloned().collect();
    feature_ids.sort();
    let jobs: Vec<_> = feature_ids
        .into_iter()
        .filter_map(|feature_id| {
            let fc = device.features.get(&feature_id).cloned()?;
            let hooks = requirement_hooks(&fc)?;
            Some((feature_id, fc, hooks))
        })
        .collect();
    list_jobs(jobs, INSPECTION_TIMEOUT).await
}

type RequirementJob = (
    String,
    Arc<crate::bmc::feature::connection::FeatureConnection>,
    Arc<crate::bmc::feature::registry::BackendRequirementHooks>,
);

async fn inspect_acquired(
    fc: &Arc<crate::bmc::feature::connection::FeatureConnection>,
    hooks: &crate::bmc::feature::registry::BackendRequirementHooks,
    timeout: Duration,
) -> Result<Vec<BackendRequirement>, ()> {
    tokio::time::timeout(timeout, (hooks.inspect)(fc.clone()))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

async fn list_jobs(jobs: Vec<RequirementJob>, inspection_timeout: Duration) -> Response {
    let results = futures::stream::iter(jobs)
        .map(|(feature_id, fc, hooks)| async move {
            let Ok(_guard) = acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await
            else {
                return Err(());
            };
            match inspect_acquired(&fc, &hooks, inspection_timeout).await {
                Ok(requirements) => Ok(requirements
                    .into_iter()
                    .map(|requirement| RequirementRecord {
                        feature: feature_id.clone(),
                        backend: fc.backend.clone(),
                        requirement,
                    })
                    .collect::<Vec<_>>()),
                Err(()) => Err(()),
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    let complete = results.iter().all(Result::is_ok);
    let mut records: Vec<_> = results
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .collect();
    records.sort_by_key(|record| {
        let priority = match record.requirement.state {
            RequirementState::ActionRequired => 0,
            RequirementState::Pending => 1,
            RequirementState::Unavailable => 2,
            RequirementState::Unknown => 3,
            RequirementState::Satisfied => 4,
        };
        (priority, record.feature.clone(), record.requirement.id)
    });
    let count = |state| {
        records
            .iter()
            .filter(|record| record.requirement.state == state)
            .count()
    };
    let mut response = serde_json::json!({
        "complete": complete,
        "requirements": records,
        "summary": {
            "total": records.len(),
            "action_required": count(RequirementState::ActionRequired),
            "pending": count(RequirementState::Pending),
            "unavailable": count(RequirementState::Unavailable),
            "unknown": count(RequirementState::Unknown),
        }
    });
    if !complete {
        response["success"] = serde_json::json!(false);
        response["fault"] = serde_json::json!(FeatureFault::operation(
            FeatureFaultDomain::Operation,
            "requirements_inspection_failed",
            "Some device requirements could not be checked; retry before relying on this inventory",
            true,
            "requirements",
        ));
    }
    (
        if complete {
            StatusCode::OK
        } else {
            StatusCode::BAD_GATEWAY
        },
        Json(response),
    )
        .into_response()
}

pub async fn remediate(
    Path(device_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<RemediateRequest>,
) -> Response {
    let (_guard, fc, hooks) = match acquire_provider(&state, &device_id, &request.feature).await {
        Ok(provider) => provider,
        Err(response) => return response,
    };
    remediate_acquired(_guard, fc, hooks, request, INSPECTION_TIMEOUT).await
}

async fn remediate_acquired(
    guard: crate::bmc::feature::connection::FeatureGuard,
    fc: Arc<crate::bmc::feature::connection::FeatureConnection>,
    hooks: Arc<crate::bmc::feature::registry::BackendRequirementHooks>,
    request: RemediateRequest,
    inspection_timeout: Duration,
) -> Response {
    let backend_generation = fc.backend_generation();
    crate::web::state::mutation_response(tokio::spawn(async move {
    let _guard = guard;
    let Ok(_backend_operation) = fc.backend_operation_guard(backend_generation).await else {
        return requirement_fault(
            StatusCode::SERVICE_UNAVAILABLE,
            "requirement_provider_unavailable",
            "The requirement provider is no longer available; no remediation was started",
            true,
            "remediate_requirement",
        );
    };
    let advertised = match inspect_acquired(&fc, &hooks, inspection_timeout).await {
        Ok(requirements) => {
            advertised_remediation(&requirements, request.requirement_id, request.action).is_some()
        }
        Err(()) => return requirement_fault(
            StatusCode::BAD_GATEWAY,
            "requirements_inspection_failed",
            "The device requirements could not be checked; no remediation was started",
            true,
            "remediate_requirement",
        ),
    };
    if !advertised {
        return requirement_fault(
            StatusCode::CONFLICT,
            "requirement_remediation_not_available",
            "The requested remediation is not currently available",
            false,
            "remediate_requirement",
        );
    }
    if fc.is_stopped() {
        return requirement_fault(
            StatusCode::SERVICE_UNAVAILABLE,
            "requirement_provider_unavailable",
            "The requirement provider is no longer available; no remediation was started",
            true,
            "remediate_requirement",
        );
    }
    match (hooks.remediate)(fc.clone(), request.requirement_id, request.action).await {
        Ok(result) => Json(serde_json::json!({
            "success": true,
            "result": result,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(%error, "backend requirement remediation failed");
            requirement_fault(
                StatusCode::BAD_GATEWAY,
                "requirement_remediation_failed",
                "The remediation result could not be confirmed; inspect device state before retrying",
                false,
                "remediate_requirement",
            )
        }
    }
    })).await
}

pub fn mount(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(base, get(list))
        .route(&format!("{base}/remediate"), post(remediate))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn inspection_job(mode: u8) -> RequirementJob {
        let fc = crate::bmc::feature::connection::FeatureConnection::new(
            "device".into(),
            format!("provider-{mode}"),
            "fixture".into(),
            crate::bmc::feature::config::FeatureConfig {
                backend: "fixture".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        fc.lifecycle_started
            .store(mode != 4, std::sync::atomic::Ordering::SeqCst);
        fc.transition(crate::bmc::feature::connection::ConnectionState::Connected)
            .await;
        let hooks = Arc::new(crate::bmc::feature::registry::BackendRequirementHooks {
            inspect: Arc::new(move |_| {
                Box::pin(async move {
                    match mode {
                        0 => Ok(vec![]),
                        1 => Ok(vec![inventory_requirement(
                            RequirementState::ActionRequired,
                        )]),
                        3 => std::future::pending().await,
                        _ => Err("private-provider-detail".into()),
                    }
                })
            }),
            remediate: Arc::new(|_, _, _| Box::pin(async { Err("unused".into()) })),
        });
        (fc.id.clone(), fc, hooks)
    }

    #[tokio::test]
    async fn incomplete_inspections_fail_without_discarding_successful_results_or_leases() {
        for mode in [2, 3, 4] {
            let failed = inspection_job(mode).await;
            let fc = failed.1.clone();
            let response = list_jobs(
                vec![inspection_job(1).await, failed],
                Duration::from_millis(1),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let bytes = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("private-provider-detail"));
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["complete"], false);
            assert_eq!(value["success"], false);
            assert_eq!(value["fault"]["code"], "requirements_inspection_failed");
            assert_eq!(value["summary"]["total"], 1);
            assert_eq!(value["summary"]["action_required"], 1);
            assert_eq!(value["requirements"].as_array().unwrap().len(), 1);
            assert_eq!(fc.ref_count(), 0);
        }
    }

    #[tokio::test]
    async fn empty_inspections_are_success_only_when_all_registered_providers_were_checked() {
        for jobs in [vec![], vec![inspection_job(0).await]] {
            let response = list_jobs(jobs, Duration::from_millis(1)).await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["complete"], true);
            assert_eq!(value["summary"]["total"], 0);
            assert!(value.get("fault").is_none());
        }
    }

    #[tokio::test]
    async fn remediation_preflight_errors_and_timeouts_never_dispatch_a_mutation() {
        use crate::bmc::feature::connection::FeatureGuard;
        for mode in [0, 2, 3] {
            let (_, fc, original) = inspection_job(mode).await;
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let hooks = Arc::new(crate::bmc::feature::registry::BackendRequirementHooks {
                inspect: original.inspect.clone(),
                remediate: Arc::new({
                    let calls = calls.clone();
                    move |_, _, _| {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Box::pin(async { Err("must not run".into()) })
                    }
                }),
            });
            let response = remediate_acquired(
                FeatureGuard::new(fc.clone()),
                fc.clone(),
                hooks,
                RemediateRequest {
                    feature: fc.id.clone(),
                    requirement_id: RequirementId::FirmwareInventoryCollection,
                    action: RemediationAction::EnableInventoryCollection,
                },
                Duration::from_millis(1),
            )
            .await;
            assert_eq!(
                response.status(),
                if mode == 0 {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::BAD_GATEWAY
                }
            );
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("private-provider-detail"));
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                value["fault"]["code"],
                if mode == 0 {
                    "requirement_remediation_not_available"
                } else {
                    "requirements_inspection_failed"
                }
            );
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(fc.ref_count(), 0);
        }
    }

    fn inventory_requirement(state: RequirementState) -> BackendRequirement {
        BackendRequirement {
            id: RequirementId::FirmwareInventoryCollection,
            category: RequirementCategory::Inventory,
            state,
            impact: RequirementImpact::Degraded,
            title_key: "requirements.firmwareInventory.title".to_string(),
            message_key: "requirements.firmwareInventory.actionRequired".to_string(),
            remediation: Some(Remediation {
                action: RemediationAction::EnableInventoryCollection,
                confirmation: ConfirmationLevel::Required,
                restart_requirement: RestartRequirement::NextHostPost,
            }),
        }
    }

    #[tokio::test]
    async fn remediation_rejects_stop_during_preflight_and_pins_backend_until_dispatch() {
        use crate::bmc::feature::connection::FeatureGuard;
        for stop in [true, false] {
            let (_, fc, _) = inspection_job(1).await;
            fc.set_backend_runtime(1u8).await;
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let hooks = Arc::new(crate::bmc::feature::registry::BackendRequirementHooks {
                inspect: Arc::new({
                    let entered = entered.clone();
                    let release = release.clone();
                    move |fc| {
                        let entered = entered.clone();
                        let release = release.clone();
                        Box::pin(async move {
                            assert_eq!(fc.backend_runtime::<u8>().await, Some(1));
                            entered.notify_one();
                            release.notified().await;
                            Ok(vec![inventory_requirement(
                                RequirementState::ActionRequired,
                            )])
                        })
                    }
                }),
                remediate: Arc::new({
                    let calls = calls.clone();
                    move |fc, _, _| {
                        let calls = calls.clone();
                        Box::pin(async move {
                            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            assert_eq!(fc.backend_runtime::<u8>().await, Some(1));
                            Err("confirmation unavailable".into())
                        })
                    }
                }),
            });
            let response = tokio::spawn(remediate_acquired(
                FeatureGuard::new(fc.clone()),
                fc.clone(),
                hooks,
                RemediateRequest {
                    feature: fc.id.clone(),
                    requirement_id: RequirementId::FirmwareInventoryCollection,
                    action: RemediationAction::EnableInventoryCollection,
                },
                INSPECTION_TIMEOUT,
            ));
            entered.notified().await;
            let replacement = fc.clear_backend_runtime();
            tokio::pin!(replacement);
            if stop {
                fc.shutdown();
            } else {
                assert!(futures::poll!(&mut replacement).is_pending());
            }
            release.notify_one();
            let response = tokio::time::timeout(Duration::from_secs(1), response)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                response.status(),
                if stop {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::BAD_GATEWAY
                }
            );
            if !stop {
                replacement.await;
            }
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                usize::from(!stop)
            );
            assert_eq!(fc.ref_count(), 0);
        }
    }

    #[tokio::test]
    async fn cancelled_remediation_keeps_its_lease_until_remote_confirmation() {
        use crate::bmc::feature::{
            config::FeatureConfig,
            connection::{FeatureConnection, FeatureGuard},
            registry::BackendRequirementHooks,
        };
        let fc = FeatureConnection::new(
            "device".into(),
            "boot".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started = Arc::new(tokio::sync::Mutex::new(Some(started_tx)));
        let release = Arc::new(tokio::sync::Notify::new());
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hooks = Arc::new(BackendRequirementHooks {
            inspect: Arc::new(|_| {
                Box::pin(async {
                    Ok(vec![inventory_requirement(
                        RequirementState::ActionRequired,
                    )])
                })
            }),
            remediate: Arc::new({
                let done = done.clone();
                let release = release.clone();
                move |_, _, _| {
                    let started = started.clone();
                    let done = done.clone();
                    let release = release.clone();
                    Box::pin(async move {
                        started.lock().await.take().unwrap().send(()).unwrap();
                        release.notified().await;
                        done.store(true, std::sync::atomic::Ordering::SeqCst);
                        Err("confirmation unavailable".into())
                    })
                }
            }),
        });
        let guard = FeatureGuard::new(fc.clone());
        let waiter = tokio::spawn(remediate_acquired(
            guard,
            fc.clone(),
            hooks,
            RemediateRequest {
                feature: "boot".into(),
                requirement_id: RequirementId::FirmwareInventoryCollection,
                action: RemediationAction::EnableInventoryCollection,
            },
            INSPECTION_TIMEOUT,
        ));
        started_rx.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert_eq!(fc.ref_count(), 1);
        let replacement = fc.clear_backend_runtime();
        tokio::pin!(replacement);
        assert!(futures::poll!(&mut replacement).is_pending());
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), async {
            while fc.ref_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        replacement.await;
        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn public_requirement_json_contains_only_generic_typed_fields() {
        let record = RequirementRecord {
            feature: "boot".to_string(),
            backend: "idrac-wsman".to_string(),
            requirement: inventory_requirement(RequirementState::ActionRequired),
        };
        let json = serde_json::to_value(record).expect("serialize requirement");

        assert_eq!(json["id"], "firmware_inventory_collection");
        assert_eq!(json["category"], "inventory");
        assert_eq!(json["remediation"]["action"], "enable_inventory_collection");
        for vendor_field in [
            "resource_uri",
            "instance_id",
            "attribute_name",
            "attribute_value",
            "return_value",
            "reboot_job_type",
        ] {
            assert!(json.get(vendor_field).is_none(), "leaked {vendor_field}");
        }
    }

    #[test]
    fn remediation_requires_a_current_action_required_advertisement() {
        let action = RemediationAction::EnableInventoryCollection;
        let id = RequirementId::FirmwareInventoryCollection;
        let actionable = inventory_requirement(RequirementState::ActionRequired);
        assert!(advertised_remediation(&[actionable], id, action).is_some());

        for state in [
            RequirementState::Satisfied,
            RequirementState::Pending,
            RequirementState::Unavailable,
            RequirementState::Unknown,
        ] {
            let requirement = inventory_requirement(state);
            assert!(advertised_remediation(&[requirement], id, action).is_none());
        }

        let mut without_action = inventory_requirement(RequirementState::ActionRequired);
        without_action.remediation = None;
        assert!(advertised_remediation(&[without_action], id, action).is_none());
    }
}
