use std::sync::Arc;

use bmc_backend_ipmi::{IpmiBootBackend, IpmiConfig};
use futures::future::BoxFuture;

use crate::bmc::feature::boot::provider::BootProviderRegistry;
use crate::bmc::feature::boot::{self, BootBackend};
use crate::bmc::feature::connection::FeatureConnection;

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let bp = &fc.config.backend_params;
        let runtime = boot::feature_runtime(&fc).await;
        let backend = Arc::new(IpmiBootBackend::new(IpmiConfig {
            host: bp.host.clone(),
            port: bp.port.unwrap_or(623),
            username: bp.username.clone().unwrap_or_else(|| "admin".into()),
            password: bp.password.clone().unwrap_or_default(),
        })) as Arc<dyn BootBackend>;
        let registry =
            Arc::new(BootProviderRegistry::new().register_snapshot_provider("ipmi", backend));
        fc.set_backend_runtime(registry.clone()).await;
        registry.initialize(&fc, &runtime).await;
        Ok(())
    })
}
