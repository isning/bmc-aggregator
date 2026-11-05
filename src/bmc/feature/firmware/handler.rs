use super::{FirmwareBackend, FirmwareStatusSnapshot};
use crate::bmc::feature::fault::{
    acquisition_fault_response, record_fault_response, unavailable_fault_response, FeatureFault,
    FeatureFaultDomain,
};
use crate::bmc::feature::feature_connect_status;
use crate::{
    bmc::feature::{acquire_connected_feature, build_feature_status, retry_feature},
    web::state::AppState,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use std::{sync::Arc, time::Duration};

pub(crate) async fn publish_backend_inventory(
    fc: &Arc<crate::bmc::feature::connection::FeatureConnection>,
    backend_generation: u64,
    cause: &'static str,
) {
    let Some(backend) = fc.backend_runtime::<Arc<dyn FirmwareBackend>>().await else {
        return;
    };
    let runtime = super::feature_runtime(fc).await;
    let _ = runtime
        .status
        .update_at(fc, backend_generation, cause, None, || async move {
            Ok(FirmwareStatusSnapshot {
                inventory: backend.inventory().await?,
            })
        })
        .await;
}
pub async fn status(Path(id): Path<String>, State(app): State<Arc<AppState>>) -> Response {
    let Some(dev) = app.registry.get_dev(&id).await else {
        return unavailable_fault_response(StatusCode::NOT_FOUND, "firmware");
    };
    let Some(fc) = dev.features.get("firmware").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "firmware");
    };
    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(g) => g,
        Err(e) => return acquisition_fault_response(&fc, feature_connect_status(&e), &e).await,
    };
    let _guard = guard;
    let backend_generation = fc.backend_generation();
    let Some(b) = fc.backend_runtime::<Arc<dyn FirmwareBackend>>().await else {
        return record_fault_response(
            &fc,
            StatusCode::SERVICE_UNAVAILABLE,
            FeatureFault::operation(
                FeatureFaultDomain::Lifecycle,
                "backend_runtime_unavailable",
                "The feature backend runtime is not available",
                true,
                "status",
            ),
        )
        .await;
    };
    let rt = super::super::firmware::feature_runtime(&fc).await;
    match rt
        .status
        .refresh_at(&fc, backend_generation, || async move {
            Ok(FirmwareStatusSnapshot {
                inventory: b.inventory().await?,
            })
        })
        .await
    {
        Ok(s) => {
            fc.faults.clear_operation("status").await;
            let mut v = build_feature_status(&fc).await;
            if let Some(m) = v.as_object_mut() {
                m.insert(
                    "inventory".into(),
                    serde_json::json!(s.value.map(|value| value.inventory).unwrap_or_default()),
                );
                m.insert(
                    "resource".into(),
                    serde_json::json!({"epoch":fc.data_epoch,"revision":s.revision,"stale":false}),
                );
            }
            Json(v).into_response()
        }
        Err(_) => {
            record_fault_response(
                &fc,
                StatusCode::BAD_GATEWAY,
                FeatureFault::operation(
                    FeatureFaultDomain::Operation,
                    "firmware_read_failed",
                    "The firmware provider could not read its state",
                    true,
                    "status",
                ),
            )
            .await
        }
    }
}
pub async fn retry(Path(id): Path<String>, State(app): State<Arc<AppState>>) -> Response {
    retry_feature(&app, &id, "firmware").await
}
