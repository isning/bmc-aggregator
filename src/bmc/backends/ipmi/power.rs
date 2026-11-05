use std::sync::Arc;

use bmc_backend_ipmi::{IpmiConfig, IpmiPowerBackend};
use futures::future::BoxFuture;
use tracing::info;

use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::power::PowerBackend as PowerBackendTrait;

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let bp = &fc.config.backend_params;
        if bp.host.is_empty() {
            return Err("IPMI host is empty".to_string());
        }
        let port = bp.port.unwrap_or(623);
        let username = bp.username.clone().unwrap_or_else(|| "admin".into());
        let password = bp.password.clone().unwrap_or_default();
        let backend = Arc::new(IpmiPowerBackend::new(IpmiConfig {
            host: bp.host.clone(),
            port,
            username,
            password,
        })) as Arc<dyn PowerBackendTrait>;
        fc.set_backend_runtime(backend).await;
        info!("[{}] ipmi power connected", fc.device_id);
        Ok(())
    })
}
