use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::{broadcast, watch, Notify, RwLock};
use tracing::{info, warn};

use crate::bmc::feature::config::FeatureConfig;
use crate::bmc::feature::fault::{lifecycle_connect_fault, FeatureFault, FeatureFaultStore};

pub type FeatureConnectFn =
    Arc<dyn Fn(Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;
pub type LifecycleFinishedFn = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

static NEXT_DATA_EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
}

#[derive(Clone, Debug)]
pub enum ConnectionEvent {
    StateChanged {
        feature: String,
        instance: String,
        state: ConnectionState,
    },
    StageChanged {
        feature: String,
        instance: String,
        stage: String,
    },
    Health {
        feature: String,
        instance: String,
        online: bool,
        fps: u32,
        width: u16,
        height: u16,
    },
    FaultChanged {
        feature: String,
        instance: String,
        fault: Option<FeatureFault>,
    },
    StreamStateChanged {
        feature: String,
        instance: String,
        state: String,
        reason: Option<String>,
    },
    DataChanged {
        feature: String,
        instance: String,
        epoch: String,
        resource: String,
        revision: u64,
        cause: String,
        mutation_id: Option<String>,
        data: serde_json::Value,
    },
    DataResyncRequired {
        feature: String,
        instance: String,
        epoch: String,
        resource: Option<String>,
        skipped: u64,
    },
}

/// Per-device, per-feature connection handle.
pub struct FeatureConnection {
    pub id: String,
    pub instance_id: String,
    /// Unique for the lifetime of this connection. Resource revisions are
    /// ordered only within this epoch and restart at zero after replacement.
    pub data_epoch: String,
    pub device_id: String,
    pub backend: String,
    pub config: FeatureConfig,
    pub state: RwLock<ConnectionState>,
    pub ref_count: AtomicU32,
    pub event_tx: watch::Sender<Option<ConnectionEvent>>,
    pub event_rx: watch::Receiver<Option<ConnectionEvent>>,
    /// Loss-aware stream for revisioned feature data. Connection state uses
    /// `watch` because only its latest value matters; data mutations must not
    /// be silently coalesced.
    pub data_event_tx: broadcast::Sender<ConnectionEvent>,
    pub wake: Notify,
    pub healthy: AtomicBool,
    pub faults: FeatureFaultStore,
    pub lifecycle_started: AtomicBool,
    /// Set true by backend's disconnect monitor when the underlying transport
    /// (e.g. pool TCP connection) drops.  Lifecycle reads this flag in the
    /// CONNECTED loop and transitions to DISCONNECT immediately (instead of
    /// waiting for idle timeout).
    pub disconnected: AtomicBool,
    /// Set by registry replacement/shutdown. A stopped connection must never
    /// reconnect, even if a caller still owns a guard from the old registry.
    stopped: AtomicBool,
    stop_tx: watch::Sender<bool>,
    /// Backend runtime state created by connect() and cleared on disconnect.
    /// Pin callback-producing operations without holding the publication lock.
    /// Replacement takes this write lock before `backend_effect` so callbacks
    /// can still publish while an admitted operation finishes.
    backend_operation: RwLock<()>,
    backend_effect: Arc<tokio::sync::Mutex<()>>,
    backend_generation: AtomicU64,
    backend_generation_tx: watch::Sender<u64>,
    backend_runtime: RwLock<Option<Box<dyn std::any::Any + Send + Sync>>>,
    /// Feature-owned runtime state that survives backend reconnects.
    feature_runtime: RwLock<Option<Box<dyn std::any::Any + Send + Sync>>>,
}

impl FeatureConnection {
    pub fn new(device_id: String, id: String, backend: String, config: FeatureConfig) -> Arc<Self> {
        Self::new_instance(device_id, id, "default".to_string(), backend, config)
    }

