use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::warn;

use crate::bmc::connection::feature::{FeatureConnection, FeatureGuard};
use crate::bmc::connection::transport::{TransportAdapter, TransportState, connect_task as spawn_transport_task};
use crate::bmc::connector::PROTOCOL_BACKENDS;
use crate::bmc::encoder::backend::create_shared_encoder;
use crate::bmc::session::managed::ManagedDevice;
use crate::bmc::session::types::DeviceSession;
use crate::bmc::traits::DeviceInfo;
use crate::bmc::registry::find_backend;
use crate::config::DeviceConfig;

pub struct DeviceRegistry {
    pub(crate) devices: Arc<RwLock<HashMap<String, Arc<ManagedDevice>>>>,
    pub(crate) keepalive_guards: Arc<RwLock<HashMap<String, FeatureGuard>>>,
}

impl DeviceRegistry {
    pub fn new() -> Self {
        Self {
            devices: Arc::new(RwLock::new(HashMap::new())),
            keepalive_guards: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn init(&self, configs: &[DeviceConfig]) {
        let mut devs = self.devices.write().await;
        for cfg in configs {
            if devs.contains_key(&cfg.id) {
                warn!("[{}] duplicate device id, skipping", cfg.id);
                continue;
            }

            let encoder = create_shared_encoder(&cfg.encoder_backend);
            let (dev, _transports) = Self::build_device(cfg.clone(), encoder);
            devs.insert(cfg.id.clone(), dev);
        }
    }

    /// Inject a pre-connected device (for tests / mock backends).
    /// Creates a virtual "injected" transport with all features, and
    /// immediately sets the session as connected.
    pub async fn inject_device(
        &self, id: String, name: String, protocol: String,
        session: DeviceSession,
    ) {
        // If already exists, just overwrite
        let mut devs = self.devices.write().await;

        let encoder = create_shared_encoder("software");
        let cfg = DeviceConfig {
            id: id.clone(),
            name,
            protocol,
            host: String::new(),
            port: 0,
            kvm_port: None,
            ipmi_host: None,
            ipmi_port: None,
            max_fps: 30,
            video_backend: "injected".into(),
            encoder_backend: "software".into(),
            audio_backend: None,
            video_device: None,
            video_format: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            video_quality: None,
            auth: crate::config::AuthConfig { username: String::new(), password: String::new() },
        };

        // Create a single transport for all features
        let ts = TransportState::new(
            Arc::new(crate::bmc::connection::transport::InjectedTransport::new(&id)),
        );
        // Set the session immediately
        *ts.session.write().await = Some(session);
        ts.connected.send_replace(true);

        // Create feature connections for known feature IDs
        let feature_ids = ["video", "hid", "msd", "power"];
        let mut all_features = HashMap::new();
        for &fid in &feature_ids {
            all_features.insert(fid.to_string(), FeatureConnection::new(fid.to_string(), &ts));
        }

        let dev = ManagedDevice::new(
            cfg,
            encoder,
            all_features,
            vec![ts],
        );
        devs.insert(id, dev);
    }

    fn build_device(
        cfg: DeviceConfig,
        encoder: crate::bmc::encoder::backend::SharedEncoder,
    ) -> (Arc<ManagedDevice>, Vec<Arc<TransportState>>) {
        let backend = match find_backend(PROTOCOL_BACKENDS, &cfg.protocol) {
            Some(b) => b,
            None => {
                warn!("[{}] unknown protocol '{}', no connections created", cfg.id, cfg.protocol);
                let dev = ManagedDevice::new(cfg, encoder, HashMap::new(), Vec::new());
                return (dev, Vec::new());
            }
        };

        let cfg = Arc::new(cfg);
        let mut transport_states: Vec<Arc<TransportState>> = Vec::new();
        let mut all_features: HashMap<String, Arc<FeatureConnection>> = HashMap::new();
        let mut transport_features: Vec<Vec<Arc<FeatureConnection>>> = Vec::new();

        // Phase 1: Create all transport states
        for ct in backend.connections {
            let adapter = TransportAdapter::new(ct);
            let ts = TransportState::new(Arc::new(adapter));
            transport_states.push(ts);
        }

        // Phase 2: Create all feature connections (with Weak refs to transport states)
        for (i, ct) in backend.connections.iter().enumerate() {
            let mut feats = Vec::new();
            for &fid in ct.features {
                let fid_str = fid.to_string();
                let fc = FeatureConnection::new(fid_str.clone(), &transport_states[i]);
                all_features.entry(fid_str).or_insert_with(|| fc.clone());
                feats.push(fc);
            }
            transport_features.push(feats);
        }

        // Phase 3: Create managed device
        let enc_for_spawn = encoder.clone();
        let dev = ManagedDevice::new(
            (*cfg).clone(),
            encoder,
            all_features,
            transport_states.clone(),
        );

        // Phase 4: Spawn connect_tasks
        for (i, _ct) in backend.connections.iter().enumerate() {
            let ts = transport_states[i].clone();
            let feats = transport_features[i].clone();
            let cfg2 = cfg.clone();
            let enc2 = enc_for_spawn.clone();
            tokio::spawn(async move {
                spawn_transport_task(ts, feats, cfg2, enc2).await;
            });
        }

        (dev, transport_states)
    }

    pub(crate) async fn get_dev(&self, id: &str) -> Option<Arc<ManagedDevice>> {
        self.devices.read().await.get(id).cloned()
    }

    pub async fn list_devices(&self) -> Vec<DeviceInfo> {
        let devs = self.devices.read().await;
        let mut result = Vec::with_capacity(devs.len());
        for (id, dev) in devs.iter() {
            let online = dev.get_base().await.is_some();
            result.push(DeviceInfo {
                id: id.clone(),
                name: dev.cfg.name.clone(),
                width: 0,
                height: 0,
                online,
            });
        }
        result
    }

    pub async fn device_count(&self) -> usize {
        self.devices.read().await.len()
    }

    pub async fn is_registered(&self, id: &str) -> bool {
        self.devices.read().await.contains_key(id)
    }

    pub async fn device_info(&self, id: &str) -> Option<DeviceInfo> {
        let dev = self.get_dev(id).await?;
        let (width, height) = match self.get_session(id).await {
            Some(session) => match session.video() {
                Some(video) => {
                    let info = video.info().await;
                    (info.width, info.height)
                }
                None => (0, 0),
            },
            None => (0, 0),
        };
        Some(DeviceInfo {
            id: dev.cfg.id.clone(),
            name: dev.cfg.name.clone(),
            width,
            height,
            online: dev.get_base().await.is_some(),
        })
    }

    /// Get the current (best-available) DeviceSession for a device.
    /// Returns None if no transport is connected.
    pub async fn get_session(&self, id: &str) -> Option<DeviceSession> {
        let dev = self.get_dev(id).await?;
        for ts in &dev.transports {
            if *ts.connected.borrow() {
                let guard = ts.session.read().await;
                if let Some(ref s) = *guard {
                    return Some(s.clone());
                }
            }
        }
        None
    }

    /// Get a specific feature's connection events receiver.
    pub async fn subscribe_feature_events(
        &self, id: &str, feature: &str,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::bmc::connection::event::ConnectionEvent>> {
        let dev = self.get_dev(id).await?;
        dev.subscribe(feature)
    }
}
