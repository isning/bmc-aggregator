use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::bmc::connection::event::ConnectionEventReceiver;
use crate::bmc::connection::feature::{FeatureConnection, FeatureGuard};
use crate::bmc::connection::state::ConnectionState;
use crate::bmc::connection::transport::TransportState;
use crate::bmc::encoder::backend::SharedEncoder;
use crate::bmc::session::types::DeviceSession;
use crate::bmc::traits::{BmcInputSession, BmcMsdSession, BmcPowerSession, BmcSession};
use crate::bmc::stream::pipeline::SharedVideoPipeline;
use crate::bmc::webrtc::session::WebRtcSession;
use crate::config::DeviceConfig;

/// Per-device runtime state. Holds all per-feature connections and shared resources.
#[allow(dead_code)]
pub(crate) struct ManagedDevice {
    pub cfg: DeviceConfig,
    pub encoder: SharedEncoder,
    /// Per-feature connection state (e.g. "video", "hid", "power")
    pub features: HashMap<String, Arc<FeatureConnection>>,
    /// Transport instances for this device
    pub(crate) transports: Vec<Arc<TransportState>>,
    /// Runtime-overridable encoder backend id
    pub current_encoder_backend: RwLock<String>,

    // Device-level shared resources (shared across features)
    pub name: String,
    pub protocol: String,
    pub video_backend: String,
    pub encoder_backend: String,
    pub audio_backend: Option<String>,
    pub pipeline: RwLock<Option<Arc<SharedVideoPipeline>>>,
    pub webrtc_sessions: Arc<RwLock<HashMap<String, Arc<WebRtcSession>>>>,
    pub mode: RwLock<String>,
    pub encoder_error: Arc<RwLock<Option<String>>>,
}

#[allow(dead_code)]
impl ManagedDevice {
    pub fn new(
        cfg: DeviceConfig,
        encoder: SharedEncoder,
        features: HashMap<String, Arc<FeatureConnection>>,
        transports: Vec<Arc<TransportState>>,
    ) -> Arc<Self> {
        let encoder_backend = cfg.encoder_backend.clone();
        let current_backend = encoder_backend.clone();
        Arc::new(ManagedDevice {
            cfg,
            encoder,
            features,
            transports,
            current_encoder_backend: RwLock::new(current_backend),
            name: String::new(),
            protocol: String::new(),
            video_backend: "mahogany".into(),
            encoder_backend,
            audio_backend: None,
            pipeline: RwLock::new(None),
            webrtc_sessions: Arc::new(RwLock::new(HashMap::new())),
            mode: RwLock::new("mjpeg".to_string()),
            encoder_error: Arc::new(RwLock::new(None)),
        })
    }

    /// Acquire a feature, incrementing its ref_count and waking its transport.
    /// Returns a FeatureGuard that keeps the connection alive.
    pub fn acquire(self: &Arc<Self>, feature_id: &str) -> Result<FeatureGuard, String> {
        let fc = self.features.get(feature_id)
            .ok_or_else(|| format!("feature '{}' not supported on '{}'", feature_id, self.cfg.id))?;
        Ok(FeatureGuard::new(fc.clone()))
    }

    /// Wait until a specific feature's transport has a session ready.
    pub async fn wait_for_session(&self, feature_id: &str, timeout: std::time::Duration) -> Result<DeviceSession, String> {
        let fc = self.features.get(feature_id)
            .ok_or_else(|| format!("feature '{}' not supported", feature_id))?;
        let ts = fc.transport.upgrade()
            .ok_or_else(|| "transport dropped".to_string())?;
        ts.wait_for_session(timeout).await
    }

    /// Subscribe to connection events for a specific feature.
    pub fn subscribe(&self, feature_id: &str) -> Option<ConnectionEventReceiver> {
        self.features.get(feature_id).map(|fc| fc.subscribe())
    }

    /// Get the feature's current connection state.
    pub async fn feature_state(&self, feature_id: &str) -> Option<ConnectionState> {
        self.features.get(feature_id).map(|fc| {
            // Blocking read is OK here — RwLock contention is negligible.
            Some(*fc.state.blocking_read())
        }).flatten()
    }

    // ── Typed session getters (convenience, use the transport) ──

    pub async fn get_base(&self) -> Option<Arc<dyn BmcSession>> {
        self.first_session().await.and_then(|s| s.base)
    }

    pub async fn get_input(&self) -> Option<Arc<dyn BmcInputSession>> {
        self.first_session().await.and_then(|s| s.input)
    }

    pub async fn get_msd(&self) -> Option<Arc<dyn BmcMsdSession>> {
        self.first_session().await.and_then(|s| s.msd)
    }

    pub async fn get_power(&self) -> Option<Arc<dyn BmcPowerSession>> {
        for ts in &self.transports {
            let guard = ts.session.read().await;
            if let Some(ref s) = *guard {
                if s.power.is_some() {
                    return s.power.clone();
                }
            }
        }
        None
    }

    async fn first_session(&self) -> Option<DeviceSession> {
        for ts in &self.transports {
            let guard = ts.session.read().await;
            if let Some(ref s) = *guard {
                return Some(s.clone());
            }
        }
        None
    }
}

impl std::fmt::Debug for ManagedDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedDevice")
            .field("id", &self.cfg.id)
            .field("features", &self.features.keys().collect::<Vec<_>>())
            .field("transports", &self.transports.iter().map(|t| &t.id).collect::<Vec<_>>())
            .finish()
    }
}
