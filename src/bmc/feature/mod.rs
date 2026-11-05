pub mod config;
pub mod config_edit;
pub mod connection;
pub mod data;
pub mod device_registry;
pub mod fault;

pub mod audio;
pub mod boot;
pub mod firmware;
pub mod health;
pub mod hid;
pub mod identify;
pub mod managed;
pub mod msd;
pub mod power;
pub mod registry;
pub mod requirements;
pub mod video;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::response::IntoResponse;
use axum::Router;
use futures::future::BoxFuture;

use crate::bmc::feature::connection::{ConnectionState, FeatureConnection, FeatureGuard};
use crate::bmc::feature::fault::{
    fault_response, unavailable_fault_response, FeatureFault, FeatureFaultDomain,
};
use crate::bmc::feature::managed::ManagedDevice;
use crate::bmc::feature::registry::FeatureBackendTemplate;
use crate::define_backends;
use crate::web::state::AppState;

use crate::bmc::backends::idrac_avct::hid as idrac_avct_hid;
use crate::bmc::backends::idrac_avct::video as idrac_avct_video;
use crate::bmc::backends::idrac_legacy_media::msd as idrac_legacy_media_msd;
use crate::bmc::backends::idrac_wsman;
use crate::bmc::backends::ipmi::boot as ipmi_boot;
use crate::bmc::backends::ipmi::identify as ipmi_identify;
use crate::bmc::backends::ipmi::power as ipmi_power;
use crate::bmc::backends::mahogany::firmware as mahogany_firmware;
use crate::bmc::backends::mahogany::hid as mahogany_hid;
use crate::bmc::backends::mahogany::msd as mahogany_msd;
use crate::bmc::backends::mahogany::video as mahogany_video;
use crate::bmc::backends::redfish::boot as redfish_boot;
use crate::bmc::backends::redfish::identify as redfish_identify;
use crate::bmc::backends::redfish::msd as redfish_msd;
use crate::bmc::backends::redfish::power as redfish_power;
use crate::bmc::backends::redfish::{firmware as redfish_firmware, health as redfish_health};
use crate::bmc::backends::vnc::hid as vnc_hid;
use crate::bmc::backends::vnc::video as vnc_video;
use crate::bmc::backends::wol as wol_power;

/// Mount all feature routes on the axum router.
/// Each feature submodule owns its route declarations.
pub fn mount_all_feature_routes(router: Router<Arc<AppState>>) -> Router<Arc<AppState>> {
    let feat = "/bmc/:id/api/feature";
    let router = hid::mount(router, &format!("{feat}/hid"));
    let router = msd::mount(router, &format!("{feat}/msd"));
    let router = power::mount(router, &format!("{feat}/power"));
    let router = boot::mount(router, &format!("{feat}/boot"));
    let router = identify::mount(router, &format!("{feat}/identify"));
    let router = firmware::mount(router, &format!("{feat}/firmware"));
    let router = health::mount(router, &format!("{feat}/health"));
    let router = audio::mount(router, &format!("{feat}/audio"));
    let router = video::mount(router, &format!("{feat}/video"));
    requirements::mount(router, "/bmc/:id/api/requirements")
}

/// All registered feature backends (lazy-initialized via define_backends!).
pub fn registered_backends() -> &'static [FeatureBackendTemplate] {
    define_backends!(
        "mahogany" => "video" => mahogany_video,
        "mahogany" => "hid" => mahogany_hid,
        "mahogany" => "msd" => mahogany_msd,
        "mahogany" => "firmware" => mahogany_firmware,
        "vnc" => "video" => vnc_video,
        "vnc" => "hid" => vnc_hid,
        "idrac-avct-console" => "video" => idrac_avct_video,
        "idrac-avct-console" => "hid" => idrac_avct_hid,
        "idrac-legacy-media" => "msd" => idrac_legacy_media_msd,
        "idrac-wsman" => "boot" => idrac_wsman => requirements,
        "ipmi" => "power" => ipmi_power,
        "ipmi" => "boot" => ipmi_boot,
        "ipmi" => "identify" => ipmi_identify,
        "redfish" => "power" => redfish_power,
        "redfish" => "boot" => redfish_boot,
        "redfish" => "identify" => redfish_identify,
        "redfish" => "firmware" => redfish_firmware,
        "redfish" => "health" => redfish_health,
        "redfish" => "msd" => redfish_msd,
        "wol" => "power" => wol_power,
    )
}

