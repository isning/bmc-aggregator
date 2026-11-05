use std::sync::Arc;

use bmc_backend_redfish::{RedfishConfig, RedfishIdentifyBackend};
use bmc_feature_api::identify::IdentifyBackend;
use futures::future::BoxFuture;

use crate::bmc::feature::connection::FeatureConnection;

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let params = &fc.config.backend_params;
        let backend = RedfishIdentifyBackend::new(RedfishConfig {
            host: params.host.clone(),
            port: params.port.unwrap_or(443),
            username: params.username.clone().unwrap_or_else(|| "admin".into()),
            password: params.password.clone().unwrap_or_default(),
            insecure_tls: params.redfish_insecure_tls.unwrap_or(false),
        })
        .await?;
        fc.set_backend_runtime(Arc::new(backend) as Arc<dyn IdentifyBackend>)
            .await;
        Ok(())
    })
}
