use std::sync::Arc;

use bmc_backend_redfish::{RedfishConfig, RedfishMsdBackend};
use futures::future::BoxFuture;
use tracing::info;

use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::msd::MsdBackend;

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let params = &fc.config.backend_params;
        let backend = Arc::new(RedfishMsdBackend::new(RedfishConfig {
            host: params.host.clone(),
            port: params.port.unwrap_or(443),
            username: params.username.clone().unwrap_or_else(|| "admin".into()),
            password: params.password.clone().unwrap_or_default(),
            insecure_tls: params.redfish_insecure_tls.unwrap_or(false),
        })?);
        backend.verify_capability().await?;
        fc.set_backend_runtime(backend as Arc<dyn MsdBackend>).await;
        info!("[{}] Redfish MSD connected", fc.device_id);
        Ok(())
    })
}
