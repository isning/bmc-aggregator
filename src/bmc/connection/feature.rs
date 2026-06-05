use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Weak;

use tokio::sync::RwLock;

use super::event::{ConnectionEvent, ConnectionEventReceiver, ConnectionEventSender};
use super::state::ConnectionState;
use super::transport::TransportState;

/// Per-feature connection runtime state.
pub struct FeatureConnection {
    pub id: String,
    pub state: RwLock<ConnectionState>,
    pub ref_count: AtomicI64,
    pub event_tx: ConnectionEventSender,
    pub transport: Weak<TransportState>,
}

impl FeatureConnection {
    pub fn new(id: String, transport: &Arc<TransportState>) -> Arc<Self> {
        let (event_tx, _) = tokio::sync::broadcast::channel(32);
        Arc::new(FeatureConnection {
            id,
            state: RwLock::new(ConnectionState::Disconnected),
            ref_count: AtomicI64::new(0),
            event_tx,
            transport: Arc::downgrade(transport),
        })
    }

    pub fn subscribe(&self) -> ConnectionEventReceiver {
        self.event_tx.subscribe()
    }

    pub(crate) fn publish(&self, event: ConnectionEvent) {
        let _ = self.event_tx.send(event);
    }
}

/// RAII guard: increments ref_count on creation, decrements on drop.
/// While the guard lives, the transport stays connected.
pub struct FeatureGuard {
    pub(crate) feature: Arc<FeatureConnection>,
}

impl FeatureGuard {
    pub(crate) fn new(feature: Arc<FeatureConnection>) -> Self {
        feature.ref_count.fetch_add(1, Ordering::Acquire);
        if let Some(ts) = feature.transport.upgrade() {
            let val = ts.wake_tx.borrow().wrapping_add(1);
            let _ = ts.wake_tx.send(val);
        }
        FeatureGuard { feature }
    }

    pub fn feature_id(&self) -> &str {
        &self.feature.id
    }
}

impl Drop for FeatureGuard {
    fn drop(&mut self) {
        self.feature.ref_count.fetch_sub(1, Ordering::Release);
        if let Some(ts) = self.feature.transport.upgrade() {
            let val = ts.wake_tx.borrow().wrapping_add(1);
            let _ = ts.wake_tx.send(val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_transport_state() -> Arc<TransportState> {
        let (wake_tx, _) = tokio::sync::watch::channel(0u64);
        let (connected, _) = tokio::sync::watch::channel(false);
        Arc::new(TransportState {
            id: "test".to_string(),
            transport: Arc::new(crate::bmc::connection::transport::InjectedTransport::new("test")),
            session: tokio::sync::RwLock::new(None),
            ref_count: AtomicI64::new(0),
            wake_tx,
            connected,
            idle_timeout: Duration::from_secs(10),
        })
    }

    fn make_feature() -> Arc<FeatureConnection> {
        let ts = make_transport_state();
        FeatureConnection::new("video".to_string(), &ts)
    }

    #[test]
    fn test_guard_new_increments_ref_count() {
        let fc = make_feature();
        assert_eq!(fc.ref_count.load(Ordering::Acquire), 0);
        let guard = FeatureGuard::new(fc.clone());
        assert_eq!(fc.ref_count.load(Ordering::Acquire), 1);
        drop(guard);
        assert_eq!(fc.ref_count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn test_guard_multiple_stacks_ref_count() {
        let fc = make_feature();
        let g1 = FeatureGuard::new(fc.clone());
        assert_eq!(fc.ref_count.load(Ordering::Acquire), 1);
        let g2 = FeatureGuard::new(fc.clone());
        assert_eq!(fc.ref_count.load(Ordering::Acquire), 2);
        drop(g1);
        assert_eq!(fc.ref_count.load(Ordering::Acquire), 1);
        drop(g2);
        assert_eq!(fc.ref_count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn test_guard_new_does_not_deadlock() {
        let fc = make_feature();
        let guard = FeatureGuard::new(fc.clone());
        drop(guard);
    }

    #[test]
    fn test_guard_drop_does_not_deadlock() {
        let fc = make_feature();
        {
            let _guard = FeatureGuard::new(fc.clone());
        }
    }

    #[test]
    fn test_guard_wake_tx_incremented_on_new() {
        let ts = make_transport_state();
        let rx = ts.wake_tx.subscribe();
        let fc = FeatureConnection::new("video".to_string(), &ts);
        assert_eq!(*rx.borrow(), 0);
        let guard = FeatureGuard::new(fc.clone());
        assert_eq!(*rx.borrow(), 1);
        drop(guard);
        assert_eq!(*rx.borrow(), 2);
    }

    #[tokio::test]
    async fn test_guard_new_wakes_changed() {
        let ts = make_transport_state();
        let mut rx = ts.wake_tx.subscribe();
        let fc = FeatureConnection::new("video".to_string(), &ts);

        tokio::select! {
            _ = rx.changed() => panic!("should not be ready before guard"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }

        let guard = FeatureGuard::new(fc.clone());

        tokio::select! {
            result = rx.changed() => assert!(result.is_ok()),
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                panic!("changed() not woken after guard creation");
            }
        }

        drop(guard);
    }

    #[test]
    fn test_guard_dead_transport_does_not_panic() {
        let ts = make_transport_state();
        let fc = FeatureConnection::new("video".to_string(), &ts);
        drop(ts);
        let guard = FeatureGuard::new(fc.clone());
        drop(guard);
    }
}
