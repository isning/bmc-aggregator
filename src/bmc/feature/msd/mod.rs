pub mod config;
pub mod handler;
pub mod transfer;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;

use crate::web::state::AppState;

pub use bmc_feature_api::msd::*;

pub fn configuration_descriptor() -> bmc_feature_api::configuration::FeatureConfigDescriptor {
    use bmc_feature_api::configuration::{
        ConfigFieldGroup, ConfigFieldKind, ConfigValue, FeatureBindingKind,
    };
    crate::bmc::configuration::feature_descriptor(
        "msd",
        FeatureBindingKind::Single,
        true,
        vec![crate::bmc::configuration::configuration_field(
            "max_image_bytes",
            ConfigFieldGroup::Advanced,
            false,
            Some(ConfigValue::Integer(64 * 1024 * 1024 * 1024)),
            ConfigFieldKind::Integer {
                min: Some(1),
                max: None,
                step: Some(1),
            },
        )],
    )
}

#[derive(Clone, Debug, Default)]
pub struct MsdTargetRegistry {
    targets: Arc<tokio::sync::RwLock<BTreeMap<String, MsdSlotCapabilities>>>,
}

impl MsdTargetRegistry {
    pub async fn create(
        &self,
        media_type: MsdMediaKind,
        label: Option<String>,
    ) -> MsdSlotCapabilities {
        let prefix = match media_type {
            MsdMediaKind::Optical => "optical",
            MsdMediaKind::Floppy => "floppy",
            MsdMediaKind::Disk => "disk",
            MsdMediaKind::Flash => "flash",
        };
        let target = MsdSlotCapabilities::dynamic(
            format!("{prefix}-{}", uuid::Uuid::new_v4()),
            label.filter(|value| !value.trim().is_empty()),
            media_type,
        );
        self.targets
            .write()
            .await
            .insert(target.id.clone(), target.clone());
        target
    }

    pub async fn create_bounded(
        &self,
        media_type: MsdMediaKind,
        label: Option<String>,
        limit: usize,
    ) -> Result<MsdSlotCapabilities, String> {
        let mut targets = self.targets.write().await;
        if targets
            .values()
            .filter(|target| target.media_types.contains(&media_type))
            .count()
            >= limit
        {
            return Err("all dynamic targets for this media type are already defined".to_string());
        }
        let prefix = match media_type {
            MsdMediaKind::Optical => "optical",
            MsdMediaKind::Floppy => "floppy",
            MsdMediaKind::Disk => "disk",
            MsdMediaKind::Flash => "flash",
        };
        let target = MsdSlotCapabilities::dynamic(
            format!("{prefix}-{}", uuid::Uuid::new_v4()),
            label.filter(|value| !value.trim().is_empty()),
            media_type,
        );
        targets.insert(target.id.clone(), target.clone());
        Ok(target)
    }

    pub async fn delete(&self, target_id: &str) -> Result<(), String> {
        self.targets
            .write()
            .await
            .remove(target_id)
            .map(|_| ())
            .ok_or_else(|| "dynamic MSD target does not exist".to_string())
    }

    pub async fn slots(&self) -> Vec<MsdSlotCapabilities> {
        self.targets.read().await.values().cloned().collect()
    }

    pub async fn contains(&self, target_id: &str, media_type: MsdMediaKind) -> bool {
        self.targets
            .read()
            .await
            .get(target_id)
            .is_some_and(|target| target.media_types.contains(&media_type))
    }
}

#[async_trait::async_trait]
impl MsdTargetStore for MsdTargetRegistry {
    async fn slots(&self) -> Vec<MsdSlotCapabilities> {
        MsdTargetRegistry::slots(self).await
    }

    async fn create_bounded(
        &self,
        media_type: MsdMediaKind,
        label: Option<String>,
        limit: usize,
    ) -> Result<MsdSlotCapabilities, String> {
        MsdTargetRegistry::create_bounded(self, media_type, label, limit).await
    }

    async fn delete(&self, target_id: &str) -> Result<(), String> {
        MsdTargetRegistry::delete(self, target_id).await
    }
}

