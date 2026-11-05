//! Application lifecycle adapter for the isolated Dell WS-MAN backend.

use std::sync::Arc;

use bmc_backend_idrac_wsman::IdracWsmanBootBackend;
use bmc_feature_api::boot::{BootDiscoverySnapshot, BootEntryDiscoveryState, BootStateStore};
use futures::future::BoxFuture;

use crate::bmc::feature::boot::provider::BootProviderRegistry;
use crate::bmc::feature::boot::{BootBackend, BootFeatureRuntime, BootStatePublisher};
use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::requirements::{
    BackendRequirement, RemediationAction, RemediationResult, RequirementId,
};

pub fn requirements(
    fc: Arc<FeatureConnection>,
) -> BoxFuture<'static, Result<Vec<BackendRequirement>, String>> {
    Box::pin(async move {
        fc.backend_runtime::<Arc<BootProviderRegistry>>()
            .await
            .ok_or_else(|| "iDRAC boot backend is not connected".to_string())?
            .requirements()
            .await
    })
}

pub fn remediate_requirement(
    fc: Arc<FeatureConnection>,
    requirement_id: RequirementId,
    action: RemediationAction,
) -> BoxFuture<'static, Result<RemediationResult, String>> {
    Box::pin(async move {
        fc.backend_runtime::<Arc<BootProviderRegistry>>()
            .await
            .ok_or_else(|| "iDRAC boot backend is not connected".to_string())?
            .remediate_requirement(requirement_id, action)
            .await
    })
}

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let params = &fc.config.backend_params;
        let generation = params
            .idrac_generation
            .ok_or_else(|| "idrac-wsman requires idrac_generation 6 or 8".to_string())?;
        let runtime = fc
            .get_or_init_feature_runtime(|| {
                BootFeatureRuntime::new(
                    BootDiscoverySnapshot {
                        entries: Vec::new(),
                        state: BootEntryDiscoveryState::Discovering,
                        selected_entry_id: None,
                    },
                    vec![IdracWsmanBootBackend::initial_requirement()],
                )
            })
            .await;
        let child = runtime.provider_runtime("idrac-wsman").await;
        if child.requirements().await.is_empty() {
            child
                .set_requirements(vec![IdracWsmanBootBackend::initial_requirement()])
                .await;
        }
        let publisher = BootStatePublisher::new(child.clone(), &fc, "idrac-wsman");
        let state: Arc<dyn BootStateStore> = publisher.clone();
        let backend = Arc::new(IdracWsmanBootBackend::new(
            params.host.clone(),
            params.username.clone().unwrap_or_else(|| "admin".into()),
            params.password.clone().unwrap_or_default(),
            generation,
            state,
        )?);
        let registry =
            Arc::new(BootProviderRegistry::new().register_discovery_provider(
                "idrac-wsman",
                backend.clone() as Arc<dyn BootBackend>,
            ));
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
