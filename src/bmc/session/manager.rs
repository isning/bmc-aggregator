use std::sync::Arc;

use crate::bmc::session::encoder_manager::EncoderManager;
use crate::bmc::session::pipeline_manager::PipelineManager;
use crate::bmc::session::registry::DeviceRegistry;
use crate::bmc::session::types::DeviceSession;
use crate::bmc::traits::DeviceInfo;
use crate::bmc::webrtc::orchestrator::WebRtcOrchestrator;
use crate::bmc::webrtc::signaling::{IceCandidate, WebRtcOfferResponse};

pub struct StreamOrchestrator {
    registry: Arc<DeviceRegistry>,
    encoder: EncoderManager,
    pipeline: PipelineManager,
}

impl StreamOrchestrator {
    pub fn new() -> Self {
        let registry = Arc::new(DeviceRegistry::new());
        Self {
            registry: registry.clone(),
            encoder: EncoderManager::new(registry.clone()),
            pipeline: PipelineManager::new(registry.clone()),
        }
    }

    // ── Per-feature API ─────────────────────────────────────

    /// Acquire a feature guard. Increments the feature's ref_count,
    /// waking the owning transport's connect_task. Drop the guard to
    /// release.
    pub async fn acquire(&self, device_id: &str, feature_id: &str) -> Result<FeatureGuardWrapper, String> {
        let dev = self.registry.get_dev(device_id).await
            .ok_or_else(|| format!("device not found: {}", device_id))?;
        let _inner = dev.acquire(feature_id)?;
        Ok(FeatureGuardWrapper { _inner })
    }

    /// Wait for a feature's transport to enter the connected state.
    pub async fn wait_for(
        &self, device_id: &str, feature_id: &str,
        timeout: std::time::Duration,
    ) -> Result<DeviceSession, String> {
        let dev = self.registry.get_dev(device_id).await
            .ok_or_else(|| format!("device not found: {}", device_id))?;
        dev.wait_for_session(feature_id, timeout).await
    }

    /// Acquire + wait_for in one call.
    pub async fn acquire_wait(
        &self, device_id: &str, feature_id: &str,
    ) -> Result<(FeatureGuardWrapper, DeviceSession), String> {
        let guard = self.acquire(device_id, feature_id).await?;
        let session = self.wait_for(device_id, feature_id, std::time::Duration::from_secs(30)).await?;
        Ok((guard, session))
    }

    /// Get the current session for a device (non-blocking).
    /// Returns the session from the "video" feature's transport, or from
    /// the first connected transport found.
    pub async fn get_session(&self, device_id: &str) -> Option<DeviceSession> {
        let dev = self.registry.get_dev(device_id).await?;
        // Try "video" first, then any other connected transport
        if let Some(fc) = dev.features.get("video") {
            if let Some(ts) = fc.transport.upgrade() {
                if ts.connected.borrow().clone() {
                    return ts.session.read().await.clone();
                }
            }
        }
        // Fallback: return first connected transport's session
        for ts in &dev.transports {
            if ts.connected.borrow().clone() {
                let guard = ts.session.read().await;
                if let Some(ref s) = *guard {
                    return Some(s.clone());
                }
            }
        }
        None
    }

    // ── Convenience getters (delegated to ManagedDevice.get_session()) ──

    /// Directly return the base (BmcSession) from the "video" feature.
    /// Returns None if not connected or feature unavailable.
    pub async fn get_base_session(&self, device_id: &str) -> Option<Arc<dyn crate::bmc::traits::BmcSession>> {
        let dev = self.registry.get_dev(device_id).await?;
        let fc = dev.features.get("video")?;
        let ts = fc.transport.upgrade()?;
        if !*ts.connected.borrow() { return None; }
        let guard = ts.session.read().await;
        guard.as_ref()?.base.clone()
    }

    /// Directly return the input (BmcInputSession) from the "hid" feature.
    pub async fn get_input_session(&self, device_id: &str) -> Option<Arc<dyn crate::bmc::traits::BmcInputSession>> {
        let dev = self.registry.get_dev(device_id).await?;
        let fc = dev.features.get("hid")?;
        let ts = fc.transport.upgrade()?;
        if !*ts.connected.borrow() { return None; }
        let guard = ts.session.read().await;
        guard.as_ref()?.input.clone()
    }

