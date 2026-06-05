pub mod auth;
pub mod config;
pub mod device;
pub mod features;
pub mod hid;
pub mod misc;
pub mod msd;
pub mod power;
pub mod video;
pub mod webrtc;

pub use auth::*;
pub use features::*;
pub use config::*;
pub use power::*;
pub use device::*;
pub use hid::*;
pub use misc::*;
pub use msd::*;
pub use video::*;
pub use webrtc::*;

use std::collections::HashMap;
use axum::Json;
use tokio::sync::RwLock;

use crate::bmc::encoder::registry::EncoderRegistry;
use crate::bmc::msd_storage::FsMsdStorage;
use crate::bmc::session::manager::StreamOrchestrator;
pub use crate::bmc::msd_storage::MsdRuntimeState;

pub struct AppState {
    pub config: RwLock<crate::config::Config>,
    pub config_path: String,
    pub orchestrator: StreamOrchestrator,
    pub msd_runtime: RwLock<HashMap<String, MsdRuntimeState>>,
    pub msd_storage: FsMsdStorage,
    pub http_client: reqwest::Client,
    pub event_tx: tokio::sync::broadcast::Sender<String>,
    pub active_downloads: RwLock<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
}

impl AppState {
    pub(crate) async fn update_config<F>(&self, f: F) -> Result<(), String>
    where F: FnOnce(&mut crate::config::Config),
    {
        let mut cfg = self.config.write().await;
        f(&mut cfg);
        cfg.save(&self.config_path).map_err(|e| e.to_string())
    }

    pub(crate) async fn msd_runtime_for(&self, id: &str) -> MsdRuntimeState {
        if let Some(runtime) = self.msd_runtime.read().await.get(id).cloned() {
            return runtime;
        }
        let cfg = self.config.read().await;
        let device_cfg = cfg.msd.get(id).cloned().unwrap_or_default();
        MsdRuntimeState {
            backend: device_cfg.backend,
            mode: "drive".to_string(),
            connected: false,
            current_image: None,
        }
    }

    pub(crate) async fn update_msd_runtime<F>(&self, id: &str, f: F)
    where F: FnOnce(&mut MsdRuntimeState),
    {
        let mut map = self.msd_runtime.write().await;
        f(map.entry(id.to_string()).or_default());
    }
}

pub fn default_msd_storage_base() -> String {
    std::env::temp_dir().join("bmc-aggregator-msd").to_string_lossy().to_string()
}

pub(crate) fn build_platform_capabilities(
    video: &str, encoder: &str, audio: &Option<String>,
) -> (serde_json::Value, serde_json::Value) {
    let encoder_backends: Vec<serde_json::Value> = EncoderRegistry::discover()
        .iter()
        .map(|b| serde_json::json!({
            "id": b.id,
            "name": b.name,
            "is_hardware": b.is_hardware,
            "available": b.available,
        }))
        .collect();
    let stream_codecs: Vec<serde_json::Value> = EncoderRegistry::codecs()
        .iter()
        .map(|c| serde_json::json!({
            "id": c.id,
            "name": c.name,
            "protocol": c.protocol,
        }))
        .collect();

    let platform = serde_json::json!({
        "arch": "x86_64",
        "os": "linux",
        "machine": "aggregator",
        "mode": "bmc_aggregator",
        "mode_label": "BMC Aggregator",
        "video_capture": {
            "available": true,
            "backends": [video],
            "selected_backend": video
        },
        "encoder": {
            "available": true,
            "backends": encoder_backends,
            "selected_backend": encoder
        },
        "stream_codecs": stream_codecs,
        "hid": {
            "available": true,
            "backends": ["usb"],
            "selected_backend": "usb"
        },
        "atx": {
            "available": true,
            "backends": ["ipmi"],
            "selected_backend": "ipmi"
        },
        "msd": {"available": true, "backends": ["mahogany"], "selected_backend": "mahogany"},
        "otg": {"available": false, "backends": []},
        "audio": {
            "available": audio.is_some(),
            "backends": match audio { Some(v) => vec![v.as_str()], None => vec!["none"] },
            "selected_backend": audio.as_deref(),
            "reason": "aggregate_backend_no_audio"
        },
        "rustdesk": {"available": false, "backends": []},
        "diagnostics": {"available": false, "backends": []},
        "extensions": {"available": false, "backends": []},
        "service_installation": {"available": false, "backends": []}
    });
    let capabilities = serde_json::json!({
        "mode": "bmc_aggregator",
        "video": {"available": true, "backend": video},
        "video_capture": {"available": true, "type": "mjpeg", "backend": video},
        "encoder": {"available": true, "type": "software", "backend": encoder},
        "hid": {"available": true, "type": "usb", "backend": "usb"},
        "atx": {"available": true, "type": "ipmi", "backend": "ipmi"},
        "msd": {"available": true, "backend": "mahogany"},
        "otg": {"available": false},
        "audio": {"available": audio.is_some(), "backend": audio, "reason": "aggregate_backend_no_audio"},
        "rustdesk": {"available": false},
        "diagnostics": {"available": false},
        "extensions": {"available": false},
        "service_installation": {"available": false}
    });
    (platform, capabilities)
}

pub(crate) fn unavailable_json() -> Json<serde_json::Value> {
    Json(serde_json::json!({"available": false}))
}
