use std::sync::Arc;

use bmc_backend_ipmi::{IpmiConfig, IpmiIdentifyBackend};
use futures::future::BoxFuture;

use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::identify::IdentifyBackend;

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let bp = &fc.config.backend_params;
        let backend = Arc::new(IpmiIdentifyBackend::new(IpmiConfig {
            host: bp.host.clone(),
            port: bp.port.unwrap_or(623),
            username: bp.username.clone().unwrap_or_else(|| "admin".into()),
            password: bp.password.clone().unwrap_or_default(),
        })) as Arc<dyn IdentifyBackend>;
        fc.set_backend_runtime(backend).await;
        Ok(())
    })
}
