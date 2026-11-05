use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{
    BootBackend, BootEntryCatalog, BootEntryDiscoveryState, BootEntrySelection, BootFeatureRuntime,
    BootProviderFault, BootProviderPhase, BootProviderSnapshot, BootSelection,
};
use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::requirements::{
    BackendRequirement, RemediationAction, RemediationResult, RequirementId,
};

#[derive(Clone)]
struct RegisteredBootProvider {
    id: String,
    backend: Arc<dyn BootBackend>,
}

/// Registry for independent Boot providers. Providers only contribute opaque
/// entries and their capabilities; the feature layer never classifies them.
pub struct BootProviderRegistry {
    snapshot: Vec<RegisteredBootProvider>,
    discovery: Vec<RegisteredBootProvider>,
    refresh_active: AtomicBool,
    generation: std::sync::OnceLock<u64>,
}

impl BootProviderRegistry {
    pub fn new() -> Self {
        Self {
            snapshot: Vec::new(),
            discovery: Vec::new(),
            refresh_active: AtomicBool::new(false),
            generation: std::sync::OnceLock::new(),
        }
    }

    pub fn register_snapshot_provider(
        mut self,
        id: impl Into<String>,
        backend: Arc<dyn BootBackend>,
    ) -> Self {
        self.snapshot.push(RegisteredBootProvider {
            id: id.into(),
            backend,
        });
        self
    }

    pub fn register_discovery_provider(
        mut self,
        id: impl Into<String>,
        backend: Arc<dyn BootBackend>,
    ) -> Self {
        self.discovery.push(RegisteredBootProvider {
            id: id.into(),
            backend,
        });
        self
    }

    fn generation(&self, fc: &FeatureConnection) -> u64 {
        *self.generation.get_or_init(|| fc.backend_generation())
    }

    pub async fn initialize(
        self: &Arc<Self>,
        fc: &Arc<FeatureConnection>,
        runtime: &Arc<BootFeatureRuntime>,
    ) {
        let Ok(_effect) = fc.backend_effect_guard(self.generation(fc)).await else {
            return;
        };
        for provider in &self.snapshot {
            let runtime = runtime.provider_runtime(&provider.id).await;
            let capabilities = provider.backend.capabilities().await;
            let catalog = BootEntryCatalog {
                entries: capabilities.entries,
                discovery: capabilities.entry_discovery,
                complete: capabilities.entry_discovery != BootEntryDiscoveryState::Discovering,
            };
            let _ = runtime
                .snapshot_entries
                .commit(
                    fc,
                    BootProviderSnapshot {
                        provider: provider.id.clone(),
                        phase: BootProviderPhase::Ready,
                        data: Some(catalog),
                        fault: None,
                    },
                    "provider_registered",
                    None,
                )
                .await;
            let _ = runtime
                .snapshot_selection
                .commit(
                    fc,
                    BootProviderSnapshot::loading(provider.id.clone()),
                    "provider_registered",
                    None,
                )
                .await;
            runtime.publish_options(fc, "provider_registered").await;
        }

        for provider in &self.discovery {
            let runtime = runtime.provider_runtime(&provider.id).await;
            let capabilities = provider.backend.capabilities().await;
            let current = runtime.discovery_entries.snapshot().await.value;
            let existing = current.and_then(|snapshot| snapshot.data);
            let catalog = existing.unwrap_or(BootEntryCatalog {
                entries: capabilities.entries,
                discovery: capabilities.entry_discovery,
                complete: capabilities.entry_discovery != BootEntryDiscoveryState::Discovering,
            });
            let _ = runtime
                .discovery_entries
                .commit(
                    fc,
                    BootProviderSnapshot {
                        provider: provider.id.clone(),
                        phase: BootProviderPhase::Loading,
                        data: Some(catalog),
                        fault: None,
                    },
                    "provider_registered",
                    None,
                )
                .await;
            let _ = runtime
                .discovery_selection
                .commit(
                    fc,
                    BootProviderSnapshot::loading(provider.id.clone()),
                    "provider_registered",
                    None,
                )
                .await;
            runtime.publish_options(fc, "provider_registered").await;
        }
        self.request_snapshot_refresh(fc.clone(), runtime.clone());
        runtime.publish_options(fc, "provider_registered").await;
    }

