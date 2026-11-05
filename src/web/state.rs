use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::bmc::feature::device_registry::DeviceRegistry;
use crate::bmc::feature::video::webtransport::WebTransportRuntimeState;
use crate::bmc::msd_storage::FsMsdStorage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsdDownloadStatus {
    Downloading,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}

impl MsdDownloadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Downloading => "downloading",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

/// A bounded, device-local record for an asynchronous MSD image download.
/// Terminal records remain available for status polling until a later download
/// needs their slot. Status owns worker liveness; consuming the cancellation
/// sender does not release the worker slot.
pub struct MsdDownloadTask {
    pub device_id: String,
    pub cancellation: Option<tokio::sync::oneshot::Sender<()>>,
    pub filename: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub status: MsdDownloadStatus,
    pub fault: Option<crate::bmc::feature::fault::FeatureFault>,
    pub started_at: std::time::Instant,
}

pub struct AppState {
    pub registry: Arc<DeviceRegistry>,
    /// Shared with storage so a persisted device/MSD configuration update is
    /// immediately visible to both route handlers and filesystem resolution.
    pub config: Arc<RwLock<crate::config::Config>>,
    pub config_path: String,
    pub configuration: crate::bmc::configuration::ConfigurationCoordinator,
    pub msd_storage: FsMsdStorage,
    pub active_downloads: RwLock<HashMap<String, MsdDownloadTask>>,
    pub webtransport: RwLock<Option<WebTransportRuntimeState>>,
}

impl AppState {
    /// Persist a complete candidate before publishing it in memory. This keeps
    /// readers and the device registry on the last durable configuration when
    /// serialization or filesystem operations fail.
    pub(crate) async fn commit_config_scoped(
        self: &Arc<Self>,
        candidate: crate::config::Config,
        changed_scopes: &[crate::bmc::configuration::ConfigurationScope],
    ) -> Result<crate::bmc::feature::device_registry::ReconfigurationReport, String> {
        let state = self.clone();
        let changed_scopes = changed_scopes.to_vec();
        finish_task(tokio::spawn(async move {
            let path = state.config_path.clone();
            let durable = candidate.clone();
            tokio::task::spawn_blocking(move || durable.save(&path))
                .await
                .map_err(|error| format!("configuration save task failed: {error}"))?
                .map_err(|error| error.to_string())?;
            let devices = candidate.devices.clone();
            let publication_state = &state;
            let report = state
                .registry
                .replace_all_with_publication(
                    &devices,
                    crate::bmc::feature::registered_backends(),
                    |report| {
                        let mut scopes = changed_scopes.to_vec();
                        for (device_id, feature_ids) in &report.affected_features {
                            for feature_id in feature_ids {
                                let scope =
                                    crate::bmc::configuration::ConfigurationScope::Feature {
                                        device_id: device_id.clone(),
                                        feature_id: feature_id.clone(),
                                    };
                                if !scopes.contains(&scope) {
                                    scopes.push(scope);
                                }
                            }
                        }
                        async move {
                            let mut config = publication_state.config.write().await;
                            for scope in &scopes {
                                publication_state.configuration.advance_scope(scope);
                            }
                            *config = candidate;
                            config
                        }
                    },
                )
                .await;
            Ok(report)
        }))
        .await
    }
}

/// Return only the device-independent portion of the legacy feature response.
///
/// Feature availability and backend identity are populated by the device
/// registry in the handler. Keeping this base deliberately small prevents a
/// local aggregator capability (or a historical protocol default) from being
/// reported as a capability of every BMC device.
pub(crate) fn build_platform_capabilities() -> serde_json::Value {
    serde_json::json!({"mode": "bmc_aggregator"})
}

