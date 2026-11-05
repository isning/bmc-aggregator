use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use crate::bmc::feature::requirements::BackendRequirement;
use crate::web::state::AppState;
use async_trait::async_trait;
use axum::routing::{get, post};
use axum::Router;

pub mod handler;
pub mod provider;

pub use bmc_feature_api::boot::{
    BootBackend, BootCapabilities, BootDiscoverySnapshot, BootDiscoveryStore, BootEntry,
    BootEntryCatalog, BootEntryDiscoveryState, BootEntrySelection, BootMode, BootOptionEntry,
    BootOptionProviderStatus, BootOptionSelection, BootOptionsSnapshot, BootProviderFault,
    BootProviderPhase, BootProviderSnapshot, BootSelection, BootStateStore, BootStatus,
};

pub fn configuration_descriptor() -> bmc_feature_api::configuration::FeatureConfigDescriptor {
    crate::bmc::configuration::feature_descriptor(
        "boot",
        bmc_feature_api::configuration::FeatureBindingKind::ProviderSet { min: 1, max: None },
        true,
        Vec::new(),
    )
}

/// Feature-owned Boot state. Discovery results and requirements remain stable
/// while the protocol adapter idles out and reconnects.
pub struct BootFeatureRuntime {
    provider_states: tokio::sync::RwLock<Vec<(String, Arc<BootFeatureRuntime>)>>,
    parent: Option<Weak<BootFeatureRuntime>>,
    publish_gate: tokio::sync::Mutex<()>,
    discovery: tokio::sync::RwLock<BootDiscoverySnapshot>,
    requirements: tokio::sync::RwLock<Vec<BackendRequirement>>,
    discovery_active: AtomicBool,
    pub operation: tokio::sync::Mutex<()>,
    pub snapshot_entries: super::data::FeatureResource<BootProviderSnapshot<BootEntryCatalog>>,
    pub snapshot_selection: super::data::FeatureResource<BootProviderSnapshot<BootEntrySelection>>,
    pub discovery_entries: super::data::FeatureResource<BootProviderSnapshot<BootEntryCatalog>>,
    pub discovery_selection: super::data::FeatureResource<BootProviderSnapshot<BootEntrySelection>>,
    pub options: super::data::FeatureResource<BootOptionsSnapshot>,
}

impl BootFeatureRuntime {
    pub fn new(
        discovery: BootDiscoverySnapshot,
        requirements: Vec<BackendRequirement>,
    ) -> Arc<Self> {
        Self::new_inner(discovery, requirements, None)
    }