/// Build common feature-status JSON at the feature layer.
///
/// Returns only the unified base shape `{available, online, state, backend}`
/// from FC state. Feature handlers append typed backend fields when their
/// feature trait exposes status data.
pub async fn build_feature_status(fc: &Arc<FeatureConnection>) -> serde_json::Value {
    let state_str = match *fc.state.read().await {
        ConnectionState::Connected => "connected",
        ConnectionState::Connecting => "connecting",
        ConnectionState::Disconnected => "disconnected",
        ConnectionState::Disconnecting => "disconnecting",
    };
    let fault = fc.faults.active().await;
    serde_json::json!({
        "available": true,
        "online": fc.healthy.load(Ordering::SeqCst),
        "state": state_str,
        "backend": fc.backend,
        "connection_id": fc.instance_id,
        "ref_count": fc.ref_count(),
        "fault": fault,
    })
}

pub async fn build_feature_instances_status(
    dev: &ManagedDevice,
    feature_id: &str,
) -> (Vec<serde_json::Value>, usize) {
    let mut instances = Vec::new();
    for instance in dev.instances_for_feature(feature_id).await {
        instances.push(build_feature_status(&instance).await);
    }
    let active_instances = instances
        .iter()
        .filter(|status| {
            status
                .get("state")
                .and_then(|v| v.as_str())
                .is_some_and(|state| state != "disconnected")
        })
        .count();
    (instances, active_instances)
}

pub async fn acquire_connected_feature(
    fc: Arc<FeatureConnection>,
    timeout: Duration,
) -> Result<FeatureGuard, String> {
    if fc.is_stopped() {
        return Err(format!(
            "feature '{}' on device '{}' is being reconfigured",
            fc.id, fc.device_id
        ));
    }
    if !fc.lifecycle_started.load(Ordering::SeqCst) {
        return Err(format!(
            "feature '{}' on device '{}' has no registered backend lifecycle",
            fc.id, fc.device_id
        ));
    }

    let guard = FeatureGuard::new(fc.clone());
    let mut events = fc.event_rx.clone();
    let mut stopped = fc.stopped_rx();
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        if fc.is_stopped() {
            return Err(format!(
                "feature '{}' on device '{}' is being reconfigured",
                fc.id, fc.device_id
            ));
        }
        if *fc.state.read().await == ConnectionState::Connected {
            return Ok(guard);
        }
        if let Some(fault) = fc.faults.active_feature().await {
            return Err(format!(
                "feature '{}' on device '{}' failed to connect: {} ({})",
                fc.id, fc.device_id, fault.message, fault.code
            ));
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(format!(
                "feature '{}' on device '{}' did not connect within {}s",
                fc.id,
                fc.device_id,
                timeout.as_secs()
            ));
        }

        let remaining = deadline.saturating_duration_since(now);
        let changed = tokio::select! {
            biased;
            _ = stopped.changed() => continue,
            changed = tokio::time::timeout(remaining, events.changed()) => changed,
        };
        match changed {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => return Err("feature event channel closed".to_string()),
            Err(_) => {
                return Err(format!(
                    "feature '{}' on device '{}' did not connect within {}s",
                    fc.id,
                    fc.device_id,
                    timeout.as_secs()
                ));
            }
        }
    }
}

pub fn feature_connect_status(error: &str) -> axum::http::StatusCode {
    if error.contains("has no registered backend lifecycle")
        || error.contains("is being reconfigured")
    {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    } else if error.contains("session_in_use") {
        axum::http::StatusCode::CONFLICT
    } else {
        axum::http::StatusCode::GATEWAY_TIMEOUT
    }
}

/// Retry the feature lifecycle without replaying or clearing operation results.  Each
/// feature module owns its route and delegates here only for the common
/// connection semantics; this does not introduce a generic feature dispatcher.
pub async fn retry_feature(
    state: &Arc<AppState>,
    device_id: &str,
    feature_id: &'static str,
) -> axum::response::Response {
    let Some(device) = state.registry.get_dev(device_id).await else {
        return fault_response(
            axum::http::StatusCode::NOT_FOUND,
            FeatureFault::operation(
                FeatureFaultDomain::Lifecycle,
                "device_not_found",
                "The requested BMC device was not found",
                false,
                "retry",
            ),
        );
    };
    let Some(fc) = device.features.get(feature_id).cloned() else {
        return unavailable_fault_response(axum::http::StatusCode::SERVICE_UNAVAILABLE, feature_id);
    };
    if fc.is_stopped() {
        return unavailable_fault_response(axum::http::StatusCode::SERVICE_UNAVAILABLE, feature_id);
    }
    if let Err(fault) = fc.faults.prepare_retry().await {
        return fault_response(axum::http::StatusCode::CONFLICT, fault);
    }
    fc.wake.notify_one();
    axum::Json(serde_json::json!({
        "success": true,
        "retry_requested": true,
        "connection_id": fc.instance_id,
    }))
    .into_response()
}

