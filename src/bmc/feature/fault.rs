//! Shared, sanitized fault contract for every feature instance.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::bmc::feature::connection::FeatureConnection;

const FEATURE_HISTORY_LIMIT: usize = 16;
const CHILD_HISTORY_LIMIT: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureFaultDomain {
    Lifecycle,
    Transport,
    Protocol,
    Operation,
    Storage,
    Decode,
    Encoder,
    BrowserTransport,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum FeatureFaultScope {
    Feature,
    Operation { operation: &'static str },
    Child { kind: &'static str, id: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FeatureFault {
    pub domain: FeatureFaultDomain,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    #[serde(flatten)]
    pub scope: FeatureFaultScope,
    pub occurred_at_ms: u64,
    pub count: u64,
}

impl FeatureFault {
    pub fn feature(
        domain: FeatureFaultDomain,
        code: &'static str,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            domain,
            code,
            message: message.into(),
            retryable,
            scope: FeatureFaultScope::Feature,
            occurred_at_ms: now_ms(),
            count: 1,
        }
    }

    pub fn operation(
        domain: FeatureFaultDomain,
        code: &'static str,
        message: impl Into<String>,
        retryable: bool,
        operation: &'static str,
    ) -> Self {
        Self {
            domain,
            code,
            message: message.into(),
            retryable,
            scope: FeatureFaultScope::Operation { operation },
            occurred_at_ms: now_ms(),
            count: 1,
        }
    }

    pub fn child(
        domain: FeatureFaultDomain,
        code: &'static str,
        message: impl Into<String>,
        retryable: bool,
        kind: &'static str,
        id: impl Into<String>,
    ) -> Self {
        Self {
            domain,
            code,
            message: message.into(),
            retryable,
            scope: FeatureFaultScope::Child {
                kind,
                id: id.into(),
            },
            occurred_at_ms: now_ms(),
            count: 1,
        }
    }
}

#[derive(Default)]
struct FaultState {
    feature: Option<FeatureFault>,
    operation: Option<FeatureFault>,
    feature_history: VecDeque<FeatureFault>,
    child_history: VecDeque<FeatureFault>,
}

impl FaultState {
    fn active(&self) -> Option<&FeatureFault> {
        self.feature.as_ref().or(self.operation.as_ref())
    }
}

#[derive(Clone, Default)]
pub struct FeatureFaultStore {
    state: Arc<RwLock<FaultState>>,
    // Serialize commits through their notification without holding the state lock
    // across callbacks, so a late old notification cannot follow a newer clear.
    publication: Arc<tokio::sync::Mutex<()>>,
    notifier: Arc<StdRwLock<Option<FaultNotifier>>>,
}

type FaultNotifier = Arc<dyn Fn(Option<FeatureFault>) + Send + Sync>;

impl FeatureFaultStore {
    /// Install the feature-owned notification hook.  The store deliberately
    /// knows nothing about HTTP/WebSocket routing; FeatureConnection supplies
    /// that bridge once it has a stable identity.
    pub fn set_notifier(&self, notifier: FaultNotifier) {
        *self
            .notifier
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(notifier);
    }

    fn notify(&self, active: Option<FeatureFault>) {
        let notifier = self
            .notifier
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(notifier) = notifier {
            notifier(active);
        }
    }

    pub async fn record(&self, mut fault: FeatureFault) {
        let _publication = self.publication.lock().await;
        let mut state = self.state.write().await;
        let previous_active = state.active().cloned();
        let history = if matches!(fault.scope, FeatureFaultScope::Child { .. }) {
            &mut state.child_history
        } else {
            &mut state.feature_history
        };
        if let Some(previous) = history.back_mut().filter(|previous| {
            previous.domain == fault.domain
                && previous.code == fault.code
                && previous.scope == fault.scope
        }) {
            // Coalesce identity/count only; recovery policy and diagnostics
            // belong to the latest observation, including after a clear.
            fault.count = previous.count.saturating_add(1);
            fault.occurred_at_ms = now_ms();
            *previous = fault.clone();
        } else {
            history.push_back(fault.clone());
            let limit = if matches!(fault.scope, FeatureFaultScope::Child { .. }) {
                CHILD_HISTORY_LIMIT
            } else {
                FEATURE_HISTORY_LIMIT
            };
            while history.len() > limit {
                history.pop_front();
            }
        }
        match &fault.scope {
            FeatureFaultScope::Feature => state.feature = Some(fault),
            FeatureFaultScope::Operation { .. } => state.operation = Some(fault),
            FeatureFaultScope::Child { .. } => {}
        }
        let active = state.active().cloned();
        drop(state);
        if active != previous_active {
            self.notify(active);
        }
    }
    pub async fn active(&self) -> Option<FeatureFault> {
        self.state.read().await.active().cloned()
    }
    /// Feature availability is independent of the latest operation diagnostic.
    pub async fn active_feature(&self) -> Option<FeatureFault> {
        self.state.read().await.feature.clone()
    }
    pub async fn clear_operation(&self, operation: &str) {
        let _publication = self.publication.lock().await;
        let mut state = self.state.write().await;
        let previous_active = state.active().cloned();
        if state.operation.as_ref().is_some_and(|fault| {
            matches!(fault.scope, FeatureFaultScope::Operation { operation: name } if name == operation)
        }) {
            state.operation = None;
        }
        let active = state.active().cloned();
        drop(state);
        if active != previous_active {
            self.notify(active);
        }
    }
    pub async fn clear_retryable(&self) {
        let _publication = self.publication.lock().await;
        let mut state = self.state.write().await;
        let previous_active = state.active().cloned();
        if state.feature.as_ref().is_some_and(|fault| fault.retryable) {
            state.feature = None;
        }
        if state
            .operation
            .as_ref()
            .is_some_and(|fault| fault.retryable)
        {
            state.operation = None;
        }
        let active = state.active().cloned();
        drop(state);
        if active != previous_active {
            self.notify(active);
        }
    }
    /// A connection retry never replays or clears an operation result.
    pub async fn prepare_retry(&self) -> Result<(), FeatureFault> {
        let _publication = self.publication.lock().await;
        let mut state = self.state.write().await;
        if let Some(fault) = state.feature.as_ref().filter(|fault| !fault.retryable) {
            return Err(fault.clone());
        }
        let previous_active = state.active().cloned();
        state.feature = None;
        let active = state.active().cloned();
        drop(state);
        if active != previous_active {
            self.notify(active);
        }
        Ok(())
    }
    /// A successful reconnect clears current failures, retaining diagnostic history.
    pub async fn clear_active(&self) {
        let _publication = self.publication.lock().await;
        let mut state = self.state.write().await;
        let cleared = state.active().is_some();
        state.feature = None;
        state.operation = None;
        drop(state);
        if cleared {
            self.notify(None);
        }
    }
    pub async fn snapshot(&self) -> serde_json::Value {
        let state = self.state.read().await;
        serde_json::json!({"active": state.active(), "feature_history": state.feature_history, "child_history": state.child_history})
    }
}

pub fn lifecycle_connect_fault(error: &str) -> FeatureFault {
    let (code, message) = if error.contains("APCP") && error.contains("early eof") {
        (
            "apcp_rejected",
            "The BMC closed the AVCT APCP setup connection",
        )
    } else if error.contains("TLS handshake") {
        ("tls_handshake_failed", "The backend TLS handshake failed")
    } else if error.contains("already in use") {
        ("session_in_use", "The backend session is already in use")
    } else if error.contains("login was denied") {
        (
            "backend_login_denied",
            "The backend rejected the configured account for this feature",
        )
    } else if error.contains("timed out") {
        ("connect_timeout", "The backend connection timed out")
    } else {
        (
            "backend_unavailable",
            "The feature backend connection failed",
        )
    };
    FeatureFault::feature(FeatureFaultDomain::Lifecycle, code, message, true)
}

pub fn fault_response(status: StatusCode, fault: FeatureFault) -> Response {
    (
        status,
        Json(serde_json::json!({"success": false, "fault": fault})),
    )
        .into_response()
}

/// Persist a failure on the feature instance and return the one public error
/// shape used by HTTP feature APIs.  Backend error text is intentionally not
/// accepted here: adapters log their detailed cause and map it to a stable,
/// user-safe fault before crossing the feature boundary.
pub async fn record_fault_response(
    fc: &FeatureConnection,
    status: StatusCode,
    fault: FeatureFault,
) -> Response {
    fc.faults.record(fault.clone()).await;
    fault_response(status, fault)
}

/// Return the active lifecycle fault, or turn an acquisition failure into a
/// sanitized lifecycle fault when the lifecycle could not publish one (for
/// example while a connection is being replaced).
pub async fn acquisition_fault_response(
    fc: &FeatureConnection,
    status: StatusCode,
    error: &str,
) -> Response {
    let fault = match fc.faults.active_feature().await {
        Some(fault) => fault,
        None => {
            let fault = lifecycle_connect_fault(error);
            fc.faults.record(fault.clone()).await;
            fault
        }
    };
    fault_response(status, fault)
}

/// A structured failure that has no corresponding configured feature
/// instance.  It is intentionally not persisted because there is no stable
/// owner for it.
pub fn unavailable_fault_response(status: StatusCode, feature: &'static str) -> Response {
    fault_response(
        status,
        FeatureFault::feature(
            FeatureFaultDomain::Lifecycle,
            "feature_unavailable",
            format!("The {feature} feature is not available for this device"),
            false,
        ),
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmc::feature::config::FeatureConfig;
    use crate::bmc::feature::connection::{ConnectionEvent, FeatureConnection};

    #[tokio::test]
    async fn operation_diagnostics_cannot_replace_a_feature_failure() {
        let store = FeatureFaultStore::default();
        store
            .record(FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "fatal",
                "Feature unavailable",
                false,
            ))
            .await;
        store
            .record(FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "status_failed",
                "Read failed",
                true,
                "status",
            ))
            .await;
        assert_eq!(store.active_feature().await.unwrap().code, "fatal");
        assert_eq!(store.active().await.unwrap().code, "fatal");
        store.clear_operation("status").await;
        assert_eq!(store.active().await.unwrap().code, "fatal");
        assert_eq!(
            store.snapshot().await["feature_history"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn retryable_operation_cleanup_cannot_erase_a_feature_failure() {
        let store = FeatureFaultStore::default();
        store
            .record(FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "fatal",
                "Feature unavailable",
                false,
            ))
            .await;
        store
            .record(FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "status_failed",
                "Read failed",
                true,
                "status",
            ))
            .await;
        store.clear_retryable().await;
        assert_eq!(store.active_feature().await.unwrap().code, "fatal");
        assert_eq!(store.active().await.unwrap().code, "fatal");
        store.clear_active().await;
        assert!(store.active_feature().await.is_none());
        assert!(store.active().await.is_none());
    }

    #[tokio::test]
    async fn retry_preparation_reveals_operation_diagnostics_without_erasing_history() {
        let store = FeatureFaultStore::default();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        store.set_notifier(Arc::new({
            let events = events.clone();
            move |fault| events.lock().unwrap().push(fault.map(|fault| fault.code))
        }));
        store
            .record(FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "feature_failed",
                "Disconnected",
                true,
            ))
            .await;
        store
            .record(FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "operation_unknown",
                "Unknown result",
                false,
                "control",
            ))
            .await;
        store.prepare_retry().await.unwrap();
        assert!(store.active_feature().await.is_none());
        assert_eq!(store.active().await.unwrap().code, "operation_unknown");
        assert_eq!(
            *events.lock().unwrap(),
            [Some("feature_failed"), Some("operation_unknown")]
        );
        assert_eq!(
            store.snapshot().await["feature_history"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fault_commit_and_clear_notifications_preserve_order() {
        let faults = FeatureFaultStore::default();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (entered, ready) = tokio::sync::oneshot::channel();
        let entered = Arc::new(std::sync::Mutex::new(Some(entered)));
        let (release, wait) = std::sync::mpsc::channel();
        let wait = std::sync::Mutex::new(wait);
        faults.set_notifier(Arc::new({
            let events = events.clone();
            move |fault| {
                if fault.is_some() {
                    entered.lock().unwrap().take().unwrap().send(()).unwrap();
                    wait.lock()
                        .unwrap()
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .unwrap();
                }
                events.lock().unwrap().push(fault.map(|fault| fault.code));
            }
        }));
        let record = tokio::spawn({
            let faults = faults.clone();
            async move {
                faults
                    .record(FeatureFault::feature(
                        FeatureFaultDomain::Transport,
                        "test_transport",
                        "Transport failed",
                        true,
                    ))
                    .await
            }
        });
        ready.await.unwrap();
        let mut clear = tokio::spawn({
            let faults = faults.clone();
            async move { faults.clear_active().await }
        });
        let pending = tokio::time::timeout(std::time::Duration::from_millis(30), &mut clear)
            .await
            .is_err();
        release.send(()).unwrap();
        record.await.unwrap();
        if pending {
            clear.await.unwrap();
        }
        assert!(
            pending,
            "a newer publication cannot overtake the preceding notification"
        );
        assert!(faults.active().await.is_none());
        assert_eq!(*events.lock().unwrap(), [Some("test_transport"), None]);
    }

    #[tokio::test]
    async fn repeated_feature_fault_uses_latest_recovery_policy() {
        for (old_retryable, new_retryable) in [(true, false), (false, true)] {
            let store = FeatureFaultStore::default();
            store
                .record(FeatureFault::feature(
                    FeatureFaultDomain::Transport,
                    "test_failure",
                    "Old observation",
                    old_retryable,
                ))
                .await;
            store
                .record(FeatureFault::feature(
                    FeatureFaultDomain::Transport,
                    "test_failure",
                    "Current observation",
                    new_retryable,
                ))
                .await;
            let active = store.active().await.unwrap();
            assert_eq!(active.retryable, new_retryable);
            assert_eq!(active.message, "Current observation");
            assert_eq!(active.count, 2);
            let snapshot = store.snapshot().await;
            assert_eq!(snapshot["feature_history"].as_array().unwrap().len(), 1);
            assert_eq!(
                snapshot["feature_history"][0]["message"],
                "Current observation"
            );
            assert_eq!(store.prepare_retry().await.is_ok(), new_retryable);
        }
    }

    #[tokio::test]
    async fn rerecord_after_clear_does_not_restore_old_operation_policy() {
        let store = FeatureFaultStore::default();
        store
            .record(FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "test_operation",
                "Old observation",
                true,
                "control",
            ))
            .await;
        store.clear_active().await;
        store
            .record(FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "test_operation",
                "Current observation",
                false,
                "control",
            ))
            .await;
        store.clear_retryable().await;
        let active = store
            .active()
            .await
            .expect("the current nonretryable result must survive cleanup");
        assert_eq!(active.message, "Current observation");
        assert!(!active.retryable);
        assert_eq!(active.count, 2);
    }

    #[tokio::test]
    async fn repeated_child_fault_retains_latest_payload_without_becoming_active() {
        let store = FeatureFaultStore::default();
        store
            .record(FeatureFault::child(
                FeatureFaultDomain::Encoder,
                "test_encode",
                "Old observation",
                true,
                "stream",
                "client",
            ))
            .await;
        store
            .record(FeatureFault::child(
                FeatureFaultDomain::Encoder,
                "test_encode",
                "Current observation",
                false,
                "stream",
                "client",
            ))
            .await;
        let snapshot = store.snapshot().await;
        assert!(store.active().await.is_none());
        assert_eq!(snapshot["child_history"].as_array().unwrap().len(), 1);
        assert_eq!(
            snapshot["child_history"][0]["message"],
            "Current observation"
        );
        assert_eq!(snapshot["child_history"][0]["retryable"], false);
        assert_eq!(snapshot["child_history"][0]["count"], 2);
    }

    #[tokio::test]
    async fn deduplicates_active_faults_and_preserves_child_scope() {
        let store = FeatureFaultStore::default();
        let active = FeatureFault::operation(
            FeatureFaultDomain::Operation,
            "operation_failed",
            "A safe operation failure",
            true,
            "test",
        );
        store.record(active.clone()).await;
        store.record(active).await;

        let current = store.active().await.expect("active fault");
        assert_eq!(current.code, "operation_failed");
        assert_eq!(current.count, 2);

        store
            .record(FeatureFault::child(
                FeatureFaultDomain::Encoder,
                "encode_failed",
                "A safe child failure",
                true,
                "mjpeg",
                "browser-1",
            ))
            .await;
        assert_eq!(
            store.active().await.expect("active fault").code,
            "operation_failed"
        );

        let snapshot = store.snapshot().await;
        assert_eq!(snapshot["feature_history"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["child_history"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn feature_connection_emits_fault_changed_for_active_faults() {
        let fc = FeatureConnection::new(
            "device-a".into(),
            "video".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: Vec::new(),
            },
        );
        let mut events = fc.event_rx.clone();
        fc.faults
            .record(FeatureFault::feature(
                FeatureFaultDomain::Protocol,
                "protocol_failed",
                "A safe protocol failure",
                true,
            ))
            .await;
        events.changed().await.expect("fault event");
        let event = events.borrow().clone();
        match event {
            Some(ConnectionEvent::FaultChanged {
                fault: Some(fault), ..
            }) => {
                assert_eq!(fault.code, "protocol_failed");
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn lifecycle_mapping_never_exposes_backend_error_text() {
        let secret = "password=must-not-be-public";
        let fault = lifecycle_connect_fault(&format!("APCP early eof {secret}"));
        assert_eq!(fault.code, "apcp_rejected");
        assert!(!fault.message.contains(secret));
    }
}

#[cfg(test)]
mod operation_recovery_tests {
    use super::*;
    #[tokio::test]
    async fn successful_status_clears_only_its_own_active_fault() {
        let faults = FeatureFaultStore::default();
        faults
            .record(FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "control_failed",
                "unknown",
                false,
                "control",
            ))
            .await;
        faults.clear_operation("status").await;
        assert_eq!(faults.active().await.unwrap().code, "control_failed");
        faults
            .record(FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "status_failed",
                "unavailable",
                true,
                "status",
            ))
            .await;
        faults.clear_operation("status").await;
        assert!(faults.active().await.is_none());
    }
}