    fn new_inner(
        discovery: BootDiscoverySnapshot,
        requirements: Vec<BackendRequirement>,
        parent: Option<Weak<Self>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            provider_states: Default::default(),
            parent: parent.clone(),
            publish_gate: Default::default(),
            discovery: tokio::sync::RwLock::new(discovery),
            requirements: tokio::sync::RwLock::new(requirements),
            discovery_active: AtomicBool::new(false),
            operation: tokio::sync::Mutex::new(()),
            snapshot_entries: super::data::FeatureResource::private("provider.snapshot.entries"),
            snapshot_selection: super::data::FeatureResource::private(
                "provider.snapshot.selection",
            ),
            discovery_entries: super::data::FeatureResource::private("provider.discovery.entries"),
            discovery_selection: super::data::FeatureResource::private(
                "provider.discovery.selection",
            ),
            options: if parent.is_some() {
                super::data::FeatureResource::private("options")
            } else {
                super::data::FeatureResource::new("options")
            },
        })
    }

    pub async fn provider_runtime(self: &Arc<Self>, id: &str) -> Arc<Self> {
        let mut providers = self.provider_states.write().await;
        if let Some((_, runtime)) = providers.iter().find(|(provider, _)| provider == id) {
            return runtime.clone();
        }
        let runtime = Self::new_inner(
            BootDiscoverySnapshot {
                entries: Vec::new(),
                state: BootEntryDiscoveryState::Discovering,
                selected_entry_id: None,
            },
            Vec::new(),
            Some(Arc::downgrade(self)),
        );
        providers.push((id.to_owned(), runtime.clone()));
        runtime
    }

    pub async fn provider_count(&self) -> usize {
        self.provider_states.read().await.len()
    }

    pub async fn discovery(&self) -> BootDiscoverySnapshot {
        self.discovery.read().await.clone()
    }

    pub async fn set_discovery(&self, discovery: BootDiscoverySnapshot) {
        *self.discovery.write().await = discovery;
    }

    pub async fn set_selected_entry_id(&self, selected_entry_id: Option<String>) {
        self.discovery.write().await.selected_entry_id = selected_entry_id;
    }

    pub async fn clear_selected_entry_id_if(&self, expected_entry_id: &str) {
        let mut discovery = self.discovery.write().await;
        if discovery.selected_entry_id.as_deref() == Some(expected_entry_id) {
            discovery.selected_entry_id = None;
        }
    }

    pub async fn requirements(&self) -> Vec<BackendRequirement> {
        self.requirements.read().await.clone()
    }

    pub async fn set_requirements(&self, requirements: Vec<BackendRequirement>) {
        *self.requirements.write().await = requirements;
    }

    pub fn try_begin_discovery(self: &Arc<Self>) -> Option<BootDiscoveryLease> {
        self.discovery_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| BootDiscoveryLease {
                runtime: self.clone(),
            })
    }

    pub fn publish_options<'a>(
        &'a self,
        fc: &'a Arc<super::connection::FeatureConnection>,
        cause: impl Into<String>,
    ) -> futures::future::BoxFuture<'a, ()> {
        let cause = cause.into();
        Box::pin(async move {
            let _publish = self.publish_gate.lock().await;
            let children = self.provider_states.read().await.clone();
            if !children.is_empty() {
                let mut merged = BootOptionsSnapshot {
                    entries: Vec::new(),
                    selection: None,
                    providers: Vec::new(),
                };
                let count = children.len();
                for (id, child) in children {
                    if let Some(mut options) = child.options.snapshot().await.value {
                        for entry in &mut options.entries {
                            entry.id = option_id(&id, &entry.id, count);
                        }
                        if merged.selection.is_none() {
                            merged.selection = options.selection.map(|mut selected| {
                                selected.entry_id = option_id(&id, &selected.entry_id, count);
                                selected
                            });
                        }
                        merged.entries.extend(options.entries);
                        merged.providers.extend(options.providers);
                    }
                }
                let _ = self.options.commit(fc, merged, cause, None).await;
                return;
            }
            fn empty<T>() -> BootProviderSnapshot<T> {
                BootProviderSnapshot {
                    provider: String::new(),
                    phase: BootProviderPhase::Loading,
                    data: None,
                    fault: None,
                }
            }

            let snapshot_entries = self
                .snapshot_entries
                .snapshot()
                .await
                .value
                .unwrap_or_else(empty);
            let discovery_entries = self
                .discovery_entries
                .snapshot()
                .await
                .value
                .unwrap_or_else(empty);
            let snapshot_selection = self
                .snapshot_selection
                .snapshot()
                .await
                .value
                .unwrap_or_else(empty);
            let discovery_selection = self
                .discovery_selection
                .snapshot()
                .await
                .value
                .unwrap_or_else(empty);
            let mut options = Vec::new();
            if let Some(data) = snapshot_entries.data.as_ref() {
                options.extend(data.entries.iter().map(|entry| BootOptionEntry {
                    id: entry.id.clone(),
                    provider: snapshot_entries.provider.clone(),
                    label: Some(entry.label.clone()),
                    group: entry.group.clone(),
                    badges: entry.badges.clone(),
                    supported_modes: entry.supported_modes.clone(),
                    enabled: entry.enabled,
                    selectable: entry.selectable,
                }));
            }
            if let Some(data) = discovery_entries.data.as_ref() {
                options.extend(data.entries.iter().map(|entry| BootOptionEntry {
                    id: entry.id.clone(),
                    provider: discovery_entries.provider.clone(),
                    label: Some(entry.label.clone()),
                    group: entry.group.clone(),
                    badges: entry.badges.clone(),
                    supported_modes: entry.supported_modes.clone(),
                    enabled: entry.enabled,
                    selectable: entry.selectable,
                }));
            }
            let mut seen_entry_ids = std::collections::HashSet::new();
            options.retain(|entry| seen_entry_ids.insert(entry.id.clone()));
            let selection = [
                snapshot_selection.data.as_ref(),
                discovery_selection.data.as_ref(),
            ]
            .into_iter()
            .flatten()
            .find_map(|value| {
                let entry_id = value.entry_id.as_ref()?;
                let entry = options.iter().find(|entry| &entry.id == entry_id)?;
                let mode = value
                    .mode
                    .or_else(|| entry.supported_modes.first().copied())?;
                Some(BootOptionSelection {
                    entry_id: entry.id.clone(),
                    mode,
                })
            });
            let mut providers: Vec<BootOptionProviderStatus> = Vec::new();
            for (provider, phase, fault, complete) in [
                (
                    &snapshot_entries.provider,
                    if snapshot_selection.phase == BootProviderPhase::Faulted {
                        snapshot_selection.phase
                    } else {
                        snapshot_entries.phase
                    },
                    snapshot_selection
                        .fault
                        .clone()
                        .or_else(|| snapshot_entries.fault.clone()),
                    snapshot_entries
                        .data
                        .as_ref()
                        .is_some_and(|data| data.complete),
                ),
                (
                    &discovery_entries.provider,
                    if discovery_selection.phase == BootProviderPhase::Faulted {
                        discovery_selection.phase
                    } else {
                        discovery_entries.phase
                    },
                    discovery_selection
                        .fault
                        .clone()
                        .or_else(|| discovery_entries.fault.clone()),
                    discovery_entries
                        .data
                        .as_ref()
                        .is_some_and(|data| data.complete),
                ),
            ] {
                if !provider.is_empty() {
                    if let Some(existing) = providers
                        .iter_mut()
                        .find(|status| status.provider == *provider)
                    {
                        existing.complete &= complete;
                        if phase == BootProviderPhase::Faulted
                            || (phase == BootProviderPhase::Loading
                                && existing.phase != BootProviderPhase::Faulted)
                        {
                            existing.phase = phase;
                            existing.fault = fault;
                        }
                    } else {
                        providers.push(BootOptionProviderStatus {
                            provider: provider.clone(),
                            phase,
                            complete,
                            fault,
                        });
                    }
                }
            }
            let snapshot = BootOptionsSnapshot {
                entries: options,
                selection,
                providers,
            };
            let _ = self.options.commit(fc, snapshot, cause, None).await;
            if let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade) {
                parent.publish_options(fc, "provider").await;
            }
        })
    }
}