/// Transport-agnostic event protocol codec.
///
/// Each feature defines its own codec to parse raw bytes into
/// (action, params) pairs, and to encode lifecycle messages.
/// The codec does NOT depend on the transport layer (WebSocket, SSE, etc.).
#[async_trait]
pub trait FeatureEventCodec: Send + Sync {
    /// Parse a request from raw bytes into (action, params).
    fn parse_request(&self, data: &[u8]) -> Result<(String, serde_json::Value), String>;
    /// Encode the "connected" acknowledgement (e.g. RESP_OK 0x00).
    fn encode_connected(&self) -> Option<Vec<u8>>;
    /// Encode an error response.
    fn encode_error(&self, err: &str) -> Vec<u8>;
    /// Encode a request/dispatch error that should not poison the transport.
    fn encode_invalid_message(&self, err: &str) -> Vec<u8> {
        self.encode_error(err)
    }
}

/// Run a transport-agnostic feature event loop.
///
/// Caller injects:
/// - `fc` — the FeatureConnection to keep alive
/// - `codec` — feature-specific protocol parser/encoder
/// - `recv` — async closure: receive the next raw message
/// - `send` — async closure: send a raw response
/// - `dispatch` — async closure: execute a parsed (action, params) against the backend
///
/// Lifecycle:
///   1. RAII FeatureGuard (keeps FC alive)
///   2. Wait up to 15s for FC → `Connected`
///   3. Send connect-ack or error via `codec` / `send`
///   4. Loop: recv → codec.parse_request → dispatch
///   5. Exit on recv returning None
pub async fn run_feature_event_loop<RecvF, SendF, DispatchF>(
    fc: Arc<FeatureConnection>,
    codec: Arc<dyn FeatureEventCodec>,
    mut recv: RecvF,
    send: SendF,
    dispatch: DispatchF,
) where
    RecvF: FnMut() -> BoxFuture<'static, Option<Vec<u8>>> + Send + 'static,
    SendF: Fn(Vec<u8>) -> BoxFuture<'static, Result<(), String>> + Send + 'static,
    DispatchF:
        Fn(String, serde_json::Value) -> BoxFuture<'static, Result<(), String>> + Send + 'static,
{
    let _guard = FeatureGuard::new(fc.clone());
    let mut events = fc.event_rx.clone();
    let mut stopped = fc.stopped_rx();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let connected = loop {
        if *stopped.borrow() {
            return;
        }
        if *fc.state.read().await == ConnectionState::Connected {
            break true;
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            break false;
        }

        let remaining = deadline.saturating_duration_since(now);
        let changed = tokio::select! {
            _ = stopped.changed() => return,
            changed = tokio::time::timeout(remaining, events.changed()) => changed,
        };
        match changed {
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break false,
        }
    };

    if connected {
        if let Some(ack) = codec.encode_connected() {
            if !send_feature_response(send(ack), &mut stopped).await {
                return;
            }
        }
    } else {
        let err = codec.encode_error("timeout");
        let _ = send_feature_response(send(err), &mut stopped).await;
        return;
    }

    loop {
        if *stopped.borrow() {
            break;
        }
        let data = tokio::select! {
            data = recv() => match data {
                Some(data) => data,
                None => break,
            },
            changed = stopped.changed() => {
                // `shutdown()` always publishes true before waking lifecycle
                // waiters.  A closed sender also means the owning FC is gone.
                if changed.is_err() || *stopped.borrow() {
                    break;
                }
                continue;
            }
        };
        match codec.parse_request(&data) {
            Ok((action, params)) => {
                if let Err(err) = dispatch(action, params).await {
                    if !send_feature_response(
                        send(codec.encode_invalid_message(&err)),
                        &mut stopped,
                    )
                    .await
                    {
                        break;
                    }
                }
            }
            Err(err) => {
                if !send_feature_response(send(codec.encode_invalid_message(&err)), &mut stopped)
                    .await
                {
                    break;
                }
            }
        }
    }
}