    pub fn request_snapshot_refresh(
        self: &Arc<Self>,
        fc: Arc<FeatureConnection>,
        runtime: Arc<BootFeatureRuntime>,
    ) {
        if self.snapshot.is_empty() {
            return;
        }
        if self
            .refresh_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let registry = self.clone();
        tokio::spawn(async move {
            let refreshes = registry.snapshot.iter().map(|provider| async {
                let child = runtime.provider_runtime(&provider.id).await;
                registry
                    .refresh_snapshot_provider(&fc, &child, provider)
                    .await;
            });
            futures::future::join_all(refreshes).await;
            registry.refresh_active.store(false, Ordering::Release);
        });
    }

    async fn refresh_snapshot_provider(
        &self,
        fc: &Arc<FeatureConnection>,
        runtime: &Arc<BootFeatureRuntime>,
        provider: &RegisteredBootProvider,
    ) {
        let _operation = runtime.operation.lock().await;
        let status = provider.backend.status().await;
        let Ok(_effect) = fc.backend_effect_guard(self.generation(fc)).await else {
            return;
        };
        match status {
            Ok(status) => {
                let catalog = BootEntryCatalog {
                    entries: status.capabilities.entries,
                    discovery: status.capabilities.entry_discovery,
                    complete: true,
                };
                let _ = runtime
                    .snapshot_entries
                    .commit(
                        fc,
                        BootProviderSnapshot::ready(provider.id.clone(), catalog),
                        "refresh",
                        None,
                    )
                    .await;
                let selection = entry_selection(&status.selection);
                let _ = runtime
                    .snapshot_selection
                    .commit(
                        fc,
                        BootProviderSnapshot::ready(provider.id.clone(), selection),
                        "refresh",
                        None,
                    )
                    .await;
                runtime.publish_options(fc, "refresh").await;
            }
            Err(_) => {
                let previous = runtime
                    .snapshot_selection
                    .snapshot()
                    .await
                    .value
                    .and_then(|value| value.data);
                let _ = runtime
                    .snapshot_selection
                    .commit(
                        fc,
                        BootProviderSnapshot {
                            provider: provider.id.clone(),
                            phase: BootProviderPhase::Faulted,
                            data: previous,
                            fault: Some(BootProviderFault {
                                code: "boot_provider_unavailable".to_string(),
                                message: "The boot provider could not read its state".to_string(),
                                retryable: true,
                            }),
                        },
                        "refresh",
                        None,
                    )
                    .await;
                runtime.publish_options(fc, "refresh").await;
            }
        }
    }

    pub async fn set_selection(
        &self,
        fc: &Arc<FeatureConnection>,
        runtime: &Arc<BootFeatureRuntime>,
        selection: BootSelection,
        mutation_id: Option<String>,
    ) -> Result<(), bmc_feature_api::mutation::MutationError> {
        let entry_id = selection.entry_id.as_deref().ok_or_else(|| {
            bmc_feature_api::mutation::MutationError::rejected(
                "boot selection must identify an entry",
            )
        })?;
        let options = runtime.options.snapshot().await.value.ok_or_else(|| {
            bmc_feature_api::mutation::MutationError::rejected("boot entries are not available")
        })?;
        let entry = options
            .entries
            .iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| {
                bmc_feature_api::mutation::MutationError::rejected("unknown boot entry")
            })?;

