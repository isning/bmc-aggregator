use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures::future::{BoxFuture, Fuse};
use futures::FutureExt;
use tokio::sync::{RwLock, watch};
use tracing::{info, warn, error};

use super::event::ConnectionEvent;
use super::feature::FeatureConnection;
use super::progress::ConnectProgress;
use super::registry::ConnectionTemplate;
use super::state::{ConnectionState, ConnectionStage};
use crate::bmc::encoder::backend::SharedEncoder;
use crate::bmc::session::types::DeviceSession;
use crate::config::DeviceConfig;

/// Backend-internal transport trait.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    fn id(&self) -> &str;
    fn features(&self) -> &[&str];
    async fn connect(&self, cfg: &DeviceConfig, encoder: SharedEncoder, progress: &ConnectProgress)
        -> Result<DeviceSession, anyhow::Error>;
    fn on_disconnect(&self) -> Option<watch::Receiver<bool>>;
}

/// Wraps a ConnectionTemplate's connect fn pointer into a Transport.
pub(crate) struct TransportAdapter {
    id: String,
    feature_ids: Vec<&'static str>,
    connect_fn: fn(
        &DeviceConfig,
        SharedEncoder,
        &ConnectProgress,
    ) -> BoxFuture<'static, Result<DeviceSession, anyhow::Error>>,
}

impl TransportAdapter {
    pub fn new(ct: &'static ConnectionTemplate) -> Self {
        TransportAdapter {
            id: ct.id.to_string(),
            feature_ids: ct.features.to_vec(),
            connect_fn: ct.connect,
        }
    }
}

#[async_trait::async_trait]
impl Transport for TransportAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn features(&self) -> &[&str] {
        &self.feature_ids
    }

    async fn connect(
        &self,
        cfg: &DeviceConfig,
        encoder: SharedEncoder,
        progress: &ConnectProgress,
    ) -> Result<DeviceSession, anyhow::Error> {
        (self.connect_fn)(cfg, encoder, progress).await
    }

    fn on_disconnect(&self) -> Option<watch::Receiver<bool>> {
        None
    }
}

/// Runtime state for a transport instance.
pub struct TransportState {
    pub id: String,
    pub transport: Arc<dyn Transport>,
    pub session: RwLock<Option<DeviceSession>>,
    pub ref_count: AtomicI64,
    pub wake_tx: watch::Sender<u64>,
    pub connected: watch::Sender<bool>,
    pub idle_timeout: Duration,
}

impl TransportState {
    pub fn new(transport: Arc<dyn Transport>) -> Arc<Self> {
        let (wake_tx, _) = watch::channel(0u64);
        let (connected, _) = watch::channel(false);
        Arc::new(TransportState {
            id: transport.id().to_string(),
            transport,
            session: RwLock::new(None),
            ref_count: AtomicI64::new(0),
            wake_tx,
            connected,
            idle_timeout: Duration::from_secs(10),
        })
    }

