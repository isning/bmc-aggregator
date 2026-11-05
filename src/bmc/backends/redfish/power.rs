use std::sync::Arc;

use bmc_backend_redfish::{RedfishConfig, RedfishPowerBackend};
use futures::future::BoxFuture;
use tracing::info;

use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::power::PowerBackend;

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let params = &fc.config.backend_params;
        let backend = Arc::new(RedfishPowerBackend::new(RedfishConfig {
            host: params.host.clone(),
            port: params.port.unwrap_or(443),
            username: params.username.clone().unwrap_or_else(|| "admin".into()),
            password: params.password.clone().unwrap_or_default(),
            insecure_tls: params.redfish_insecure_tls.unwrap_or(false),
        })?) as Arc<dyn PowerBackend>;
        fc.set_backend_runtime(backend).await;
        info!("[{}] Redfish power connected", fc.device_id);
        Ok(())
    })
}