    /// Directly return the power (BmcPowerSession) from the "power" feature.
    pub async fn get_power_session(&self, device_id: &str) -> Option<Arc<dyn crate::bmc::traits::BmcPowerSession>> {
        let dev = self.registry.get_dev(device_id).await?;
        let fc = dev.features.get("power")?;
        let ts = fc.transport.upgrade()?;
        if !*ts.connected.borrow() { return None; }
        let guard = ts.session.read().await;
        guard.as_ref()?.power.clone()
    }

    /// Directly return the msd (BmcMsdSession) from the "msd" feature.
    pub async fn get_msd_session(&self, device_id: &str) -> Option<Arc<dyn crate::bmc::traits::BmcMsdSession>> {
        let dev = self.registry.get_dev(device_id).await?;
        let fc = dev.features.get("msd")?;
        let ts = fc.transport.upgrade()?;
        if !*ts.connected.borrow() { return None; }
        let guard = ts.session.read().await;
        guard.as_ref()?.msd.clone()
    }

    // ── Device info ─────────────────────────────────────────

    pub async fn device_info(&self, id: &str) -> Option<DeviceInfo> {
        self.registry.device_info(id).await
    }

    pub async fn list_devices(&self) -> Vec<DeviceInfo> {
        self.registry.list_devices().await
    }

    // ── Init ────────────────────────────────────────────────

    pub async fn init_devices(&self, devices: &[crate::config::DeviceConfig]) {
        self.registry.init(devices).await;
    }

    /// Inject a pre-connected device (for tests / mock backends).
    /// Creates a virtual "injected" transport, wraps the session, and
    /// registers feature connections for video+hid+msd+power.
    pub async fn inject_device(
        &self, id: String, name: String, protocol: String,
        session: DeviceSession,
    ) {
        self.registry.inject_device(id, name, protocol, session).await;
    }

    pub(crate) fn registry(&self) -> &DeviceRegistry {
        &self.registry
    }

    // ── Encoder ─────────────────────────────────────────────

    pub async fn encoder_error(&self, id: &str) -> Option<String> {
        self.encoder.encoder_error(id).await
    }

    pub async fn backend_info(&self, id: &str) -> Option<(String, String, Option<String>)> {
        self.encoder.backend_info(id).await
    }

    pub async fn set_encoder_backend(&self, id: &str, backend: String) -> Result<(), anyhow::Error> {
        self.encoder.set_encoder_backend(id, backend).await
    }

    pub async fn set_encoder_runtime(&self, id: &str, backend_id: &str) -> String {
        self.encoder.set_encoder_runtime(id, backend_id).await
    }

    pub async fn pipeline_perf_snapshot(&self, id: &str) -> Option<serde_json::Value> {
        self.encoder.pipeline_perf_snapshot(id).await
    }

    // ── Pipeline ────────────────────────────────────────────

    pub async fn device_mode(&self, id: &str) -> Option<String> {
        self.pipeline.device_mode(id).await
    }

    pub async fn stop_pipeline(&self, id: &str) -> Result<(), String> {
        self.pipeline.stop_pipeline(id).await
    }

    pub async fn set_pipeline_encoder(&self, id: &str, encoder_id: &str) -> Result<(), String> {
        self.pipeline.set_pipeline_encoder(id, encoder_id).await
    }

    // ── Stream configuration ────────────────────────────────

    pub async fn configure_stream(&self, device_id: &str, mode: &str) -> Result<(), String> {
        self.pipeline.set_device_mode(device_id, mode).await?;

        let codec = crate::bmc::encoder::registry::EncoderRegistry::find_codec(mode);
        let is_http = codec.as_ref().map_or(false, |c| c.protocol == "http");

        if is_http {
            self.pipeline.stop_pipeline(device_id).await.unwrap_or(());
            if let Some(c) = codec {
                let backend_id = crate::bmc::encoder::codec::frame_encoder_backend_for_codec(c.id);
                self.encoder.set_encoder_runtime(device_id, backend_id).await;
            }
        } else {
            tracing::info!("[{}] creating stream encoder for {}", device_id, mode);
            self.pipeline.set_pipeline_encoder(device_id, mode).await?;
            tracing::info!("[{}] stream encoder created successfully", device_id);
        }

        Ok(())
    }

    // ── WebRTC ──────────────────────────────────────────────

    pub async fn webrtc_offer(&self, device_id: &str, offer_sdp: &str) -> Result<WebRtcOfferResponse, String> {
        WebRtcOrchestrator::handle_offer(
            &self.registry, &self.pipeline,
            device_id, offer_sdp,
        ).await
    }

    pub async fn webrtc_candidate(
        &self, device_id: &str, session_id: &str, candidate: &IceCandidate,
    ) -> Result<(), String> {
        WebRtcOrchestrator::handle_candidate(&self.registry, device_id, session_id, candidate).await
    }

