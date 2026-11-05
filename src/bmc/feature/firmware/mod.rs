use crate::web::state::AppState;
use axum::{
    routing::{get, post},
    Router,
};
use serde::Serialize;
use std::sync::Arc;
pub mod handler;
pub use bmc_feature_api::firmware::{FirmwareBackend, FirmwareRecord};
pub fn configuration_descriptor() -> bmc_feature_api::configuration::FeatureConfigDescriptor {
    crate::bmc::configuration::feature_descriptor(
        "firmware",
        bmc_feature_api::configuration::FeatureBindingKind::Single,
        true,
        Vec::new(),
    )
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FirmwareStatusSnapshot {
    pub inventory: Vec<FirmwareRecord>,
}
pub struct FirmwareFeatureRuntime {
    pub status: super::data::FeatureResource<FirmwareStatusSnapshot>,
}
impl Default for FirmwareFeatureRuntime {
    fn default() -> Self {
        Self {
            status: super::data::FeatureResource::new("inventory"),
        }
    }
}
pub async fn feature_runtime(
    fc: &Arc<super::connection::FeatureConnection>,
) -> Arc<FirmwareFeatureRuntime> {
    fc.get_or_init_feature_runtime(|| Arc::new(FirmwareFeatureRuntime::default()))
        .await
}
pub fn mount(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handler::status))
        .route(&format!("{base}/retry"), post(handler::retry))
}
