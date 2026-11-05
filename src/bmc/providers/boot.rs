use std::sync::Arc;

use bmc_backend_idrac_wsman::IdracWsmanBootBackend;
use bmc_backend_ipmi::{IpmiBootBackend, IpmiConfig};
use bmc_backend_redfish::{RedfishBootBackend, RedfishConfig};
use bmc_feature_api::boot::{
    BootBackend, BootDiscoverySnapshot, BootDiscoveryStore, BootEntryDiscoveryState, BootStateStore,
};
use futures::future::BoxFuture;

use crate::bmc::feature::boot::provider::BootProviderRegistry;
use crate::bmc::feature::boot::{BootFeatureRuntime, BootStatePublisher};
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
            .ok_or_else(|| "Boot providers are not connected".to_string())?
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
            .ok_or_else(|| "Boot providers are not connected".to_string())?
            .remediate_requirement(requirement_id, action)
            .await
    })
}

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let has_wsman = fc
            .config
            .providers
            .iter()
            .any(|provider| provider.backend == "idrac-wsman");
        let runtime = fc
            .get_or_init_feature_runtime(|| {
                BootFeatureRuntime::new(
                    BootDiscoverySnapshot {
                        entries: Vec::new(),
                        state: if has_wsman {
                            BootEntryDiscoveryState::Discovering
                        } else {
                            BootEntryDiscoveryState::Unsupported
                        },
                        selected_entry_id: None,
                    },
                    if has_wsman {
                        vec![IdracWsmanBootBackend::initial_requirement()]
                    } else {
                        Vec::new()
                    },
                )
            })
            .await;

        let mut registry = BootProviderRegistry::new();
        let mut discoveries: Vec<BoxFuture<'static, ()>> = Vec::new();
        let mut publishers = Vec::new();

        for provider in &fc.config.providers {
            let params = &provider.backend_params;
            let child = runtime.provider_runtime(&provider.id).await;
            match provider.backend.as_str() {
                "ipmi" => {
                    let backend = Arc::new(IpmiBootBackend::new(IpmiConfig {
                        host: params.host.clone(),
                        port: params.port.unwrap_or(623),
                        username: params.username.clone().unwrap_or_else(|| "admin".into()),
                        password: params.password.clone().unwrap_or_default(),
                    })) as Arc<dyn BootBackend>;
                    registry = registry.register_snapshot_provider(provider.id.clone(), backend);
                }
                "idrac-wsman" => {
                    if child.requirements().await.is_empty() {
                        child
                            .set_requirements(vec![IdracWsmanBootBackend::initial_requirement()])
                            .await;
                    }
                    let generation = params.idrac_generation.ok_or_else(|| {
                        "idrac-wsman provider requires idrac_generation 6 or 8".to_string()
                    })?;
                    let publisher =
                        BootStatePublisher::new(child.clone(), &fc, provider.id.clone());
                    let state: Arc<dyn BootStateStore> = publisher.clone();
                    let backend = Arc::new(IdracWsmanBootBackend::new(
                        params.host.clone(),
                        params.username.clone().unwrap_or_else(|| "admin".into()),
                        params.password.clone().unwrap_or_default(),
                        generation,
                        state,
                    )?);
                    registry = registry.register_discovery_provider(
                        provider.id.clone(),
                        backend.clone() as Arc<dyn BootBackend>,
                    );
                    discoveries.push(Box::pin(async move { backend.discover().await }));
                    publishers.push(publisher);
                }
                "redfish" => {
                    let publisher =
                        BootStatePublisher::new(child.clone(), &fc, provider.id.clone());
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
                    registry = registry
                        .register_snapshot_provider(
                            provider.id.clone(),
                            backend.clone() as Arc<dyn BootBackend>,
                        )
                        .register_discovery_provider(
                            provider.id.clone(),
                            backend.clone() as Arc<dyn BootBackend>,
                        );
                    discoveries.push(Box::pin(async move { backend.discover().await }));
                    publishers.push(publisher);
                }
                backend => return Err(format!("unsupported Boot provider '{backend}'")),
            }
        }

        let registry = Arc::new(registry);
        fc.set_backend_runtime(registry.clone()).await;
        for publisher in publishers {
            publisher.publish_current().await;
        }
        registry.initialize(&fc, &runtime).await;
        if !discoveries.is_empty() {
            if let Some(lease) = runtime.try_begin_discovery() {
                tokio::spawn(async move {
                    futures::future::join_all(discoveries).await;
                    drop(lease);
                });
            }
        }
        Ok(())
    })
}