/// Once a mutation starts, its owner completes publication even if the HTTP caller disappears.
pub(crate) async fn finish_task<T>(
    task: tokio::task::JoinHandle<Result<T, String>>,
) -> Result<T, String> {
    task.await
        .map_err(|_| "mutation worker failed".to_string())?
}
pub(crate) async fn mutation_response(
    task: tokio::task::JoinHandle<axum::response::Response>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    task.await.unwrap_or_else(|_| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({"success": false, "message": "Mutation worker failed"})),
        )
            .into_response()
    })
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn reconfiguration_does_not_hold_config_write_while_draining_backend_effects() {
        use crate::bmc::feature::registry::FeatureBackendTemplate;
        let directory = tempfile::tempdir().unwrap();
        let mut config: crate::config::Config = serde_yaml::from_str("server:\n  listen: 127.0.0.1:12340\n  frontend_dir: frontend/dist\ndevices:\n  - id: device\n    name: Test\n    protocol: explicit\n    host: 127.0.0.1\n    port: 443\n    auth: {username: fixture, password: fixture}\n    features:\n      video: {backend: fixture}\nmsd: {}\n").unwrap();
        config.normalize_feature_config();
        let registry = crate::bmc::feature::device_registry::DeviceRegistry::new();
        registry
            .init(
                &config.devices,
                &[FeatureBackendTemplate {
                    backend: "fixture",
                    feature: "video",
                    connect: Arc::new(|_| Box::pin(async { Ok(()) })),
                    requirements: None,
                }],
            )
            .await;
        let fc = registry.get_dev("device").await.unwrap().features["video"].clone();
        let mut candidate = config.clone();
        candidate.devices[0].host = "192.0.2.1".into();
        let config = Arc::new(RwLock::new(config));
        let app = Arc::new(AppState {
            registry,
            config: config.clone(),
            config_path: directory
                .path()
                .join("config.yaml")
                .to_string_lossy()
                .into_owned(),
            configuration: crate::bmc::configuration::ConfigurationCoordinator::new(),
            msd_storage: FsMsdStorage::new(
                directory
                    .path()
                    .join("storage")
                    .to_string_lossy()
                    .into_owned(),
                config.clone(),
            ),
            active_downloads: RwLock::new(Default::default()),
            webtransport: RwLock::new(None),
        });
        let effect = fc
            .backend_owned_effect_guard(fc.backend_generation())
            .await
            .unwrap();
        let commit = tokio::spawn({
            let app = app.clone();
            async move { app.commit_config_scoped(candidate, &[]).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !fc.is_stopped() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let readable =
            tokio::time::timeout(std::time::Duration::from_millis(200), config.read()).await;
        assert!(
            readable.is_ok(),
            "configuration reloading blocked the read needed by an admitted backend effect"
        );
        assert_eq!(readable.unwrap().devices[0].host, "127.0.0.1");
        drop(effect);
        tokio::time::timeout(std::time::Duration::from_secs(2), commit)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(config.read().await.devices[0].host, "192.0.2.1");
    }

    #[tokio::test]
    async fn cancellation_after_durable_save_still_publishes_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let config: crate::config::Config = serde_yaml::from_str("server:\n  listen: 127.0.0.1:12340\n  frontend_dir: frontend/dist\ndevices: []\nmsd: {}\n").unwrap();
        config.save(path.to_str().unwrap()).unwrap();
        let mut candidate = config.clone();
        candidate.server.listen = "127.0.0.1:12341".into();
        let config = Arc::new(RwLock::new(config));
        let app = Arc::new(AppState {
            registry: crate::bmc::feature::device_registry::DeviceRegistry::new(),
            config: config.clone(),
            config_path: path.to_string_lossy().into_owned(),
            configuration: crate::bmc::configuration::ConfigurationCoordinator::new(),
            msd_storage: FsMsdStorage::new(
                dir.path().join("storage").to_string_lossy().into_owned(),
                config.clone(),
            ),
            active_downloads: RwLock::new(Default::default()),
            webtransport: RwLock::new(None),
        });
        let memory_lock = config.write().await;
        let request = tokio::spawn(async move { app.commit_config_scoped(candidate, &[]).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if crate::config::Config::load(path.to_str().unwrap())
                    .unwrap()
                    .server
                    .listen
                    .ends_with("12341")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        request.abort();
        let _ = request.await;
        drop(memory_lock);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if config.read().await.server.listen.ends_with("12341") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