    pub fn new_instance(
        device_id: String,
        id: String,
        instance_id: String,
        backend: String,
        config: FeatureConfig,
    ) -> Arc<Self> {
        let (event_tx, event_rx) = watch::channel(None);
        let (data_event_tx, _) = broadcast::channel(256);
        let (stop_tx, _) = watch::channel(false);
        let fc = Arc::new(Self {
            id,
            instance_id,
            data_epoch: NEXT_DATA_EPOCH.fetch_add(1, Ordering::SeqCst).to_string(),
            device_id,
            backend,
            config,
            state: RwLock::new(ConnectionState::Disconnected),
            ref_count: AtomicU32::new(0),
            event_tx,
            event_rx,
            data_event_tx,
            wake: Notify::new(),
            healthy: AtomicBool::new(false),
            faults: FeatureFaultStore::default(),
            lifecycle_started: AtomicBool::new(false),
            disconnected: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            stop_tx,
            backend_operation: RwLock::new(()),
            backend_effect: Arc::new(tokio::sync::Mutex::new(())),
            backend_generation: AtomicU64::new(0),
            backend_generation_tx: watch::channel(0).0,
            backend_runtime: RwLock::new(None),
            feature_runtime: RwLock::new(None),
        });
        let weak = Arc::downgrade(&fc);
        fc.faults.set_notifier(Arc::new(move |fault| {
            if let Some(fc) = weak.upgrade() {
                let _ = fc.event_tx.send(Some(ConnectionEvent::FaultChanged {
                    feature: fc.id.clone(),
                    instance: fc.instance_id.clone(),
                    fault,
                }));
            }
        }));
        fc
    }

    pub async fn transition(&self, new: ConnectionState) {
        let mut state = self.state.write().await;
        *state = new.clone();
        let _ = self.event_tx.send(Some(ConnectionEvent::StateChanged {
            feature: self.id.clone(),
            instance: self.instance_id.clone(),
            state: new,
        }));
    }

    pub fn ref_count(&self) -> u32 {
        self.ref_count.load(Ordering::SeqCst)
    }

    pub async fn set_backend_runtime<T>(&self, value: T)
    where
        T: std::any::Any + Send + Sync + 'static,
    {
        let _operation = self.backend_operation.write().await;
        let _effect = self.backend_effect.lock().await;
        let mut runtime = self.backend_runtime.write().await;
        if runtime.is_some() {
            let generation = self.backend_generation.fetch_add(1, Ordering::SeqCst) + 1;
            self.backend_generation_tx.send_replace(generation);
        }
        runtime.replace(Box::new(value));
    }

    pub async fn backend_runtime<T>(&self) -> Option<T>
    where
        T: std::any::Any + Clone + Send + Sync + 'static,
    {
        self.backend_runtime
            .read()
            .await
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .cloned()
    }

    pub async fn clear_backend_runtime(&self) {
        let _operation = self.backend_operation.write().await;
        let _effect = self.backend_effect.lock().await;
        let mut runtime = self.backend_runtime.write().await;
        let generation = self.backend_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.backend_generation_tx.send_replace(generation);
        *runtime = None;
    }

    pub fn backend_generation(&self) -> u64 {
        self.backend_generation.load(Ordering::SeqCst)
    }

    pub(crate) fn backend_generation_rx(&self) -> watch::Receiver<u64> {
        self.backend_generation_tx.subscribe()
    }

