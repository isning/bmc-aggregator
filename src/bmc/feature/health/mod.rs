use crate::web::state::AppState;
use axum::{
    routing::{get, post},
    Router,
};
use std::sync::Arc;
pub mod handler;
pub use bmc_feature_api::health::{HealthBackend, HealthSnapshot, SensorRecord};
pub fn configuration_descriptor() -> bmc_feature_api::configuration::FeatureConfigDescriptor {
    crate::bmc::configuration::feature_descriptor(
        "health",
        bmc_feature_api::configuration::FeatureBindingKind::Single,
        true,
        Vec::new(),
    )
}
pub struct HealthFeatureRuntime {
    pub status: super::data::FeatureResource<HealthSnapshot>,
}
impl Default for HealthFeatureRuntime {
    fn default() -> Self {
        Self {
            status: super::data::FeatureResource::new("health"),
        }
    }
}
pub async fn feature_runtime(
    fc: &Arc<super::connection::FeatureConnection>,
) -> Arc<HealthFeatureRuntime> {
    fc.get_or_init_feature_runtime(|| Arc::new(HealthFeatureRuntime::default()))
        .await
}
pub fn mount(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handler::status))
        .route(&format!("{base}/retry"), post(handler::retry))
}