pub(super) fn option_id(provider: &str, entry: &str, count: usize) -> String {
    if count > 1 {
        format!("{}:{provider}:{entry}", provider.len())
    } else {
        entry.to_owned()
    }
}

/// Backend-facing state port that turns protocol discovery updates into
/// independently revisioned Boot resources without coupling protocol crates
/// to `FeatureConnection`.
pub struct BootStatePublisher {
    runtime: Arc<BootFeatureRuntime>,
    connection: Weak<super::connection::FeatureConnection>,
    provider: String,
    backend_generation: u64,
}

impl BootStatePublisher {
    pub fn new(
        runtime: Arc<BootFeatureRuntime>,
        connection: &Arc<super::connection::FeatureConnection>,
        provider: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            connection: Arc::downgrade(connection),
            provider: provider.into(),
            backend_generation: connection.backend_generation(),
        })
    }

    pub async fn publish_current(&self) {
        let Some(connection) = self.connection.upgrade() else {
            return;
        };
        let Ok(_effect) = connection
            .backend_effect_guard(self.backend_generation)
            .await
        else {
            return;
        };
        let discovery = self.runtime.discovery().await;
        self.publish_discovery(discovery).await;
    }

    async fn publish_discovery(&self, discovery: BootDiscoverySnapshot) {
        let Some(fc) = self.connection.upgrade() else {
            return;
        };
        let complete = discovery.state != BootEntryDiscoveryState::Discovering;
        let catalog = BootEntryCatalog {
            entries: discovery.entries.clone(),
            discovery: discovery.state,
            complete,
        };
        let (phase, fault) = match discovery.state {
            BootEntryDiscoveryState::Discovering => (BootProviderPhase::Loading, None),
            BootEntryDiscoveryState::Available | BootEntryDiscoveryState::InventoryEmpty => {
                (BootProviderPhase::Ready, None)
            }
            BootEntryDiscoveryState::Unsupported => (BootProviderPhase::Unsupported, None),
            BootEntryDiscoveryState::InventoryUnavailable => (
                BootProviderPhase::Faulted,
                Some(BootProviderFault {
                    code: "boot_inventory_unavailable".to_string(),
                    message: "The boot entry inventory is unavailable".to_string(),
                    retryable: true,
                }),
            ),
            BootEntryDiscoveryState::TransportUnavailable => (
                BootProviderPhase::Faulted,
                Some(BootProviderFault {
                    code: "boot_entry_transport_unavailable".to_string(),
                    message: "The boot entry provider is unavailable".to_string(),
                    retryable: true,
                }),
            ),
        };
        let _ = self
            .runtime
            .discovery_entries
            .commit(
                &fc,
                BootProviderSnapshot {
                    provider: self.provider.clone(),
                    phase,
                    data: Some(catalog),
                    fault: fault.clone(),
                },
                "provider",
                None,
            )
            .await;
        let selection_phase = if fault.is_some() {
            BootProviderPhase::Faulted
        } else if discovery.state == BootEntryDiscoveryState::Discovering
            && discovery.selected_entry_id.is_none()
        {
            BootProviderPhase::Loading
        } else if discovery.state == BootEntryDiscoveryState::Unsupported {
            BootProviderPhase::Unsupported
        } else {
            BootProviderPhase::Ready
        };
        let _ = self
            .runtime
            .discovery_selection
            .commit(
                &fc,
                BootProviderSnapshot {
                    provider: self.provider.clone(),
                    phase: selection_phase,
                    data: Some(BootEntrySelection {
                        entry_id: discovery.selected_entry_id,
                        mode: None,
                    }),
                    fault,
                },
                "provider",
                None,
            )
            .await;
        self.runtime.publish_options(&fc, "provider").await;
    }
}

