
use crate::bmc::connection::feature::FeatureGuard;
use crate::bmc::session::registry::DeviceRegistry;
use crate::bmc::session::types::DeviceSession;
use crate::bmc::msd_storage::{MsdRuntimeState, MsdStorageProvider};

/// Coordinates MSD operations across HTTP handlers, session lifecycle,
/// and backend selection. Extracted from `msd.rs` handler to give
/// domain logic its own seam.
pub struct MsdManager;

impl MsdManager {
    /// Acquire a feature guard + session for MSD operations ("msd" feature).
    async fn acquire_msd(
        registry: &DeviceRegistry, id: &str,
    ) -> Result<(FeatureGuard, DeviceSession), String> {
        let dev = registry.get_dev(id).await.ok_or_else(|| format!("device not found: {}", id))?;
        let guard = dev.acquire("msd")?;
        let session = dev.wait_for_session("msd", std::time::Duration::from_secs(30)).await?;
        Ok((guard, session))
    }

    /// Connect MSD for a device. Returns `true` if keepalive should be held.
    pub async fn connect(
        registry: &DeviceRegistry, storage: &dyn MsdStorageProvider,
        id: &str, backend: &str, _mode: &str, image_id: Option<String>,
    ) -> Result<bool, String> {
        let (_guard, session) = Self::acquire_msd(registry, id).await?;

        if let Some(ref image) = image_id {
            if let Some(ref msd) = session.msd {
                msd.msd_set_image(image.clone(), 0).await;
            }
        }

        let needs_keepalive = match backend {
            "mahogany" => {
                if let Some(ref msd) = session.msd {
                    msd.msd_connect().await.map_err(|e| e.to_string())?;
                }
                true
            }
            "virtual-drive" => {
                storage.ensure_dirs(id).await.map_err(|e| e.to_string())?;
                false
            }
            _ => return Err(format!("unknown msd backend: {}", backend)),
        };

        if needs_keepalive {
            let dev = registry.get_dev(id).await.ok_or_else(|| format!("device not found: {}", id))?;
            let hold = dev.acquire("msd")?;
            registry.keepalive_guards.write().await.insert(id.to_string(), hold);
        }

        Ok(needs_keepalive)
    }

    /// Disconnect MSD for a device.
    pub async fn disconnect(
        registry: &DeviceRegistry, _storage: &dyn MsdStorageProvider,
        id: &str, backend: &str,
    ) -> Result<(), String> {
        let (_guard, session) = Self::acquire_msd(registry, id).await?;

        if backend == "mahogany" {
            if let Some(ref msd) = session.msd {
                msd.msd_disconnect().await.map_err(|e| e.to_string())?;
            }
        }

        registry.keepalive_guards.write().await.remove(id);
        Ok(())
    }

    /// Set the MSD image for a device.
    pub async fn set_image(
        registry: &DeviceRegistry, id: &str, image: Option<String>, size: u64,
    ) -> Result<(), String> {
        let (_guard, session) = Self::acquire_msd(registry, id).await?;
        let path = image.clone().unwrap_or_default();
        if let Some(ref msd) = session.msd {
            msd.msd_set_image(path, size).await;
        }
        Ok(())
    }

    /// Get the current MSD status info, merging runtime state with session state.
    pub async fn status(
        registry: &DeviceRegistry, runtime: &MsdRuntimeState,
        id: &str,
    ) -> serde_json::Value {
        let session = registry.get_session(id).await;
        match session {
            Some(s) => {
                let info = match s.msd.as_ref() {
                    Some(msd) => msd.msd_status().await,
                    None => Default::default(),
                };
                let connected = if runtime.backend == "virtual-drive" { runtime.connected } else { info.connected };
                let image = runtime.current_image.clone().unwrap_or(info.image.clone());
                serde_json::json!({
                    "available": true,
                    "online": connected,
                    "connected": connected,
                    "enabled": info.enabled,
                    "image": image,
                    "image_size": info.image_size,
                    "state": {
                        "connected": connected,
                        "mode": runtime.mode,
                        "backend": runtime.backend,
                        "current_image": runtime.current_image.as_ref()
                            .map(|id| serde_json::json!({"id": id}))
                            .unwrap_or(serde_json::Value::Null)
                    },
                    "backends": ["mahogany", "virtual-drive"]
                })
            }
            None => serde_json::json!({
                "available": true,
                "online": false,
                "enabled": false,
                "image": "",
                "image_size": 0,
                "state": {
                    "connected": false,
                    "mode": runtime.mode,
                    "backend": runtime.backend,
                    "current_image": serde_json::Value::Null
                },
                "backends": ["mahogany", "virtual-drive"]
            }),
        }
    }
}