    /// Hold through dispatch, backend callbacks, readback and publication.
    /// Callers serialize their own mutations and must not hold `backend_effect`
    /// when acquiring this lease. Shutdown rejects new leases; admitted remote
    /// work can finish, while stopped connections still reject data commits.
    pub(crate) async fn backend_operation_guard(
        &self,
        generation: u64,
    ) -> Result<tokio::sync::RwLockReadGuard<'_, ()>, String> {
        let guard = self.backend_operation.read().await;
        if self.is_stopped() || self.backend_generation() != generation {
            return Err("backend was replaced before operation".to_owned());
        }
        Ok(guard)
    }

    /// Return a commit lease only for the current backend's terminal observation.
    /// Replacement/stop releases silent old observers without polling.
    pub(crate) async fn wait_for_backend_disconnect(
        &self,
        generation: u64,
        mut disconnected: watch::Receiver<bool>,
    ) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        let mut changes = self.backend_generation_rx();
        let mut stopped = self.stopped_rx();
        loop {
            if self.is_stopped() || self.backend_generation() != generation {
                return None;
            }
            if *disconnected.borrow() {
                return self.backend_effect_guard(generation).await.ok();
            }
            tokio::select! {
                biased;
                _ = stopped.changed() => return None,
                _ = changes.changed() => continue,
                result = disconnected.changed() => {
                    if result.is_err() {
                        return self.backend_effect_guard(generation).await.ok();
                    }
                }
            }
        }
    }

    /// Pool observations belong to one runtime generation, even when another
    /// feature keeps the shared transport and its watch sender alive.
    pub(crate) async fn wait_for_backend_update<T>(
        &self,
        generation: u64,
        updates: &mut watch::Receiver<T>,
    ) -> bool {
        let mut generations = self.backend_generation_rx();
        let mut stopped = self.stopped_rx();
        if self.is_stopped() || self.backend_generation() != generation {
            return false;
        }
        tokio::select! {
            biased;
            _ = stopped.changed() => false,
            _ = generations.changed() => false,
            result = updates.changed() => result.is_ok()
                && !self.is_stopped() && self.backend_generation() == generation,
        }
    }

    pub(crate) async fn backend_effect_guard(
        &self,
        generation: u64,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>, String> {
        let guard = self.backend_effect.lock().await;
        if self.is_stopped() || self.backend_generation() != generation {
            return Err("backend was replaced before effect".to_owned());
        }
        Ok(guard)
    }

    pub(crate) async fn backend_owned_effect_guard(
        &self,
        generation: u64,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
        let guard = self.backend_effect.clone().lock_owned().await;
        if self.is_stopped() || self.backend_generation() != generation {
            return Err("backend was replaced before effect".to_owned());
        }
        Ok(guard)
    }

    pub(crate) async fn backend_commit_guard(
        &self,
        generation: u64,
    ) -> Result<impl Send + '_, String> {
        let guard = self.backend_runtime.read().await;
        if self.is_stopped() || self.backend_generation() != generation {
            return Err("backend was replaced before data commit".to_owned());
        }
        Ok(guard)
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.stop_tx.send_replace(true);
        self.wake.notify_waiters();
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub fn stopped_rx(&self) -> watch::Receiver<bool> {
        self.stop_tx.subscribe()
    }

    pub async fn set_feature_runtime<T>(&self, value: T)
    where
        T: std::any::Any + Send + Sync + 'static,
    {
        self.feature_runtime.write().await.replace(Box::new(value));
    }

    pub async fn feature_runtime<T>(&self) -> Option<T>
    where
        T: std::any::Any + Clone + Send + Sync + 'static,
    {
        self.feature_runtime
            .read()
            .await
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .cloned()
    }

    pub async fn get_or_init_feature_runtime<T, F>(&self, init: F) -> T
    where
        T: std::any::Any + Clone + Send + Sync + 'static,
        F: FnOnce() -> T,
    {
        let mut slot = self.feature_runtime.write().await;
        if let Some(value) = slot.as_ref().and_then(|value| value.downcast_ref::<T>()) {
            return value.clone();
        }
        let value = init();
        slot.replace(Box::new(value.clone()));
        value
    }
}

/// RAII guard that keeps a FeatureConnection alive.
pub struct FeatureGuard {
    fc: Arc<FeatureConnection>,
}

impl FeatureGuard {
    pub fn new(fc: Arc<FeatureConnection>) -> Self {
        fc.ref_count.fetch_add(1, Ordering::SeqCst);
        fc.wake.notify_one();
        Self { fc }
    }

    pub fn fc(&self) -> &Arc<FeatureConnection> {
        &self.fc
    }
}

impl Clone for FeatureGuard {
    fn clone(&self) -> Self {
        self.fc.ref_count.fetch_add(1, Ordering::SeqCst);
        self.fc.wake.notify_one();
        Self {
            fc: self.fc.clone(),
        }
    }
}

impl Drop for FeatureGuard {
    fn drop(&mut self) {
        self.fc.ref_count.fetch_sub(1, Ordering::SeqCst);
        self.fc.wake.notify_one();
    }
}

/// Spawn a per-FC lifecycle task.
/// State machine:
///   WAITING → (ref_count>0) → CONNECTING → (ok) → CONNECTED
///     → (ref_count==0 + idle_timeout) → DISCONNECT → WAITING
pub fn spawn_lifecycle(fc: Arc<FeatureConnection>, connect: FeatureConnectFn) {
    spawn_lifecycle_inner(fc, connect, Duration::from_secs(30), None);
}

pub fn spawn_ephemeral_lifecycle(
    fc: Arc<FeatureConnection>,
    connect: FeatureConnectFn,
    on_finished: LifecycleFinishedFn,
) {
    spawn_lifecycle_inner(fc, connect, Duration::from_secs(30), Some(on_finished));
}

#[cfg(test)]
pub fn spawn_ephemeral_lifecycle_with_idle_timeout(
    fc: Arc<FeatureConnection>,
    connect: FeatureConnectFn,
    idle_timeout: Duration,
    on_finished: LifecycleFinishedFn,
) {
    spawn_lifecycle_inner(fc, connect, idle_timeout, Some(on_finished));
}