    pub async fn wait_for_session(&self, timeout: Duration) -> Result<DeviceSession, String> {
        let mut rx = self.connected.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if *rx.borrow() {
                let guard = self.session.read().await;
                if let Some(ref s) = *guard {
                    return Ok(s.clone());
                }
            }
            tokio::select! {
                _ = rx.changed() => {}
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
        Err("connection timeout".to_string())
    }
}

/// Background task per transport: state machine loop.
pub async fn connect_task(
    ts: Arc<TransportState>,
    features: Vec<Arc<FeatureConnection>>,
    cfg: Arc<DeviceConfig>,
    encoder: SharedEncoder,
) {
    let mut wake_rx = ts.wake_tx.subscribe();
    let mut retry_delay = INITIAL_RETRY_DELAY;
    let mut disconnect_fut: Fuse<BoxFuture<'static, ()>> = futures::future::pending().boxed().fuse();

    loop {
        // ── WAITING ──
        set_features_state(&features, ConnectionState::Disconnected).await;
        while !any_feature_in_use(&features) {
            if wake_rx.changed().await.is_err() {
                return;
            }
            retry_delay = INITIAL_RETRY_DELAY;
        }

        // ── CONNECTING ──
        set_features_state(&features, ConnectionState::Connecting).await;
        loop {
            if !any_feature_in_use(&features) {
                break;
            }

            let (progress, mut progress_rx) = ConnectProgress::new();
            let connect = ts.transport.connect(&cfg, encoder.clone(), &progress);
            tokio::pin!(connect);

            let mut woke = false;
            let result = loop {
                tokio::select! {
                    result = &mut connect => break Ok(result),
                    stage = progress_rx.recv() => {
                        if let Some(stage) = stage {
                            let event = ConnectionEvent::StageChanged {
                                feature: ts.id.clone(),
                                stage,
                            };
                            for fc in &features {
                                fc.publish(event.clone());
                            }
                        }
                    }
                    _ = wake_rx.changed() => {
                        woke = true;
                        break Err(anyhow::anyhow!("woken during connect"));
                    }
                }
            };

            match result {
                Ok(Ok(session)) => {
                    retry_delay = INITIAL_RETRY_DELAY;
                    *ts.session.write().await = Some(session);
                    ts.connected.send_replace(true);
                    set_features_state(&features, ConnectionState::Connected).await;

                    disconnect_fut = ts.transport.on_disconnect()
                        .map(|rx| {
                            let mut rx2 = rx.clone();
                            async move { rx2.changed().await.ok(); }.boxed().fuse()
                        })
                        .unwrap_or_else(|| futures::future::pending().boxed().fuse());
                    break;
                }
                _ => {
                    if let Ok(Err(e)) = result {
                        error!("[{}] transport '{}' connection failed: {}",
                            cfg.id, ts.id, e);
                        let event = ConnectionEvent::StageChanged {
                            feature: ts.id.clone(),
                            stage: ConnectionStage::Failed(e.to_string()),
                        };
                        for fc in &features {
                            fc.publish(event.clone());
                        }
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(retry_delay) => {}
                        _ = wake_rx.changed() => { woke = true; }
                    }
                    retry_delay = compute_retry_delay(retry_delay, woke);
                }
            }
        }

        // ── CONNECTED / HEALTH-CHECK ──
        let mut health_ticks: u64 = 0;
        loop {
            let active = any_feature_in_use(&features);
            if active {
                health_ticks = 0;
            }
            if !active && health_ticks > 0 && health_ticks >= ts.idle_timeout.as_secs() as u64 {
                info!("[{}] transport '{}' idle timeout, disconnecting",
                    cfg.id, ts.id);
                break;
            }

            tokio::select! {
                _ = wake_rx.changed() => {}
                _ = &mut disconnect_fut => {
                    warn!("[{}] transport '{}' disconnected", cfg.id, ts.id);
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    health_ticks += 1;
                }
            }
        }

        // ── DISCONNECT ──
        set_features_state(&features, ConnectionState::Disconnecting).await;
        let old_session = ts.session.write().await.take();
        ts.connected.send_replace(false);
        if let Some(s) = old_session {
            if let Some(ref base) = s.base {
                let _ = base.close().await;
            }
            if let Some(ref msd) = s.msd {
                let _ = msd.msd_disconnect().await;
            }
        }
    }
}

// ── Helpers ──

fn any_feature_in_use(features: &[Arc<FeatureConnection>]) -> bool {
    features.iter().any(|f| f.ref_count.load(Ordering::Acquire) > 0)
}

async fn set_features_state(features: &[Arc<FeatureConnection>], state: ConnectionState) {
    for fc in features {
        *fc.state.write().await = state;
        fc.publish(ConnectionEvent::StateChanged {
            feature: fc.id.clone(),
            state,
        });
    }
}

const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

fn compute_retry_delay(current: Duration, woke_during_retry: bool) -> Duration {
    if woke_during_retry {
        INITIAL_RETRY_DELAY
    } else {
        (current * 2).min(MAX_RETRY_DELAY)
    }
}

/// A transport that is always "connected" — used for injected/mock devices
/// in tests. Never actually opens any network connection.
pub(crate) struct InjectedTransport {
    id: String,
}

impl InjectedTransport {
    pub fn new(device_id: &str) -> Self {
        InjectedTransport {
            id: format!("{}-injected", device_id),
        }
    }
}

#[async_trait::async_trait]
impl Transport for InjectedTransport {
    fn id(&self) -> &str {
        &self.id
    }

