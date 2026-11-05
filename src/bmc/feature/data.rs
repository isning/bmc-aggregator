use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, RwLock};

use super::connection::{ConnectionEvent, FeatureConnection};

#[derive(Clone, Debug)]
pub struct FeatureResourceSnapshot<T> {
    pub revision: u64,
    pub value: Option<T>,
}

/// Feature-owned, reconnect-stable data resource.
///
/// Backends remain strongly typed. This primitive only provides snapshot
/// ownership, refresh single-flight and revisioned device-wide publication.
pub struct FeatureResource<T> {
    name: String,
    publish: bool,
    state: RwLock<FeatureResourceSnapshot<T>>,
    operation: Mutex<()>,
    operation_generation: AtomicU64,
    last_refresh_error: RwLock<Option<(u64, String)>>,
}

impl<T> FeatureResource<T>
where
    T: Clone + PartialEq + Serialize + Send + Sync + 'static,
{
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            publish: true,
            state: RwLock::new(FeatureResourceSnapshot {
                revision: 0,
                value: None,
            }),
            operation: Mutex::new(()),
            operation_generation: AtomicU64::new(0),
            last_refresh_error: RwLock::new(None),
        }
    }

    /// Provider-local snapshots never share the public resource revision stream.
    pub fn private(name: impl Into<String>) -> Self {
        let mut resource = Self::new(name);
        resource.publish = false;
        resource
    }

    pub async fn snapshot(&self) -> FeatureResourceSnapshot<T> {
        self.state.read().await.clone()
    }

    pub async fn refresh<F, Fut>(
        &self,
        fc: &FeatureConnection,
        loader: F,
    ) -> Result<FeatureResourceSnapshot<T>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        self.refresh_at(fc, fc.backend_generation(), loader).await
    }

    pub async fn refresh_at<F, Fut>(
        &self,
        fc: &FeatureConnection,
        backend_generation: u64,
        loader: F,
    ) -> Result<FeatureResourceSnapshot<T>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        let observed_generation = self.operation_generation.load(Ordering::SeqCst);
        let _operation = self.operation.lock().await;
        if fc.is_stopped() || fc.backend_generation() != backend_generation {
            return Err("backend was replaced before refresh".to_owned());
        }
        let current_generation = self.operation_generation.load(Ordering::SeqCst);
        if current_generation != observed_generation {
            if let Some((error_generation, error)) = self.last_refresh_error.read().await.as_ref() {
                if *error_generation == current_generation {
                    return Err(error.clone());
                }
            }
            return Ok(self.snapshot().await);
        }
        let value = match loader().await {
            Ok(value) => value,
            Err(error) => {
                let _backend_guard = fc.backend_commit_guard(backend_generation).await?;
                let generation = self.operation_generation.fetch_add(1, Ordering::SeqCst) + 1;
                *self.last_refresh_error.write().await = Some((generation, error.clone()));
                return Err(error);
            }
        };
        self.commit_locked(fc, backend_generation, value, "refresh", None)
            .await
    }

    /// Own mutation execution until remote completion, even when its caller
    /// cancels. The resource lock remains held through readback and commit.
    pub async fn mutate<F, Fut>(
        self: &std::sync::Arc<Self>,
        fc: &std::sync::Arc<FeatureConnection>,
        mutation_id: Option<String>,
        operation: F,
    ) -> Result<FeatureResourceSnapshot<T>, String>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, String>> + Send + 'static,
    {
        self.mutate_at(fc, fc.backend_generation(), mutation_id, operation)
            .await
    }

    pub async fn mutate_at<F, Fut>(
        self: &std::sync::Arc<Self>,
        fc: &std::sync::Arc<FeatureConnection>,
        backend_generation: u64,
        mutation_id: Option<String>,
        operation: F,
    ) -> Result<FeatureResourceSnapshot<T>, String>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, String>> + Send + 'static,
    {
        let owner = self.clone();
        let fc = fc.clone();
        tokio::spawn(async move {
            owner
                .update_at(&fc, backend_generation, "mutation", mutation_id, operation)
                .await
        })
        .await
        .map_err(|_| "mutation worker failed; inspect remote state before retrying".to_owned())?
    }

    /// Preserve typed mutation uncertainty through readback and publication.
    pub async fn try_mutate<E, F, Fut>(
        &self,
        fc: &FeatureConnection,
        mutation_id: Option<String>,
        operation: F,
    ) -> Result<FeatureResourceSnapshot<T>, E>
    where
        E: From<String>,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        self.try_mutate_at(fc, fc.backend_generation(), mutation_id, operation)
            .await
    }

    pub async fn try_mutate_at<E, F, Fut>(
        &self,
        fc: &FeatureConnection,
        backend_generation: u64,
        mutation_id: Option<String>,
        operation: F,
    ) -> Result<FeatureResourceSnapshot<T>, E>
    where
        E: From<String>,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        let _operation = self.operation.lock().await;
        if fc.is_stopped() || fc.backend_generation() != backend_generation {
            return Err("backend was replaced before operation".to_owned().into());
        }
        let value = operation().await?;
        self.commit_locked(fc, backend_generation, value, "mutation", mutation_id)
            .await
            .map_err(E::from)
    }

    /// Run a typed loader while holding the resource operation lock and
    /// publish its result with an explicit cause. This is useful when an
    /// external mutation must be followed by an authoritative catalog read.
    pub async fn update<F, Fut>(
        &self,
        fc: &FeatureConnection,
        cause: impl Into<String>,
        mutation_id: Option<String>,
        operation: F,
    ) -> Result<FeatureResourceSnapshot<T>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        self.update_at(fc, fc.backend_generation(), cause, mutation_id, operation)
            .await
    }

    pub async fn update_at<F, Fut>(
        &self,
        fc: &FeatureConnection,
        backend_generation: u64,
        cause: impl Into<String>,
        mutation_id: Option<String>,
        operation: F,
    ) -> Result<FeatureResourceSnapshot<T>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        let _operation = self.operation.lock().await;
        if fc.is_stopped() || fc.backend_generation() != backend_generation {
            return Err("backend was replaced before operation".to_owned());
        }
        let value = operation().await?;
        self.commit_locked(fc, backend_generation, value, cause, mutation_id)
            .await
    }

    pub async fn commit(
        &self,
        fc: &FeatureConnection,
        value: T,
        cause: impl Into<String>,
        mutation_id: Option<String>,
    ) -> Result<FeatureResourceSnapshot<T>, String> {
        self.commit_at(fc, fc.backend_generation(), value, cause, mutation_id)
            .await
    }

    pub async fn commit_at(
        &self,
        fc: &FeatureConnection,
        backend_generation: u64,
        value: T,
        cause: impl Into<String>,
        mutation_id: Option<String>,
    ) -> Result<FeatureResourceSnapshot<T>, String> {
        let _operation = self.operation.lock().await;
        self.commit_locked(fc, backend_generation, value, cause, mutation_id)
            .await
    }

    async fn commit_locked(
        &self,
        fc: &FeatureConnection,
        backend_generation: u64,
        value: T,
        cause: impl Into<String>,
        mutation_id: Option<String>,
    ) -> Result<FeatureResourceSnapshot<T>, String> {
        let _backend_guard = fc.backend_commit_guard(backend_generation).await?;
        let data = serde_json::to_value(&value).map_err(|error| error.to_string())?;
        let mut state = self.state.write().await;
        if fc.is_stopped() {
            return Err("feature connection was replaced before data commit".to_string());
        }
        if state.value.as_ref() == Some(&value) {
            self.operation_generation.fetch_add(1, Ordering::SeqCst);
            *self.last_refresh_error.write().await = None;
            return Ok(state.clone());
        }
        state.revision += 1;
        state.value = Some(value.clone());
        let snapshot = state.clone();
        self.operation_generation.fetch_add(1, Ordering::SeqCst);
        *self.last_refresh_error.write().await = None;
        drop(state);
        let revision = snapshot.revision;
        if self.publish {
            let _ = fc.data_event_tx.send(ConnectionEvent::DataChanged {
                feature: fc.id.clone(),
                instance: fc.instance_id.clone(),
                epoch: fc.data_epoch.clone(),
                resource: self.name.to_string(),
                revision,
                cause: cause.into(),
                mutation_id,
                data,
            });
        }
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::FeatureResource;
    use crate::bmc::feature::config::{BackendParams, FeatureConfig, FeatureParams};
    use crate::bmc::feature::connection::{ConnectionEvent, FeatureConnection};

    fn connection() -> Arc<FeatureConnection> {
        FeatureConnection::new(
            "dev".into(),
            "power".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        )
    }

    #[tokio::test]
    async fn cancelled_mutation_keeps_resource_lock_until_blocking_work_completes() {
        let fc = connection();
        let resource = Arc::new(FeatureResource::new("status"));
        let remote = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let first = tokio::spawn({
            let fc = fc.clone();
            let resource = resource.clone();
            let remote = remote.clone();
            async move {
                resource
                    .mutate(&fc, None, move || async move {
                        tokio::task::spawn_blocking(move || {
                            let _ = started.send(());
                            wait.recv_timeout(std::time::Duration::from_secs(5))
                                .unwrap();
                            remote.store(1, std::sync::atomic::Ordering::SeqCst);
                        })
                        .await
                        .unwrap();
                        Ok(1u64)
                    })
                    .await
            }
        });
        ready.await.unwrap();
        first.abort();
        let _ = first.await;
        let mut second = tokio::spawn({
            let fc = fc.clone();
            let resource = resource.clone();
            let remote = remote.clone();
            async move {
                resource
                    .mutate(&fc, None, move || async move {
                        remote.store(0, std::sync::atomic::Ordering::SeqCst);
                        Ok(0u64)
                    })
                    .await
            }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut second)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(remote.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(resource.snapshot().await.value, Some(0));
    }

    #[tokio::test]
    async fn refresh_from_replaced_backend_cannot_commit() {
        let fc = connection();
        fc.set_backend_runtime(1u32).await;
        let resource = FeatureResource::new("status");
        let mut events = fc.data_event_tx.subscribe();
        let result = resource
            .refresh(&fc, || async {
                fc.clear_backend_runtime().await;
                fc.set_backend_runtime(2u32).await;
                Ok("old")
            })
            .await;
        assert!(result.is_err());
        assert_eq!(resource.snapshot().await.revision, 0);
        assert!(events.try_recv().is_err());
        let old_generation = fc.backend_generation();
        fc.clear_backend_runtime().await;
        let mut called = false;
        let result = resource
            .update_at(&fc, old_generation, "mutation", None, || async {
                called = true;
                Ok("old")
            })
            .await;
        assert!(result.is_err());
        assert!(!called);
    }

    #[tokio::test]
    async fn private_resources_do_not_publish_public_events() {
        let fc = connection();
        let mut events = fc.data_event_tx.subscribe();
        let resource = FeatureResource::private("options");
        assert_eq!(
            resource
                .commit(&fc, "provider", "refresh", None)
                .await
                .unwrap()
                .revision,
            1
        );
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn commit_increments_revision_and_publishes_snapshot() {
        let fc = connection();
        let mut events = fc.data_event_tx.subscribe();
        let resource = FeatureResource::new("status");
        let first = resource
            .commit(&fc, "on", "mutation", Some("m1".into()))
            .await
            .unwrap();
        assert_eq!(first.revision, 1);
        let event = events.recv().await.unwrap();
        assert!(matches!(
            event,
            ConnectionEvent::DataChanged { revision: 1, resource, .. } if resource == "status"
        ));
    }

    #[tokio::test]
    async fn identical_commit_does_not_advance_revision_or_publish() {
        let fc = connection();
        let mut events = fc.data_event_tx.subscribe();
        let resource = FeatureResource::new("status");
        resource.commit(&fc, "on", "refresh", None).await.unwrap();
        assert!(matches!(
            events.recv().await.unwrap(),
            ConnectionEvent::DataChanged { revision: 1, .. }
        ));

        let snapshot = resource
            .commit(&fc, "on", "mutation", Some("m2".into()))
            .await
            .unwrap();
        assert_eq!(snapshot.revision, 1);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), events.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn refresh_started_before_mutation_cannot_overwrite_mutation() {
        use tokio::sync::Notify;

        let fc = connection();
        let resource = Arc::new(FeatureResource::new("status"));
        let loader_started = Arc::new(Notify::new());
        let release_loader = Arc::new(Notify::new());

        let refresh = {
            let resource = resource.clone();
            let fc = fc.clone();
            let loader_started = loader_started.clone();
            let release_loader = release_loader.clone();
            tokio::spawn(async move {
                resource
                    .refresh(&fc, || async move {
                        loader_started.notify_one();
                        release_loader.notified().await;
                        Ok("old")
                    })
                    .await
                    .unwrap()
            })
        };
        loader_started.notified().await;

        let mutation = {
            let resource = resource.clone();
            let fc = fc.clone();
            tokio::spawn(async move {
                resource
                    .mutate(&fc, Some("m1".into()), || async { Ok("new") })
                    .await
                    .unwrap()
            })
        };
        tokio::task::yield_now().await;
        release_loader.notify_one();

        assert_eq!(refresh.await.unwrap().value, Some("old"));
        assert_eq!(mutation.await.unwrap().value, Some("new"));
        assert_eq!(resource.snapshot().await.value, Some("new"));
    }

    #[tokio::test]
    async fn concurrent_refreshes_share_the_first_result() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let fc = connection();
        let resource = Arc::new(FeatureResource::new("status"));
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(tokio::sync::Barrier::new(3));
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let fc = fc.clone();
            let resource = resource.clone();
            let calls = calls.clone();
            let start = start.clone();
            tasks.push(tokio::spawn(async move {
                start.wait().await;
                resource
                    .refresh(&fc, || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        Ok("shared")
                    })
                    .await
                    .unwrap()
            }));
        }
        start.wait().await;
        for task in tasks {
            assert_eq!(task.await.unwrap().value, Some("shared"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_unchanged_refreshes_still_run_one_loader() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let fc = connection();
        let resource = Arc::new(FeatureResource::new("status"));
        resource.commit(&fc, "same", "seed", None).await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(tokio::sync::Barrier::new(3));
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let fc = fc.clone();
            let resource = resource.clone();
            let calls = calls.clone();
            let start = start.clone();
            tasks.push(tokio::spawn(async move {
                start.wait().await;
                resource
                    .refresh(&fc, || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        Ok("same")
                    })
                    .await
                    .unwrap()
            }));
        }
        start.wait().await;
        for task in tasks {
            assert_eq!(task.await.unwrap().value, Some("same"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(resource.snapshot().await.revision, 1);
    }

    #[tokio::test]
    async fn concurrent_failed_refreshes_share_one_loader_attempt() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let fc = connection();
        let resource = Arc::new(FeatureResource::<String>::new("status"));
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(tokio::sync::Barrier::new(3));
        let release_loader = Arc::new(tokio::sync::Notify::new());
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let fc = fc.clone();
            let resource = resource.clone();
            let calls = calls.clone();
            let start = start.clone();
            let release_loader = release_loader.clone();
            tasks.push(tokio::spawn(async move {
                start.wait().await;
                resource
                    .refresh(&fc, || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        release_loader.notified().await;
                        Err("provider unavailable".to_string())
                    })
                    .await
            }));
        }
        start.wait().await;
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        release_loader.notify_waiters();

        let results = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            futures::future::join_all(tasks),
        )
        .await
        .expect("queued refreshes must complete after the shared failure");
        assert!(results
            .into_iter()
            .all(|result| { result.expect("refresh task").is_err() }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn data_broadcast_preserves_rapid_revisions() {
        let fc = connection();
        let mut events = fc.data_event_tx.subscribe();
        let resource = FeatureResource::new("status");
        for value in 1..=3 {
            resource
                .commit(&fc, value, "mutation", Some(format!("m{value}")))
                .await
                .unwrap();
        }
        for revision in 1..=3 {
            assert!(matches!(
                events.recv().await.unwrap(),
                ConnectionEvent::DataChanged { revision: actual, .. } if actual == revision
            ));
        }
    }

    #[tokio::test]
    async fn stopped_connection_rejects_resource_operations_before_dispatch() {
        let fc = connection();
        let resource = FeatureResource::new("status");
        fc.shutdown();
        let dispatched = std::sync::atomic::AtomicUsize::new(0);
        let operation = || async {
            dispatched.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, String>("unexpected")
        };

        assert!(resource.refresh(&fc, operation).await.is_err());
        assert!(resource
            .update(&fc, "mutation", None, operation)
            .await
            .is_err());
        assert!(resource
            .try_mutate::<String, _, _>(&fc, None, operation)
            .await
            .is_err());
        assert_eq!(dispatched.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(resource.snapshot().await.revision, 0);
    }

    #[tokio::test]
    async fn queued_resource_operation_rechecks_shutdown_after_admission() {
        let fc = connection();
        let resource = Arc::new(FeatureResource::new("status"));
        let admission = resource.operation.lock().await;
        let dispatched = std::sync::atomic::AtomicBool::new(false);
        let operation = resource.update(&fc, "mutation", None, || async {
            dispatched.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("unexpected")
        });
        tokio::pin!(operation);
        assert!(futures::poll!(&mut operation).is_pending());
        fc.shutdown();
        drop(admission);
        assert!(operation.await.is_err());
        assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stopped_connection_cannot_publish_a_late_revision() {
        let fc = connection();
        let mut events = fc.data_event_tx.subscribe();
        let resource = FeatureResource::new("status");
        fc.shutdown();

        let error = resource
            .commit(&fc, "late", "mutation", Some("late-mutation".into()))
            .await
            .unwrap_err();

        assert!(error.contains("replaced"));
        assert_eq!(resource.snapshot().await.revision, 0);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), events.recv())
                .await
                .is_err()
        );
    }
}
