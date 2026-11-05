pub mod config;
pub mod handler;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::web::state::AppState;

pub fn configuration_descriptor() -> bmc_feature_api::configuration::FeatureConfigDescriptor {
    crate::bmc::configuration::feature_descriptor(
        "audio",
        bmc_feature_api::configuration::FeatureBindingKind::Single,
        true,
        Vec::new(),
    )
}

pub fn mount(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handler::audio_status))
        .route(&format!("{base}/start"), post(handler::audio_start))
        .route(&format!("{base}/stop"), post(handler::audio_stop))
        .route(&format!("{base}/retry"), post(handler::audio_retry))
        .route(&format!("{base}/quality"), post(handler::audio_set_quality))
        .route(
            &format!("{base}/device"),
            post(handler::audio_select_device),
        )
        .route(
            &format!("{base}/config"),
            get(config::config_audio).patch(config::config_audio_update),
        )
}