    fn features(&self) -> &[&str] {
        &["video", "hid", "msd", "power"]
    }

    async fn connect(
        &self,
        _cfg: &DeviceConfig,
        _encoder: SharedEncoder,
        _progress: &ConnectProgress,
    ) -> Result<DeviceSession, anyhow::Error> {
        // Should never be called — inject_device sets session directly.
        Err(anyhow::anyhow!("InjectedTransport cannot connect"))
    }

    fn on_disconnect(&self) -> Option<watch::Receiver<bool>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmc::connection::feature::{FeatureGuard, FeatureConnection};
    use crate::bmc::session::types::DeviceSession;

    fn make_ts() -> Arc<TransportState> {
        let (wake_tx, _) = watch::channel(0u64);
        let (connected, _) = watch::channel(false);
        Arc::new(TransportState {
            id: "test".to_string(),
            transport: Arc::new(InjectedTransport::new("test")),
            session: RwLock::new(None),
            ref_count: AtomicI64::new(0),
            wake_tx,
            connected,
            idle_timeout: Duration::from_secs(10),
        })
    }

    #[tokio::test]
    async fn test_wait_for_session_times_out() {
        let ts = make_ts();
        let start = tokio::time::Instant::now();
        let result = ts.wait_for_session(Duration::from_millis(50)).await;
        let elapsed = start.elapsed();
        assert!(result.is_err());
        match result {
            Err(e) => assert_eq!(e, "connection timeout"),
            Ok(_) => panic!("expected timeout error"),
        }
        assert!(elapsed < Duration::from_millis(500), "took too long: {:?}", elapsed);
    }

    #[tokio::test]
    async fn test_wait_for_session_returns_when_connected() {
        let ts = make_ts();
        let session = DeviceSession { base: None, input: None, power: None, msd: None };
        *ts.session.write().await = Some(session);
        ts.connected.send_replace(true);
        let result = ts.wait_for_session(Duration::from_secs(5)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_for_session_honours_long_timeout() {
        let ts = make_ts();
        let start = tokio::time::Instant::now();
        let result = ts.wait_for_session(Duration::from_millis(200)).await;
        let elapsed = start.elapsed();
        assert!(result.is_err());
        assert!(elapsed >= Duration::from_millis(150), "returned too fast: {:?}", elapsed);
    }

    #[tokio::test]
    async fn test_wait_for_session_reacts_to_late_connect() {
        let ts = make_ts();
        let ts_clone = ts.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let session = DeviceSession { base: None, input: None, power: None, msd: None };
            *ts_clone.session.write().await = Some(session);
            ts_clone.connected.send_replace(true);
        });
        let result = ts.wait_for_session(Duration::from_secs(5)).await;
        assert!(result.is_ok(), "should connect after background set");
    }

    #[tokio::test]
    async fn test_any_feature_in_use_true_when_ref_count_positive() {
        let ts = make_ts();
        let fc = FeatureConnection::new("video".to_string(), &ts);
        let features = vec![fc.clone()];
        assert!(!any_feature_in_use(&features));
        let _guard = FeatureGuard::new(fc);
        assert!(any_feature_in_use(&features));
    }

    #[test]
    fn test_compute_retry_delay_grows_exponentially() {
        let d = compute_retry_delay(Duration::from_secs(1), false);
        assert_eq!(d, Duration::from_secs(2));
        let d = compute_retry_delay(Duration::from_secs(30), false);
        assert_eq!(d, MAX_RETRY_DELAY);
    }

    #[test]
    fn test_compute_retry_delay_resets_on_wake() {
        let d = compute_retry_delay(Duration::from_secs(16), true);
        assert_eq!(d, INITIAL_RETRY_DELAY);
    }
}