async fn send_feature_response(
    send: impl std::future::Future<Output = Result<(), String>>,
    stopped: &mut tokio::sync::watch::Receiver<bool>,
) -> bool {
    if *stopped.borrow() {
        return false;
    }
    tokio::select! {
        biased;
        _ = stopped.changed() => false,
        result = tokio::time::timeout(Duration::from_secs(10), send) =>
            matches!(result, Ok(Ok(()))),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn shutdown_is_sticky_and_interrupts_connection_acquisition() {
        let fc = FeatureConnection::new(
            "device".into(),
            "identify".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        let silent = FeatureConnection::new(
            "silent".into(),
            "identify".into(),
            "test".into(),
            fc.config.clone(),
        );
        silent.shutdown();
        assert!(
            *silent.stopped_rx().borrow(),
            "late subscribers must observe shutdown without prior receivers"
        );
        fc.lifecycle_started.store(true, Ordering::SeqCst);
        let waiter = tokio::spawn(acquire_connected_feature(
            fc.clone(),
            Duration::from_secs(30),
        ));
        tokio::task::yield_now().await;
        fc.shutdown();
        let result = tokio::time::timeout(Duration::from_millis(100), waiter)
            .await
            .unwrap()
            .unwrap();
        assert!(result.err().unwrap().contains("is being reconfigured"));
        let late = fc.stopped_rx();
        assert!(*late.borrow());
        drop(late);
        fc.shutdown();
        assert!(*fc.stopped_rx().borrow());
    }

    #[tokio::test]
    async fn feature_stop_interrupts_blocked_event_responses() {
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let pending = super::send_feature_response(std::future::pending(), &mut stopped);
        let (sent, ()) = tokio::join!(pending, async {
            stop.send(true).unwrap();
        });
        assert!(!sent);
        assert!(!super::send_feature_response(std::future::ready(Ok(())), &mut stopped).await);
    }

    #[tokio::test]
    async fn failed_event_responses_are_terminal() {
        let (_stop, mut stopped) = tokio::sync::watch::channel(false);
        assert!(
            !super::send_feature_response(std::future::ready(Err("closed".into())), &mut stopped)
                .await
        );
    }

    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{acquire_connected_feature, feature_connect_status};
    use crate::bmc::feature::config::{FeatureConfig, FeatureParams};
    use crate::bmc::feature::connection::{spawn_lifecycle, FeatureConnection};
    use crate::bmc::feature::fault::{FeatureFault, FeatureFaultDomain};

    #[tokio::test]
    async fn acquisition_returns_an_active_lifecycle_fault_without_waiting_for_timeout() {
        let fc = FeatureConnection::new(
            "device".to_string(),
            "msd".to_string(),
            "backend".to_string(),
            FeatureConfig {
                backend: "backend".to_string(),
                backend_params: Default::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        );
        fc.lifecycle_started.store(true, Ordering::SeqCst);
        fc.faults
            .record(FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "session_in_use",
                "The backend session is already in use",
                true,
            ))
            .await;

        let result = tokio::time::timeout(
            Duration::from_millis(100),
            acquire_connected_feature(fc, Duration::from_secs(15)),
        )
        .await
        .expect("active fault must terminate acquisition immediately");
        let result = match result {
            Ok(_) => panic!("active fault unexpectedly acquired the feature"),
            Err(error) => error,
        };
        assert!(result.contains("session_in_use"));
        assert_eq!(
            feature_connect_status(&result),
            axum::http::StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn operation_fault_does_not_prevent_reacquisition() {
        let fc = FeatureConnection::new(
            "device".to_string(),
            "boot".to_string(),
            "backend".to_string(),
            FeatureConfig {
                backend: "backend".to_string(),
                backend_params: Default::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        );
        fc.faults
            .record(FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "boot_operation_failed",
                "The boot backend rejected the requested operation",
                true,
                "status",
            ))
            .await;
        spawn_lifecycle(fc.clone(), Arc::new(|_| Box::pin(async { Ok(()) })));

        let guard = tokio::time::timeout(
            Duration::from_millis(250),
            acquire_connected_feature(fc.clone(), Duration::from_secs(15)),
        )
        .await
        .expect("operation fault must not stall reacquisition")
        .expect("operation fault must not reject reacquisition");

        assert_eq!(guard.fc().ref_count(), 1);
        assert!(fc.faults.active().await.is_none());
    }
}