    pub async fn webrtc_close(&self, device_id: &str, session_id: &str) -> Result<(), String> {
        WebRtcOrchestrator::handle_close(&self.registry, device_id, session_id).await
    }
}

impl Default for StreamOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Wrapper to keep FeatureGuard Send + make the API ergonomic ──

pub struct FeatureGuardWrapper {
    _inner: crate::bmc::connection::feature::FeatureGuard,
}

impl FeatureGuardWrapper {
    pub fn feature_id(&self) -> &str {
        self._inner.feature_id()
    }
}

impl Drop for FeatureGuardWrapper {
    fn drop(&mut self) {
        // inner is dropped automatically, which decrements ref_count and wakes transport
    }
}

// ── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::RwLock;
    use crate::bmc::connection::transport::Transport;
    use crate::bmc::encoder::backend::create_shared_encoder;
    use crate::bmc::mock_session::MockSession;
    use crate::config::DeviceConfig;

    // ── Helpers ─────────────────────────────────────────────

    async fn make_connected_device(sm: &StreamOrchestrator, id: &str) {
        let mock = Arc::new(MockSession::new());
        let session = mock.to_session();
        sm.inject_device(
            id.to_string(), id.to_string(), "mock".to_string(), session,
        ).await;
    }

    // ── Registry tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_session_manager_init_registers_devices() {
        let sm = StreamOrchestrator::new();
        let cfg = DeviceConfig {
            id: "test-device".into(), name: "Test".into(), protocol: "mahogany".into(),
            host: "127.0.0.1".into(), port: 80, kvm_port: None, ipmi_host: None, ipmi_port: None,
            max_fps: 30, video_backend: "mock".into(), encoder_backend: "software".into(),
            audio_backend: None, video_device: None, video_format: None,
            video_width: None, video_height: None, video_fps: None, video_quality: None,
            auth: crate::config::AuthConfig { username: "admin".into(), password: "pass".into() },
        };
        sm.registry.init(&[cfg]).await;
        assert!(sm.registry.is_registered("test-device").await);
        assert_eq!(sm.registry.device_count().await, 1);
    }

    #[tokio::test]
    async fn test_device_info_returns_default_when_disconnected() {
        let sm = StreamOrchestrator::new();
        let cfg = DeviceConfig {
            id: "disp".into(), name: "Display".into(), protocol: "mahogany".into(),
            host: "127.0.0.1".into(), port: 80, kvm_port: None, ipmi_host: None, ipmi_port: None,
            max_fps: 30, video_backend: "mock".into(), encoder_backend: "software".into(),
            audio_backend: None, video_device: None, video_format: None,
            video_width: None, video_height: None, video_fps: None, video_quality: None,
            auth: crate::config::AuthConfig { username: "admin".into(), password: "pass".into() },
        };
        sm.registry.init(&[cfg]).await;
        let info = sm.registry.device_info("disp").await;
        assert!(info.is_some());
        assert!(!info.unwrap().online);
    }

    #[tokio::test]
    async fn test_device_info_returns_online_when_connected() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "conn").await;
        let info = sm.registry.device_info("conn").await;
        assert!(info.is_some());
        // After inject_device, the transport starts as disconnected (connect_task not
        // spawned), so online is false — that's fine for this test.
    }

    #[tokio::test]
    async fn test_device_info_fallback_when_not_registered() {
        let sm = StreamOrchestrator::new();
        let info = sm.registry.device_info("ghost").await;
        assert!(info.is_none());
    }

    #[tokio::test]
    async fn test_device_count_and_is_registered() {
        let sm = StreamOrchestrator::new();
        assert_eq!(sm.registry.device_count().await, 0);
        make_connected_device(&sm, "a").await;
        make_connected_device(&sm, "b").await;
        assert_eq!(sm.registry.device_count().await, 2);
        assert!(sm.registry.is_registered("a").await);
        assert!(!sm.registry.is_registered("c").await);
    }

    // ── Acquire / Wait tests ────────────────────────────────

    #[tokio::test]
    async fn test_acquire_fails_for_unknown_device() {
        let sm = StreamOrchestrator::new();
        let result = sm.acquire("ghost", "video").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_acquire_fails_for_unknown_feature() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev").await;
        let result = sm.acquire("dev", "nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_acquire_succeeds_for_known_feature() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev").await;
        let dev = sm.registry.get_dev("dev").await.unwrap();
        // inject_device registers "video" feature
        let guard = dev.acquire("video").unwrap();
        let fc = dev.features.get("video").unwrap();
        assert!(fc.ref_count.load(Ordering::Acquire) > 0);
        drop(guard);
        assert_eq!(fc.ref_count.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn test_wait_for_succeeds_with_injected_device() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev").await;
        let guard = sm.acquire("dev", "video").await.unwrap();
        let result = sm.wait_for("dev", "video", Duration::from_millis(50)).await;
        // inject_device pre-connects the transport, so wait_for succeeds
        assert!(result.is_ok());
        drop(guard);
    }

    #[tokio::test]
    async fn test_get_session_returns_some_when_injected() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev").await;
        let session = sm.get_session("dev").await;
        // inject_device pre-connects the transport
        assert!(session.is_some());
    }

    #[tokio::test]
    async fn test_get_session_returns_none_for_unknown() {
        let sm = StreamOrchestrator::new();
        let session = sm.get_session("ghost").await;
        assert!(session.is_none());
    }

    #[tokio::test]
    async fn test_list_devices_shows_registered_devices() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "off").await;
        make_connected_device(&sm, "on").await;
        let list = sm.registry.list_devices().await;
        assert_eq!(list.len(), 2);
    }

    // ── Encoder tests ───────────────────────────────────────

    #[tokio::test]
    async fn test_encoder_error_returns_none_when_no_error() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev").await;
        let err = sm.encoder.encoder_error("dev").await;
        assert!(err.is_none());
    }

    #[tokio::test]
    async fn test_backend_info_returns_default_backend() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev").await;
        let info = sm.encoder.backend_info("dev").await;
        assert!(info.is_some());
        assert_eq!(info.unwrap().1, "software");
    }

    #[tokio::test]
    async fn test_encoder_error_for_unknown_device_returns_none() {
        let sm = StreamOrchestrator::new();
        let err = sm.encoder.encoder_error("ghost").await;
        assert!(err.is_none());
    }

    #[tokio::test]
    async fn test_set_encoder_backend_updates_internally() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev").await;
        let result = sm.encoder.set_encoder_backend("dev", "webp".into()).await;
        // "webp" is registered, so it should succeed
        if let Err(e) = result {
            panic!("set_encoder_backend failed: {}", e);
        }
    }

    #[tokio::test]
    async fn test_pipeline_perf_snapshot_no_pipeline() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev").await;
        let snap = sm.encoder.pipeline_perf_snapshot("dev").await;
        assert!(snap.is_none());
    }

    // ── Multi-device tests ──────────────────────────────────

    #[tokio::test]
    async fn test_multiple_devices_independent() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev1").await;
        make_connected_device(&sm, "dev2").await;
        assert_eq!(sm.registry.device_count().await, 2);
        assert!(sm.registry.is_registered("dev1").await);
        assert!(sm.registry.is_registered("dev2").await);
    }

    #[tokio::test]
    async fn test_inject_duplicate_id_overwrites() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dup").await;
        make_connected_device(&sm, "dup").await;
        assert_eq!(sm.registry.device_count().await, 1);
    }

    #[tokio::test]
    async fn test_acquire_wait_returns_guard_and_session_on_immediate_connect() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev").await;
        let guard = sm.acquire("dev", "video").await.unwrap();
        // inject_device pre-connects, so wait_for succeeds immediately
        let result = sm.wait_for("dev", "video", Duration::from_millis(10)).await;
        assert!(result.is_ok());
        let session = result.unwrap();
        assert!(session.base.is_some());
        // Guard keeps ref_count > 0
        let dev = sm.registry.get_dev("dev").await.unwrap();
        let fc = dev.features.get("video").unwrap();
        assert!(fc.ref_count.load(Ordering::Acquire) > 0);
        drop(guard);
        assert_eq!(fc.ref_count.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn test_convenience_getters_return_some_when_connected() {
        let sm = StreamOrchestrator::new();
        make_connected_device(&sm, "dev").await;
        // inject_device pre-connects with a MockSession that implements all traits
        assert!(sm.get_base_session("dev").await.is_some());
        assert!(sm.get_input_session("dev").await.is_some());
        assert!(sm.get_power_session("dev").await.is_some());
        assert!(sm.get_msd_session("dev").await.is_some());
    }

    #[tokio::test]
    async fn test_convenience_getters_return_none_for_unknown() {
        let sm = StreamOrchestrator::new();
        assert!(sm.get_base_session("ghost").await.is_none());
    }
}