pub struct MsdFeatureRuntime {
    /// Keep mount ownership and its public attachment record in one runtime.
    /// A configuration reload replaces the FeatureConnection and therefore
    /// cannot leave an attachment behind without the guard that owns it.
    pub owned_mounts: tokio::sync::Mutex<BTreeMap<String, MsdOwnedMount>>,
    pub targets: MsdTargetRegistry,
    pub target_lifecycle: tokio::sync::Mutex<()>,
    pub status: super::data::FeatureResource<MsdStatusSnapshot>,
    catalogs: tokio::sync::Mutex<
        BTreeMap<MsdStorageLocation, Arc<super::data::FeatureResource<Vec<MsdStoredImage>>>>,
    >,
}

pub struct MsdOwnedMount {
    pub guard: super::connection::FeatureGuard,
    pub attachment: MsdAttachment,
}

impl Default for MsdFeatureRuntime {
    fn default() -> Self {
        Self {
            owned_mounts: tokio::sync::Mutex::new(BTreeMap::new()),
            targets: MsdTargetRegistry::default(),
            target_lifecycle: tokio::sync::Mutex::new(()),
            status: super::data::FeatureResource::new("status"),
            catalogs: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }
}

impl MsdFeatureRuntime {
    pub async fn catalog(
        &self,
        location: MsdStorageLocation,
    ) -> Arc<super::data::FeatureResource<Vec<MsdStoredImage>>> {
        let mut catalogs = self.catalogs.lock().await;
        catalogs
            .entry(location)
            .or_insert_with(|| {
                Arc::new(super::data::FeatureResource::new(format!(
                    "images.{}",
                    match location {
                        MsdStorageLocation::Aggregator => "aggregator",
                        MsdStorageLocation::Bmc => "bmc",
                        MsdStorageLocation::Remote => "remote",
                    }
                )))
            })
            .clone()
    }
}

pub async fn feature_runtime(
    fc: &Arc<super::connection::FeatureConnection>,
) -> Arc<MsdFeatureRuntime> {
    fc.get_or_init_feature_runtime(|| Arc::new(MsdFeatureRuntime::default()))
        .await
}

pub fn mount(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handler::msd_status))
        .route(&format!("{base}/connect"), post(handler::msd_connect))
        .route(&format!("{base}/disconnect"), post(handler::msd_disconnect))
        .route(
            &format!("{base}/targets/create"),
            post(handler::msd_target_create),
        )
        .route(
            &format!("{base}/targets/delete"),
            post(handler::msd_target_delete),
        )
        .route(&format!("{base}/retry"), post(handler::msd_retry))
        .route(&format!("{base}/set_image"), post(handler::msd_set_image))
        .route(&format!("{base}/images"), get(handler::msd_images_list))
        .route(
            &format!("{base}/images/upload"),
            post(handler::msd_images_upload).layer(DefaultBodyLimit::disable()),
        )
        .route(
            &format!("{base}/images/uploads"),
            post(transfer::create_upload),
        )
        .route(
            &format!("{base}/images/uploads/:upload_id"),
            get(transfer::upload_status)
                .post(transfer::complete_upload)
                .patch(transfer::append_upload)
                .delete(transfer::cancel_upload)
                .layer(DefaultBodyLimit::disable()),
        )
        .route(
            &format!("{base}/images/delete"),
            post(handler::msd_images_delete),
        )
        .route(
            &format!("{base}/images/download"),
            get(handler::msd_images_download),
        )
        .route(
            &format!("{base}/images/download/status"),
            get(handler::msd_images_download_status),
        )
        .route(
            &format!("{base}/images/download/cancel"),
            post(handler::msd_images_download_cancel),
        )
        .route(&format!("{base}/drive"), get(handler::msd_drive_info))
        .route(&format!("{base}/drive/init"), post(handler::msd_drive_init))
        .route(
            &format!("{base}/drive/delete"),
            post(handler::msd_drive_delete),
        )
        .route(
            &format!("{base}/drive/files"),
            get(handler::msd_drive_files_list),
        )
        .route(
            &format!("{base}/drive/files/upload"),
            post(handler::msd_drive_files_upload),
        )
        .route(
            &format!("{base}/drive/files/download"),
            get(handler::msd_drive_files_download),
        )
        .route(
            &format!("{base}/drive/files/delete"),
            post(handler::msd_drive_files_delete),
        )
        .route(
            &format!("{base}/drive/mkdir"),
            post(handler::msd_drive_mkdir),
        )
        .route(
            &format!("{base}/config"),
            get(config::config_msd).patch(config::config_msd_update),
        )
}