        let provider = self
            .snapshot
            .iter()
            .chain(&self.discovery)
            .find(|provider| provider.id == entry.provider)
            .ok_or_else(|| {
                bmc_feature_api::mutation::MutationError::rejected(
                    "boot entry provider is not connected",
                )
            })?;
        let child = runtime.provider_runtime(&provider.id).await;
        let _operation = child.operation.lock().await;
        let _backend_operation = fc
            .backend_operation_guard(self.generation(fc))
            .await
            .map_err(bmc_feature_api::mutation::MutationError::rejected)?;
        let child_options = child.options.snapshot().await.value.ok_or_else(|| {
            bmc_feature_api::mutation::MutationError::rejected(
                "boot provider entries are unavailable",
            )
        })?;
        let count = runtime.provider_count().await;
        let raw = child_options
            .entries
            .iter()
            .find(|candidate| super::option_id(&provider.id, &candidate.id, count) == entry.id)
            .ok_or_else(|| {
                bmc_feature_api::mutation::MutationError::rejected(
                    "boot provider entry is unavailable",
                )
            })?;
        let requested = BootSelection {
            entry_id: Some(raw.id.clone()),
            mode: selection.mode,
        };
        if !raw.enabled || !raw.selectable || !raw.supported_modes.contains(&selection.mode) {
            return Err(bmc_feature_api::mutation::MutationError::rejected(
                "boot selection is no longer supported",
            ));
        }
        provider
            .backend
            .set_selection_outcome(requested.clone())
            .await?;
        let observed = provider.backend.selection().await?;
        if observed != requested {
            return Err("boot selection readback mismatch; remote result unknown".into());
        }
        let _effect = fc.backend_effect_guard(self.generation(fc)).await?;
        let selection_resource = if self
            .snapshot
            .iter()
            .any(|candidate| candidate.id == provider.id)
        {
            &child.snapshot_selection
        } else {
            &child.discovery_selection
        };
        selection_resource
            .commit(
                fc,
                BootProviderSnapshot::ready(provider.id.clone(), entry_selection(&observed)),
                "mutation",
                mutation_id,
            )
            .await?;
        child.publish_options(fc, "mutation").await;
        runtime.publish_options(fc, "mutation").await;
        Ok(())
    }

    pub async fn requirements(&self) -> Result<Vec<BackendRequirement>, String> {
        let mut requirements = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for provider in self.discovery.iter().chain(&self.snapshot) {
            if seen.insert(&provider.id) {
                requirements.extend(provider.backend.requirements().await?);
            }
        }
        Ok(requirements)
    }

    pub async fn remediate_requirement(
        &self,
        requirement_id: RequirementId,
        action: RemediationAction,
    ) -> Result<RemediationResult, String> {
        let mut candidates = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for provider in self.discovery.iter().chain(&self.snapshot) {
            if seen.insert(&provider.id)
                && provider
                    .backend
                    .requirements()
                    .await?
                    .iter()
                    .any(|requirement| requirement.id == requirement_id)
            {
                candidates.push(provider);
            }
        }
        if candidates.len() != 1 {
            return Err("Boot requirement provider is absent or ambiguous".into());
        }
        candidates[0]
            .backend
            .remediate_requirement(requirement_id, action)
            .await
    }
}

fn entry_selection(selection: &BootSelection) -> BootEntrySelection {
    BootEntrySelection {
        entry_id: selection.entry_id.clone(),
        mode: Some(selection.mode),
    }
}

