pub mod config;
pub mod handler;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use serde::Serialize;

use crate::web::state::AppState;

pub use bmc_feature_api::power::{PowerAction, PowerBackend, PowerCapabilities};

pub fn configuration_descriptor() -> bmc_feature_api::configuration::FeatureConfigDescriptor {
    crate::bmc::configuration::feature_descriptor(
        "power",
        bmc_feature_api::configuration::FeatureBindingKind::Single,
        true,
        Vec::new(),
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PowerStatusSnapshot {
    pub power_status: String,
    pub capabilities: PowerCapabilities,
}

pub struct PowerFeatureRuntime {
    pub status: Arc<super::data::FeatureResource<PowerStatusSnapshot>>,
}

impl Default for PowerFeatureRuntime {
    fn default() -> Self {
        Self {
            status: Arc::new(super::data::FeatureResource::new("status")),
        }
    }
}

pub async fn feature_runtime(
    fc: &Arc<super::connection::FeatureConnection>,
) -> Arc<PowerFeatureRuntime> {
    fc.get_or_init_feature_runtime(|| Arc::new(PowerFeatureRuntime::default()))
        .await
}

pub fn mount(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handler::power_status))
        .route(
            &format!("{base}/devices/atx"),
            get(handler::power_atx_devices),
        )
        .route(&format!("{base}/control"), post(handler::power_control))
        .route(&format!("{base}/retry"), post(handler::power_retry))
        .route(
            &format!("{base}/config"),
            get(config::config_power).patch(config::config_power_update),
        )
        .route(&format!("{base}/wol"), post(handler::power_wol_send))
        .route(
            &format!("{base}/wol/history"),
            get(handler::power_wol_history),
        )
}
