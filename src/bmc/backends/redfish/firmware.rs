use crate::bmc::feature::connection::FeatureConnection;
use bmc_backend_redfish::{RedfishConfig, RedfishFirmwareBackend};
use bmc_feature_api::firmware::FirmwareBackend;
use futures::future::BoxFuture;
use std::sync::Arc;
pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let p = &fc.config.backend_params;
        let b: Arc<dyn FirmwareBackend> = Arc::new(RedfishFirmwareBackend::new(RedfishConfig {
            host: p.host.clone(),
            port: p.port.unwrap_or(443),
            username: p.username.clone().unwrap_or_else(|| "admin".into()),
            password: p.password.clone().unwrap_or_default(),
            insecure_tls: p.redfish_insecure_tls.unwrap_or(false),
        })?);
        fc.set_backend_runtime(b).await;
        Ok(())
    })
}