#[cfg(test)]
mod tests {
    use super::{
        MsdCapabilities, MsdMediaKind, MsdMediaReference, MsdSlotCapabilities,
        MsdStorageCapabilities, MsdStorageOperation, MsdTargetLimit,
        MsdTargetManagementCapabilities, MsdTargetRegistry,
    };

    #[test]
    fn capabilities_are_location_scoped_and_contain_no_oem_drive_type() {
        let capabilities = MsdCapabilities {
            virtual_drive: false,
            slots: vec![
                MsdSlotCapabilities::new("optical", vec![MsdMediaKind::Optical]),
                MsdSlotCapabilities::new("floppy", vec![MsdMediaKind::Floppy]),
            ],
            storage_locations: vec![
                MsdStorageCapabilities::aggregator(vec![
                    MsdMediaKind::Optical,
                    MsdMediaKind::Floppy,
                ]),
                MsdStorageCapabilities::bmc(
                    vec![MsdMediaKind::Disk, MsdMediaKind::Flash],
                    vec![
                        MsdStorageOperation::Select,
                        MsdStorageOperation::List,
                        MsdStorageOperation::Upload,
                    ],
                ),
                MsdStorageCapabilities::remote_url(vec![MsdMediaKind::Optical]),
            ],
            unavailable_storage_locations: Vec::new(),
            target_management: Default::default(),
        };
        let json = serde_json::to_value(capabilities).unwrap();

        assert_eq!(
            json["storage_locations"][0]["media_types"],
            serde_json::json!(["optical", "floppy"])
        );
        assert_eq!(
            json["storage_locations"][1]["operations"],
            serde_json::json!(["select", "list", "upload"])
        );
        assert_eq!(
            json["storage_locations"][2],
            serde_json::json!({
                "location": "remote",
                "operations": ["select"],
                "media_types": ["optical"],
            })
        );
        assert!(!json.to_string().contains("drive_type"));
    }

    #[test]
    fn media_references_preserve_their_storage_domain() {
        assert_eq!(
            serde_json::to_value(MsdMediaReference::Bmc {
                image_id: "asset-42".to_string(),
            })
            .unwrap(),
            serde_json::json!({"location": "bmc", "image_id": "asset-42"})
        );
    }

    #[tokio::test]
    async fn logical_target_registry_keeps_types_labels_and_independent_lifetimes() {
        let registry = MsdTargetRegistry::default();
        let optical = registry
            .create(MsdMediaKind::Optical, Some("Installer A".to_string()))
            .await;
        let second = registry.create(MsdMediaKind::Optical, None).await;
        let floppy = registry.create(MsdMediaKind::Floppy, None).await;

        assert_ne!(optical.id, second.id);
        assert_eq!(optical.label.as_deref(), Some("Installer A"));
        assert!(optical.dynamic);
        assert!(registry.contains(&floppy.id, MsdMediaKind::Floppy).await);
        assert!(!registry.contains(&floppy.id, MsdMediaKind::Flash).await);

        registry.delete(&second.id).await.unwrap();
        let slots = registry.slots().await;
        assert_eq!(slots.len(), 2);
        assert!(slots.iter().any(|slot| slot.id == optical.id));
        assert!(slots.iter().any(|slot| slot.id == floppy.id));
    }

    #[tokio::test]
    async fn bounded_target_creation_is_atomic() {
        let registry = MsdTargetRegistry::default();
        let (first, second) = tokio::join!(
            registry.create_bounded(MsdMediaKind::Optical, None, 1),
            registry.create_bounded(MsdMediaKind::Optical, None, 1),
        );
        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(registry.slots().await.len(), 1);
    }

    #[test]
    fn per_media_target_limits_override_the_legacy_summary() {
        let management = MsdTargetManagementCapabilities {
            media_types: vec![MsdMediaKind::Optical, MsdMediaKind::Floppy],
            deletable: true,
            limits: vec![
                MsdTargetLimit {
                    media_type: MsdMediaKind::Optical,
                    max_targets: 2,
                },
                MsdTargetLimit {
                    media_type: MsdMediaKind::Floppy,
                    max_targets: 1,
                },
            ],
            max_targets: Some(2),
        };
        assert_eq!(management.limit_for(MsdMediaKind::Optical), Some(2));
        assert_eq!(management.limit_for(MsdMediaKind::Floppy), Some(1));
        assert_eq!(management.limit_for(MsdMediaKind::Flash), Some(2));
    }
}