#[async_trait]
impl BootDiscoveryStore for BootStatePublisher {
    async fn discovery(&self) -> BootDiscoverySnapshot {
        self.runtime.discovery().await
    }

    async fn set_discovery(&self, discovery: BootDiscoverySnapshot) {
        let Some(connection) = self.connection.upgrade() else {
            return;
        };
        let Ok(_effect) = connection
            .backend_effect_guard(self.backend_generation)
            .await
        else {
            return;
        };
        self.runtime.set_discovery(discovery.clone()).await;
        self.publish_discovery(discovery).await;
    }
}

#[async_trait]
impl BootStateStore for BootStatePublisher {
    async fn requirements(&self) -> Vec<BackendRequirement> {
        self.runtime.requirements().await
    }

    async fn set_requirements(&self, requirements: Vec<BackendRequirement>) {
        let Some(connection) = self.connection.upgrade() else {
            return;
        };
        let Ok(_effect) = connection
            .backend_effect_guard(self.backend_generation)
            .await
        else {
            return;
        };
        self.runtime.set_requirements(requirements).await;
    }

    async fn set_selected_entry_id(&self, selected_entry_id: Option<String>) {
        let Some(connection) = self.connection.upgrade() else {
            return;
        };
        let Ok(_effect) = connection
            .backend_effect_guard(self.backend_generation)
            .await
        else {
            return;
        };
        self.runtime
            .set_selected_entry_id(selected_entry_id.clone())
            .await;
        let discovery = self.runtime.discovery().await;
        self.publish_discovery(discovery).await;
    }

    async fn clear_selected_entry_id_if(&self, expected_entry_id: &str) {
        let Some(connection) = self.connection.upgrade() else {
            return;
        };
        let Ok(_effect) = connection
            .backend_effect_guard(self.backend_generation)
            .await
        else {
            return;
        };
        self.runtime
            .clear_selected_entry_id_if(expected_entry_id)
            .await;
        let discovery = self.runtime.discovery().await;
        self.publish_discovery(discovery).await;
        if let Some(fc) = self.connection.upgrade() {
            self.runtime.publish_options(&fc, "provider").await;
        }
    }
}

pub async fn feature_runtime(
    fc: &Arc<super::connection::FeatureConnection>,
) -> Arc<BootFeatureRuntime> {
    fc.get_or_init_feature_runtime(|| {
        BootFeatureRuntime::new(
            BootDiscoverySnapshot {
                entries: Vec::new(),
                state: BootEntryDiscoveryState::Unsupported,
                selected_entry_id: None,
            },
            Vec::new(),
        )
    })
    .await
}

pub struct BootDiscoveryLease {
    runtime: Arc<BootFeatureRuntime>,
}

impl Drop for BootDiscoveryLease {
    fn drop(&mut self) {
        self.runtime
            .discovery_active
            .store(false, Ordering::Release);
    }
}

