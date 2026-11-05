use super::HealthBackend;
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
pub async fn status(Path(id): Path<String>, State(app): State<Arc<AppState>>) -> Response {
    let Some(dev) = app.registry.get_dev(&id).await else {
        return unavailable_fault_response(StatusCode::NOT_FOUND, "health");
    };
    let Some(fc) = dev.features.get("health").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "health");
    };
    let g = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(g) => g,
        Err(e) => return acquisition_fault_response(&fc, feature_connect_status(&e), &e).await,
    };
    let _g = g;
    let backend_generation = fc.backend_generation();
    let Some(b) = fc.backend_runtime::<Arc<dyn HealthBackend>>().await else {
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
    let rt = super::super::health::feature_runtime(&fc).await;
    match rt
        .status
        .refresh_at(
            &fc,
            backend_generation,
            || async move { b.snapshot().await },
        )
        .await
    {
        Ok(s) => {
            fc.faults.clear_operation("status").await;
            let mut v = build_feature_status(&fc).await;
            if let Some(m) = v.as_object_mut() {
                m.insert("health".into(), serde_json::json!(s.value));
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
                    "health_read_failed",
                    "The health provider could not read its state",
                    true,
                    "status",
                ),
            )
            .await
        }
    }
}
pub async fn retry(Path(id): Path<String>, State(app): State<Arc<AppState>>) -> Response {
    retry_feature(&app, &id, "health").await
}