impl Default for BootProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        BootCapabilities, BootDiscoverySnapshot, BootEntry, BootMode, BootStatePublisher,
    };
    use super::*;
    use bmc_feature_api::boot::{BootDiscoveryStore, BootStateStore};

    struct Backend {
        selected: tokio::sync::Mutex<BootSelection>,
        writes: std::sync::atomic::AtomicUsize,
        failure: Option<bool>,
    }
    impl Backend {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                selected: tokio::sync::Mutex::new(BootSelection {
                    entry_id: Some("same-opaque-id".into()),
                    mode: BootMode::Once,
                }),
                writes: Default::default(),
                failure: None,
            })
        }
    }
    #[async_trait::async_trait]
    impl BootBackend for Backend {
        async fn capabilities(&self) -> BootCapabilities {
            BootCapabilities {
                entries: vec![BootEntry {
                    id: "same-opaque-id".into(),
                    label: "Disk".into(),
                    group: None,
                    badges: Vec::new(),
                    enabled: true,
                    selectable: true,
                    supported_modes: vec![BootMode::Once, BootMode::Persistent],
                }],
                entry_discovery: BootEntryDiscoveryState::Unsupported,
            }
        }
        async fn selection(&self) -> Result<BootSelection, String> {
            if self.failure.is_some() && self.writes.load(Ordering::SeqCst) > 0 {
                return Err(
                    "private-provider-detail https://fixture.invalid/?token=private".into(),
                );
            }
            Ok(self.selected.lock().await.clone())
        }
        async fn set_selection(&self, selection: BootSelection) -> Result<(), String> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.failure == Some(true) {
                return Err("write acknowledgement lost".into());
            }
            *self.selected.lock().await = selection;
            Ok(())
        }
    }
    fn connection() -> Arc<FeatureConnection> {
        FeatureConnection::new(
            "test".into(),
            "boot".into(),
            "test".into(),
            crate::bmc::feature::config::FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: Vec::new(),
            },
        )
    }
    fn runtime() -> Arc<BootFeatureRuntime> {
        BootFeatureRuntime::new(
            BootDiscoverySnapshot {
                entries: Vec::new(),
                state: BootEntryDiscoveryState::Unsupported,
                selected_entry_id: None,
            },
            Vec::new(),
        )
    }

    async fn initialized_registry(
        backend: Arc<dyn BootBackend>,
    ) -> (
        Arc<FeatureConnection>,
        Arc<BootFeatureRuntime>,
        Arc<BootProviderRegistry>,
    ) {
        let fc = connection();
        let runtime = runtime();
        let registry =
            Arc::new(BootProviderRegistry::new().register_snapshot_provider("provider", backend));
        fc.set_backend_runtime(registry.clone()).await;
        registry.initialize(&fc, &runtime).await;
        while registry.refresh_active.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        (fc, runtime, registry)
    }

    #[tokio::test]
    async fn queued_boot_selection_never_dispatches_after_stop_or_replacement() {
        for stop in [true, false] {
            let backend = Backend::new();
            let (fc, runtime, registry) = initialized_registry(backend.clone()).await;
            let child = runtime.provider_runtime("provider").await;
            let held = child.operation.lock().await;
            let selection = registry.set_selection(
                &fc,
                &runtime,
                BootSelection {
                    entry_id: Some("same-opaque-id".into()),
                    mode: BootMode::Persistent,
                },
                None,
            );
            tokio::pin!(selection);
            assert!(futures::poll!(&mut selection).is_pending());
            if stop {
                fc.shutdown();
            } else {
                fc.clear_backend_runtime().await;
            }
            drop(held);
            assert!(selection.await.is_err());
            assert_eq!(backend.writes.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn queued_boot_selection_revalidates_current_provider_capabilities() {
        for restriction in 0..3 {
            let backend = Backend::new();
            let (fc, runtime, registry) = initialized_registry(backend.clone()).await;
            let child = runtime.provider_runtime("provider").await;
            let held = child.operation.lock().await;
            let selection = registry.set_selection(
                &fc,
                &runtime,
                BootSelection {
                    entry_id: Some("same-opaque-id".into()),
                    mode: BootMode::Persistent,
                },
                None,
            );
            tokio::pin!(selection);
            assert!(futures::poll!(&mut selection).is_pending());
            let mut snapshot = child.snapshot_entries.snapshot().await.value.unwrap();
            let entry = &mut snapshot.data.as_mut().unwrap().entries[0];
            match restriction {
                0 => entry.enabled = false,
                1 => entry.selectable = false,
                _ => entry.supported_modes = vec![BootMode::Once],
            }
            child
                .snapshot_entries
                .commit(&fc, snapshot, "refresh", None)
                .await
                .unwrap();
            child.publish_options(&fc, "refresh").await;
            drop(held);
            assert_eq!(
                selection.await.unwrap_err().failure,
                bmc_feature_api::mutation::MutationFailure::Rejected
            );
            assert_eq!(backend.writes.load(Ordering::SeqCst), 0);
        }
    }

    struct PublishingBackend {
        inner: Arc<Backend>,
        publisher: tokio::sync::OnceCell<Arc<BootStatePublisher>>,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    #[async_trait::async_trait]
    impl BootBackend for PublishingBackend {
        async fn capabilities(&self) -> BootCapabilities {
            self.inner.capabilities().await
        }
        async fn selection(&self) -> Result<BootSelection, String> {
            self.inner.selection().await
        }
        async fn set_selection(&self, selection: BootSelection) -> Result<(), String> {
            self.inner.set_selection(selection.clone()).await?;
            self.entered.notify_one();
            self.release.notified().await;
            // WS-MAN publishes through this port before its mutation returns.
            self.publisher
                .get()
                .unwrap()
                .set_selected_entry_id(selection.entry_id)
                .await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn boot_selection_pins_backend_through_reentrant_publication_and_readback() {
        for clear in [true, false] {
            let backend = Arc::new(PublishingBackend {
                inner: Backend::new(),
                publisher: Default::default(),
                entered: Default::default(),
                release: Default::default(),
            });
            let (fc, runtime, registry) = initialized_registry(backend.clone()).await;
            let child = runtime.provider_runtime("provider").await;
            assert!(backend
                .publisher
                .set(BootStatePublisher::new(child, &fc, "provider"))
                .is_ok());
            let selection = registry.set_selection(
                &fc,
                &runtime,
                BootSelection {
                    entry_id: Some("same-opaque-id".into()),
                    mode: BootMode::Persistent,
                },
                None,
            );
            tokio::pin!(selection);
            assert!(futures::poll!(&mut selection).is_pending());
            backend.entered.notified().await;
            let replacement = async {
                if clear {
                    fc.clear_backend_runtime().await;
                } else {
                    fc.set_backend_runtime(2u8).await;
                }
            };
            tokio::pin!(replacement);
            assert!(futures::poll!(&mut replacement).is_pending());
            backend.release.notify_one();
            tokio::time::timeout(std::time::Duration::from_secs(1), selection)
                .await
                .unwrap()
                .unwrap();
            replacement.await;
            assert_eq!(backend.inner.writes.load(Ordering::SeqCst), 1);
            assert_eq!(fc.backend_generation(), 1);
        }
    }
    #[tokio::test]
    async fn snapshot_failures_publish_sanitized_faults_in_snapshots_and_events() {
        let fc = connection();
        let runtime = runtime();
        let registry = BootProviderRegistry::new().register_snapshot_provider(
            "provider",
            Arc::new(Backend {
                selected: tokio::sync::Mutex::new(BootSelection {
                    entry_id: None,
                    mode: BootMode::Once,
                }),
                writes: std::sync::atomic::AtomicUsize::new(1),
                failure: Some(false),
            }),
        );
        let child = runtime.provider_runtime("provider").await;
        child
            .snapshot_entries
            .commit(
                &fc,
                BootProviderSnapshot::ready(
                    "provider",
                    BootEntryCatalog {
                        entries: Backend::new().capabilities().await.entries,
                        discovery: BootEntryDiscoveryState::Unsupported,
                        complete: true,
                    },
                ),
                "provider_registered",
                None,
            )
            .await
            .unwrap();
        let mut events = fc.data_event_tx.subscribe();
        registry
            .refresh_snapshot_provider(&fc, &child, &registry.snapshot[0])
            .await;
        let snapshot = runtime.options.snapshot().await.value.unwrap();
        let fault = snapshot.providers[0].fault.as_ref().unwrap();
        assert_eq!(snapshot.providers[0].phase, BootProviderPhase::Faulted);
        assert_eq!(fault.code, "boot_provider_unavailable");
        assert!(fault.retryable);
        assert!(!serde_json::to_string(&snapshot)
            .unwrap()
            .contains("private"));
        let mut published = false;
        while let Ok(event) = events.try_recv() {
            if let super::super::super::connection::ConnectionEvent::DataChanged { data, .. } =
                event
            {
                published = true;
                assert!(!data.to_string().contains("private"));
            }
        }
        assert!(published);
    }

    #[tokio::test]
    async fn failed_dispatch_or_readback_preserves_unknown_result_without_replaying() {
        for failure in [false, true] {
            let fc = connection();
            let runtime = runtime();
            let backend = Arc::new(Backend {
                selected: tokio::sync::Mutex::new(BootSelection {
                    entry_id: Some("same-opaque-id".into()),
                    mode: BootMode::Once,
                }),
                writes: Default::default(),
                failure: Some(failure),
            });
            let registry = Arc::new(
                BootProviderRegistry::new().register_snapshot_provider("provider", backend.clone()),
            );
            registry.initialize(&fc, &runtime).await;
            let result = registry
                .set_selection(
                    &fc,
                    &runtime,
                    BootSelection {
                        entry_id: Some("same-opaque-id".into()),
                        mode: BootMode::Persistent,
                    },
                    None,
                )
                .await
                .unwrap_err();
            assert_eq!(
                result.failure,
                bmc_feature_api::mutation::MutationFailure::Unknown
            );
            assert_eq!(backend.writes.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn multiple_snapshot_providers_keep_ids_and_route_colliding_entries_independently() {
        let fc = connection();
        let runtime = runtime();
        let first = Backend::new();
        let second = Backend::new();
        let registry = Arc::new(
            BootProviderRegistry::new()
                .register_snapshot_provider("primary", first.clone())
                .register_snapshot_provider("secondary", second.clone()),
        );
        registry.initialize(&fc, &runtime).await;
        let options = runtime.options.snapshot().await.value.unwrap();
        assert_eq!(options.entries.len(), 2);
        assert_ne!(options.entries[0].id, options.entries[1].id);
        assert_eq!(
            options
                .providers
                .iter()
                .map(|p| p.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["primary", "secondary"]
        );
        let entry = options
            .entries
            .iter()
            .find(|entry| entry.provider == "secondary")
            .unwrap();
        registry
            .set_selection(
                &fc,
                &runtime,
                BootSelection {
                    entry_id: Some(entry.id.clone()),
                    mode: BootMode::Persistent,
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(first.writes.load(Ordering::SeqCst), 0);
        assert_eq!(second.writes.load(Ordering::SeqCst), 1);
        assert_eq!(
            second.selected.lock().await.entry_id.as_deref(),
            Some("same-opaque-id")
        );
    }
    #[tokio::test]
    async fn discovery_and_selection_are_isolated_per_configured_provider() {
        let fc = connection();
        let runtime = runtime();
        let first = runtime.provider_runtime("first").await;
        let second = runtime.provider_runtime("second").await;
        let publisher_a = BootStatePublisher::new(first.clone(), &fc, "first");
        let publisher_b = BootStatePublisher::new(second.clone(), &fc, "second");
        let entry = Backend::new().capabilities().await.entries.remove(0);
        publisher_a
            .set_discovery(BootDiscoverySnapshot {
                entries: vec![entry.clone()],
                state: BootEntryDiscoveryState::Available,
                selected_entry_id: Some(entry.id.clone()),
            })
            .await;
        publisher_b
            .set_discovery(BootDiscoverySnapshot {
                entries: vec![entry],
                state: BootEntryDiscoveryState::Available,
                selected_entry_id: None,
            })
            .await;
        assert!(second.discovery().await.selected_entry_id.is_none());
        assert!(first.discovery().await.selected_entry_id.is_some());
        assert_eq!(
            runtime
                .options
                .snapshot()
                .await
                .value
                .unwrap()
                .entries
                .len(),
            2
        );
    }
}