pub fn mount(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handler::status))
        .route(&format!("{base}/selection"), post(handler::set_selection))
        .route(&format!("{base}/retry"), post(handler::retry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmc::feature::config::{BackendParams, FeatureConfig, FeatureParams};
    use crate::bmc::feature::connection::FeatureConnection;

    fn runtime() -> Arc<BootFeatureRuntime> {
        BootFeatureRuntime::new(
            BootDiscoverySnapshot {
                entries: Vec::new(),
                state: BootEntryDiscoveryState::Discovering,
                selected_entry_id: None,
            },
            Vec::new(),
        )
    }

    fn connection() -> Arc<FeatureConnection> {
        FeatureConnection::new(
            "dev1".to_string(),
            "boot".to_string(),
            "test".to_string(),
            FeatureConfig {
                backend: "test".to_string(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        )
    }

    fn entry(id: &str, label: &str, mode: BootMode) -> BootEntry {
        BootEntry {
            id: id.to_string(),
            label: label.to_string(),
            group: None,
            badges: Vec::new(),
            enabled: true,
            selectable: true,
            supported_modes: vec![mode],
        }
    }

    #[tokio::test]
    async fn only_aggregator_publishes_the_public_options_revision_stream() {
        use crate::bmc::feature::connection::ConnectionEvent;
        let runtime = runtime();
        let fc = connection();
        let mut events = fc.data_event_tx.subscribe();
        for id in ["first", "second"] {
            let child = runtime.provider_runtime(id).await;
            child
                .snapshot_entries
                .commit(
                    &fc,
                    BootProviderSnapshot::ready(
                        id,
                        BootEntryCatalog {
                            entries: vec![entry(id, id, BootMode::Once)],
                            discovery: BootEntryDiscoveryState::Available,
                            complete: true,
                        },
                    ),
                    "test",
                    None,
                )
                .await
                .unwrap();
            child.publish_options(&fc, "test").await;
        }
        let second = runtime.provider_runtime("second").await;
        for index in 0..6 {
            second
                .snapshot_selection
                .commit(
                    &fc,
                    BootProviderSnapshot::ready(
                        "second",
                        BootEntrySelection {
                            entry_id: Some("second".into()),
                            mode: Some(if index % 2 == 0 {
                                BootMode::Once
                            } else {
                                BootMode::Persistent
                            }),
                        },
                    ),
                    "test",
                    None,
                )
                .await
                .unwrap();
            second.publish_options(&fc, "test").await;
        }
        let mut revision = 0;
        let mut latest = None;
        while let Ok(event) = events.try_recv() {
            let ConnectionEvent::DataChanged {
                resource,
                revision: next,
                data,
                ..
            } = event
            else {
                continue;
            };
            assert_eq!(resource, "options");
            assert_eq!(next, revision + 1);
            revision = next;
            latest = Some(data);
        }
        let snapshot = runtime.options.snapshot().await;
        assert_eq!(revision, snapshot.revision);
        assert_eq!(
            latest.unwrap(),
            serde_json::to_value(snapshot.value.unwrap()).unwrap()
        );
    }

    #[tokio::test]
    async fn replaced_backend_publisher_cannot_change_provider_state() {
        let runtime = runtime();
        let fc = connection();
        let child = runtime.provider_runtime("discovery").await;
        let old = BootStatePublisher::new(child.clone(), &fc, "discovery");
        fc.clear_backend_runtime().await;
        let current = BootStatePublisher::new(child.clone(), &fc, "discovery");
        let fresh = BootDiscoverySnapshot {
            entries: vec![entry("new", "new", BootMode::Once)],
            state: BootEntryDiscoveryState::Available,
            selected_entry_id: Some("new".into()),
        };
        current.set_discovery(fresh.clone()).await;
        let revision = runtime.options.snapshot().await.revision;
        old.set_discovery(BootDiscoverySnapshot {
            entries: vec![],
            state: BootEntryDiscoveryState::InventoryEmpty,
            selected_entry_id: None,
        })
        .await;
        assert_eq!(child.discovery().await, fresh);
        assert_eq!(runtime.options.snapshot().await.revision, revision);
    }

    #[tokio::test]
    async fn provider_contributions_publish_as_one_entry_list() {
        let runtime = runtime();
        let fc = connection();
        runtime
            .snapshot_entries
            .commit(
                &fc,
                BootProviderSnapshot::ready(
                    "provider-a",
                    BootEntryCatalog {
                        entries: vec![entry("a", "Entry A", BootMode::Persistent)],
                        discovery: BootEntryDiscoveryState::Available,
                        complete: true,
                    },
                ),
                "test",
                None,
            )
            .await
            .unwrap();
        runtime
            .discovery_entries
            .commit(
                &fc,
                BootProviderSnapshot::ready(
                    "provider-b",
                    BootEntryCatalog {
                        entries: vec![entry("b", "Entry B", BootMode::Once)],
                        discovery: BootEntryDiscoveryState::Available,
                        complete: true,
                    },
                ),
                "test",
                None,
            )
            .await
            .unwrap();

        runtime.publish_options(&fc, "test").await;
        let options = runtime.options.snapshot().await.value.unwrap();
        assert_eq!(options.entries.len(), 2);
        assert_eq!(
            options.entries[0].supported_modes,
            vec![BootMode::Persistent]
        );
        assert_eq!(options.entries[1].supported_modes, vec![BootMode::Once]);
    }

    #[tokio::test]
    async fn one_provider_fault_does_not_hide_other_provider_entries() {
        let runtime = runtime();
        let fc = connection();
        runtime
            .snapshot_entries
            .commit(
                &fc,
                BootProviderSnapshot::ready(
                    "slow-provider",
                    BootEntryCatalog {
                        entries: vec![entry("slow", "Slow", BootMode::Once)],
                        discovery: BootEntryDiscoveryState::Available,
                        complete: true,
                    },
                ),
                "test",
                None,
            )
            .await
            .unwrap();
        runtime
            .snapshot_selection
            .commit(
                &fc,
                BootProviderSnapshot {
                    provider: "slow-provider".to_string(),
                    phase: BootProviderPhase::Faulted,
                    data: None,
                    fault: Some(BootProviderFault {
                        code: "unavailable".to_string(),
                        message: "selection unavailable".to_string(),
                        retryable: true,
                    }),
                },
                "test",
                None,
            )
            .await
            .unwrap();
        runtime
            .discovery_entries
            .commit(
                &fc,
                BootProviderSnapshot::ready(
                    "fast-provider",
                    BootEntryCatalog {
                        entries: vec![entry("fast", "Fast", BootMode::Once)],
                        discovery: BootEntryDiscoveryState::Available,
                        complete: true,
                    },
                ),
                "test",
                None,
            )
            .await
            .unwrap();

        runtime.publish_options(&fc, "test").await;
        let options = runtime.options.snapshot().await.value.unwrap();
        assert_eq!(
            options
                .entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["slow", "fast"]
        );
        assert!(options
            .providers
            .iter()
            .any(|provider| provider.provider == "slow-provider"
                && provider.phase == BootProviderPhase::Faulted));
    }

    #[tokio::test]
    async fn feature_connection_retains_discovery_when_backend_runtime_is_cleared() {
        let runtime = runtime();
        let fc = connection();
        fc.set_feature_runtime(runtime.clone()).await;
        fc.set_backend_runtime(Arc::new("protocol-adapter".to_string()))
            .await;
        runtime
            .set_discovery(BootDiscoverySnapshot {
                entries: vec![BootEntry {
                    id: "opaque".to_string(),
                    label: "UEFI disk".to_string(),
                    group: None,
                    badges: Vec::new(),
                    supported_modes: vec![BootMode::Once],
                    enabled: true,
                    selectable: false,
                }],
                state: BootEntryDiscoveryState::Available,
                selected_entry_id: None,
            })
            .await;

        fc.clear_backend_runtime().await;
        assert!(fc.backend_runtime::<Arc<String>>().await.is_none());
        let retained = fc
            .feature_runtime::<Arc<BootFeatureRuntime>>()
            .await
            .expect("feature-owned runtime survives protocol disconnect");
        let snapshot = retained.discovery().await;
        assert_eq!(snapshot.state, BootEntryDiscoveryState::Available);
        assert_eq!(snapshot.entries.len(), 1);

        retained
            .set_selected_entry_id(Some("newer-selection".to_string()))
            .await;
        retained.clear_selected_entry_id_if("older-selection").await;
        assert_eq!(
            retained.discovery().await.selected_entry_id.as_deref(),
            Some("newer-selection")
        );
        retained.clear_selected_entry_id_if("newer-selection").await;
        assert!(retained.discovery().await.selected_entry_id.is_none());
    }

    #[test]
    fn discovery_lease_prevents_duplicate_probes_and_releases_on_drop() {
        let runtime = runtime();
        let lease = runtime.try_begin_discovery().expect("first probe");
        assert!(runtime.try_begin_discovery().is_none());
        drop(lease);
        assert!(runtime.try_begin_discovery().is_some());
    }
}
