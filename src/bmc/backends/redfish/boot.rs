use std::sync::Arc;

use bmc_backend_redfish::{RedfishBootBackend, RedfishConfig};
use bmc_feature_api::boot::BootDiscoveryStore;
use futures::future::BoxFuture;

use crate::bmc::feature::boot::provider::BootProviderRegistry;
use crate::bmc::feature::boot::{self, BootBackend, BootStatePublisher};
use crate::bmc::feature::connection::FeatureConnection;

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let runtime = boot::feature_runtime(&fc).await;
        let params = &fc.config.backend_params;
        let child = runtime.provider_runtime("redfish").await;
        let publisher = BootStatePublisher::new(child.clone(), &fc, "redfish");
        let discovery: Arc<dyn BootDiscoveryStore> = publisher.clone();
        let backend = Arc::new(RedfishBootBackend::new(
            RedfishConfig {
                host: params.host.clone(),
                port: params.port.unwrap_or(443),
                username: params.username.clone().unwrap_or_else(|| "admin".into()),
                password: params.password.clone().unwrap_or_default(),
                insecure_tls: params.redfish_insecure_tls.unwrap_or(false),
            },
            discovery,
        )?);
        let registry = Arc::new(
            BootProviderRegistry::new()
                .register_snapshot_provider("redfish", backend.clone() as Arc<dyn BootBackend>)
                .register_discovery_provider("redfish", backend.clone() as Arc<dyn BootBackend>),
        );
        fc.set_backend_runtime(registry.clone()).await;
        publisher.publish_current().await;
        registry.initialize(&fc, &runtime).await;
        if let Some(lease) = runtime.try_begin_discovery() {
            tokio::spawn(async move {
                backend.discover().await;
                drop(lease);
            });
        }
        Ok(())
    })
}
