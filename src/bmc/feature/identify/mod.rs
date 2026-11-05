use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use serde::Serialize;

use crate::web::state::AppState;

pub mod handler;

pub use bmc_feature_api::identify::{
    IdentifyBackend, IdentifyCapabilities, IdentifyMode, IdentifyState,
};

pub fn configuration_descriptor() -> bmc_feature_api::configuration::FeatureConfigDescriptor {
    crate::bmc::configuration::feature_descriptor(
        "identify",
        bmc_feature_api::configuration::FeatureBindingKind::Single,
        true,
        Vec::new(),
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IdentifyStatusSnapshot {
    pub capabilities: IdentifyCapabilities,
    pub identify: IdentifyState,
}

pub struct IdentifyFeatureRuntime {
    pub status: super::data::FeatureResource<IdentifyStatusSnapshot>,
}

impl Default for IdentifyFeatureRuntime {
    fn default() -> Self {
        Self {
            status: super::data::FeatureResource::new("status"),
        }
    }
}

pub async fn feature_runtime(
    fc: &Arc<super::connection::FeatureConnection>,
) -> Arc<IdentifyFeatureRuntime> {
    fc.get_or_init_feature_runtime(|| Arc::new(IdentifyFeatureRuntime::default()))
        .await
}

pub fn mount(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handler::status))
        .route(&format!("{base}/set"), post(handler::set))
        .route(&format!("{base}/retry"), post(handler::retry))
}