fn spawn_lifecycle_inner(
    fc: Arc<FeatureConnection>,
    connect: FeatureConnectFn,
    idle_timeout: Duration,
    on_finished: Option<LifecycleFinishedFn>,
) {
    fc.lifecycle_started.store(true, Ordering::SeqCst);
    tokio::spawn(async move {
        let mut retry_attempts = 0u32;

        'lifecycle: loop {
            if fc.is_stopped() {
                break;
            }
            // WAITING
            fc.transition(ConnectionState::Disconnected).await;
            let unused_deadline = tokio::time::Instant::now() + idle_timeout;
            while fc.ref_count() == 0 && !fc.is_stopped() {
                if on_finished.is_some() {
                    tokio::select! {
                        _ = fc.wake.notified() => {},
                        _ = tokio::time::sleep_until(unused_deadline) => {
                            if fc.ref_count() == 0 { break 'lifecycle; }
                        }
                    }
                } else {
                    fc.wake.notified().await;
                }
            }
            if fc.is_stopped() {
                break;
            }

            // CONNECTING
            fc.transition(ConnectionState::Connecting).await;
            fc.disconnected.store(false, Ordering::SeqCst);
            fc.clear_backend_runtime().await;
            let mut stopped = fc.stopped_rx();
            let connect_result = tokio::select! {
                result = (connect)(fc.clone()) => Some(result),
                changed = stopped.changed() => {
                    if changed.is_err() || *stopped.borrow() { None } else { continue }
                }
            };
            let Some(connect_result) = connect_result else {
                fc.clear_backend_runtime().await;
                fc.transition(ConnectionState::Disconnected).await;
                break;
            };
            match connect_result {
                Ok(()) => {
                    retry_attempts = 0;
                    fc.faults.clear_active().await;
                    fc.transition(ConnectionState::Connected).await;
                    fc.healthy.store(true, Ordering::SeqCst);
                }
                Err(e) => {
                    warn!("[{}:{}] connect failed: {}", fc.device_id, fc.id, e);
                    fc.faults.record(lifecycle_connect_fault(&e)).await;
                    fc.healthy.store(false, Ordering::SeqCst);
                    let reconnect = &fc.config.feature_params.reconnect;
                    let max_attempts = reconnect.max_attempts;
                    retry_attempts += 1;
                    if max_attempts != 0 && retry_attempts >= max_attempts {
                        info!(
                            "[{}:{}] reconnect attempt limit reached; waiting for an explicit retry",
                            fc.device_id, fc.id
                        );
                        fc.clear_backend_runtime().await;
                        fc.transition(ConnectionState::Disconnected).await;
                        // Keep the lifecycle alive so `POST .../retry` (or a
                        // later consumer acquiring this feature) can wake it
                        // without reconstructing a registry connection.
                        retry_attempts = 0;
                        fc.wake.notified().await;
                        continue;
                    }
                    let multiplier = 1u64 << retry_attempts.saturating_sub(1).min(63);
                    let delay = Duration::from_secs(
                        (reconnect.base_delay_s.max(1) as u64)
                            .saturating_mul(multiplier)
                            .min(reconnect.max_delay_s.max(1) as u64),
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = fc.wake.notified() => {}
                    }
                    if fc.is_stopped() {
                        fc.clear_backend_runtime().await;
                        fc.transition(ConnectionState::Disconnected).await;
                        break;
                    }
                    continue;
                }
            }

            // CONNECTED — health-check loop
            let mut idle_start: Option<tokio::time::Instant> = None;
            loop {
                if fc.is_stopped() || fc.disconnected.load(Ordering::SeqCst) {
                    info!(
                        "[{}:{}] backend disconnected, tearing down",
                        fc.device_id, fc.id
                    );
                    break;
                }

                if fc.ref_count() == 0 {
                    match idle_start {
                        None => idle_start = Some(tokio::time::Instant::now()),
                        Some(start) if start.elapsed() >= idle_timeout => {
                            info!("[{}:{}] idle timeout", fc.device_id, fc.id);
                            break;
                        }
                        _ => {}
                    }
                } else {
                    idle_start = None;
                }

                let tick = std::cmp::min(Duration::from_secs(1), idle_timeout);
                tokio::select! {
                    _ = tokio::time::sleep(tick) => {}
                    _ = fc.wake.notified() => {}
                }
            }

            // DISCONNECT
            fc.transition(ConnectionState::Disconnecting).await;
            fc.healthy.store(false, Ordering::SeqCst);
            fc.clear_backend_runtime().await;
            fc.transition(ConnectionState::Disconnected).await;

            if fc.is_stopped() {
                break;
            }

            if on_finished.is_some() {
                break;
            }
        }
        if let Some(on_finished) = on_finished {
            fc.shutdown();
            fc.clear_backend_runtime().await;
            fc.healthy.store(false, Ordering::SeqCst);
            on_finished().await;
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use futures::future::BoxFuture;

    use super::{spawn_lifecycle, ConnectionState, FeatureConnection, FeatureGuard};
    use crate::bmc::feature::config::{BackendParams, FeatureConfig, FeatureParams};

    #[tokio::test]
    async fn disconnect_observers_accept_terminal_values_and_retire_on_replacement() {
        let fc = FeatureConnection::new(
            "device".into(),
            "video".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        fc.set_backend_runtime(Arc::new(())).await;
        let generation = fc.backend_generation();
        let (_closed, terminal) = tokio::sync::watch::channel(true);
        assert!(fc
            .wait_for_backend_disconnect(generation, terminal)
            .await
            .is_some());
        let (old_signal, pending) = tokio::sync::watch::channel(false);
        let observer = tokio::spawn({
            let fc = fc.clone();
            async move {
                fc.wait_for_backend_disconnect(generation, pending)
                    .await
                    .is_some()
            }
        });
        tokio::task::yield_now().await;
        fc.clear_backend_runtime().await;
        assert!(!tokio::time::timeout(Duration::from_millis(100), observer)
            .await
            .unwrap()
            .unwrap());
        old_signal.send_replace(true);
        assert!(fc
            .wait_for_backend_disconnect(generation, old_signal.subscribe())
            .await
            .is_none());
        let (_open, pending) = tokio::sync::watch::channel(false);
        let observer = tokio::spawn({
            let fc = fc.clone();
            async move {
                fc.wait_for_backend_disconnect(fc.backend_generation(), pending)
                    .await
                    .is_some()
            }
        });
        tokio::task::yield_now().await;
        fc.shutdown();
        assert!(!tokio::time::timeout(Duration::from_millis(100), observer)
            .await
            .unwrap()
            .unwrap());
    }

    #[tokio::test]
    async fn backend_update_observers_accept_live_changes_and_reject_stale_or_closed_sources() {
        for ending in 0..4 {
            let fc = FeatureConnection::new(
                "device".into(),
                "msd".into(),
                "test".into(),
                FeatureConfig {
                    backend: "test".into(),
                    backend_params: Default::default(),
                    feature_params: Default::default(),
                    providers: vec![],
                },
            );
            let generation = fc.backend_generation();
            let (sender, mut updates) = tokio::sync::watch::channel(0u64);
            sender.send_replace(1);
            assert!(fc.wait_for_backend_update(generation, &mut updates).await);
            let waiting = fc.wait_for_backend_update(generation, &mut updates);
            tokio::pin!(waiting);
            assert!(futures::poll!(&mut waiting).is_pending());
            match ending {
                0 => fc.shutdown(),
                1 | 2 => {
                    fc.clear_backend_runtime().await;
                    if ending == 2 {
                        sender.send_replace(2);
                    }
                }
                _ => drop(sender),
            }
            assert!(!tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .unwrap());
        }
    }

    async fn wait_connected(fc: &Arc<FeatureConnection>) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if *fc.state.read().await == ConnectionState::Connected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("feature did not connect");
    }

    #[tokio::test]
    async fn never_acquired_ephemeral_instance_runs_terminal_cleanup_once() {
        let fc = FeatureConnection::new(
            "dev".into(),
            "video".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: Vec::new(),
            },
        );
        let finished = Arc::new(tokio::sync::Notify::new());
        let callbacks = Arc::new(AtomicUsize::new(0));
        super::spawn_ephemeral_lifecycle_with_idle_timeout(
            fc.clone(),
            Arc::new(|_| Box::pin(async { panic!("unused instance must not connect") })),
            Duration::from_millis(10),
            {
                let finished = finished.clone();
                let callbacks = callbacks.clone();
                Arc::new(move || {
                    let finished = finished.clone();
                    let callbacks = callbacks.clone();
                    Box::pin(async move {
                        callbacks.fetch_add(1, Ordering::SeqCst);
                        finished.notify_one();
                    })
                })
            },
        );
        tokio::time::timeout(Duration::from_secs(2), finished.notified())
            .await
            .unwrap();
        assert!(fc.is_stopped());
        assert!(!fc.healthy.load(Ordering::SeqCst));
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn guard_wakes_lifecycle_and_drops_ref_count() {
        let fc = FeatureConnection::new(
            "dev1".to_string(),
            "hid".to_string(),
            "test".to_string(),
            FeatureConfig {
                backend: "test".to_string(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        );
        let connects = Arc::new(AtomicUsize::new(0));
        let connects2 = connects.clone();
        spawn_lifecycle(
            fc.clone(),
            Arc::new(move |_fc| {
                let connects = connects2.clone();
                Box::pin(async move {
                    connects.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }) as BoxFuture<'static, Result<(), String>>
            }),
        );

        let guard = FeatureGuard::new(fc.clone());
        wait_connected(&fc).await;

        assert_eq!(connects.load(Ordering::SeqCst), 1);
        assert_eq!(fc.ref_count(), 1);

        drop(guard);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(fc.ref_count(), 0);
    }

    #[tokio::test]
    async fn successful_reconnect_clears_the_previous_lifecycle_fault() {
        let fc = FeatureConnection::new(
            "dev1".to_string(),
            "video".to_string(),
            "test".to_string(),
            FeatureConfig {
                backend: "test".to_string(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        );
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_connect = Arc::clone(&attempts);
        spawn_lifecycle(
            Arc::clone(&fc),
            Arc::new(move |_fc| {
                let attempts = Arc::clone(&attempts_for_connect);
                Box::pin(async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err("iDRAC AVCT login is already in use".to_string())
                    } else {
                        Ok(())
                    }
                }) as BoxFuture<'static, Result<(), String>>
            }),
        );

        let _guard = FeatureGuard::new(Arc::clone(&fc));
        wait_connected(&fc).await;

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(fc.faults.active().await.is_none());
    }

    #[tokio::test]
    async fn shutdown_stops_lifecycle_even_while_a_guard_is_held() {
        let fc = FeatureConnection::new(
            "dev1".to_string(),
            "video".to_string(),
            "test".to_string(),
            FeatureConfig {
                backend: "test".to_string(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        );
        let connects = Arc::new(AtomicUsize::new(0));
        let connects_for_task = connects.clone();
        spawn_lifecycle(
            fc.clone(),
            Arc::new(move |_fc| {
                let connects = connects_for_task.clone();
                Box::pin(async move {
                    connects.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }) as BoxFuture<'static, Result<(), String>>
            }),
        );

        let _guard = FeatureGuard::new(fc.clone());
        wait_connected(&fc).await;
        fc.shutdown();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while fc.healthy.load(Ordering::SeqCst) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!fc.healthy.load(Ordering::SeqCst));
        assert!(fc.is_stopped());
        assert_eq!(connects.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn shutdown_cancels_an_inflight_connect() {
        let fc = FeatureConnection::new(
            "dev1".into(),
            "video".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        );
        let entered = Arc::new(tokio::sync::Notify::new());
        let entered_connect = entered.clone();
        spawn_lifecycle(
            fc.clone(),
            Arc::new(move |_| {
                let entered = entered_connect.clone();
                Box::pin(async move {
                    entered.notify_one();
                    std::future::pending::<Result<(), String>>().await
                })
            }),
        );
        let _guard = FeatureGuard::new(fc.clone());
        entered.notified().await;
        fc.shutdown();
        tokio::time::timeout(Duration::from_millis(250), async {
            loop {
                if *fc.state.read().await == ConnectionState::Disconnected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shutdown must cancel connect promptly");
    }

    #[test]
    fn replacement_data_epochs_are_strictly_monotonic() {
        let config = FeatureConfig {
            backend: "test".to_string(),
            backend_params: BackendParams::default(),
            feature_params: FeatureParams::default(),
            providers: Vec::new(),
        };
        let first = FeatureConnection::new(
            "dev1".to_string(),
            "power".to_string(),
            "test".to_string(),
            config.clone(),
        );
        let replacement = FeatureConnection::new(
            "dev1".to_string(),
            "power".to_string(),
            "test".to_string(),
            config,
        );

        assert!(
            first.data_epoch.parse::<u64>().unwrap()
                < replacement.data_epoch.parse::<u64>().unwrap()
        );
    }
}
