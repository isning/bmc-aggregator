use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use bmc_feature_api::msd::{
    MsdAttachment, MsdBackend as MsdBackendTrait, MsdCapabilities, MsdMedia, MsdMediaKind,
    MsdMediaSource, MsdMediaStore, MsdMediaUpload, MsdMode, MsdSlotCapabilities,
    MsdStorageCapabilities, MsdStorageLocation, MsdStorageOperation, MsdStorageUnavailableReason,
    MsdStoredImage, MsdTargetLimit, MsdTargetManagementCapabilities, MsdTargetStore,
    MsdUnavailableStorage,
};

#[cfg(test)]
use crate::ServerImageSpace;
use crate::{
    dynamic_target_capacity, AvmpGenerationProfile, IdracLegacyMediaConfig, LegacyMediaSession,
    MediaStatus,
};

pub struct IdracLegacyMediaBackend {
    session: Arc<LegacyMediaSession>,
    targets: Arc<dyn MsdTargetStore>,
    server_images: Arc<IdracServerImageStore>,
    on_recycle: Arc<dyn Fn() + Send + Sync>,
}

struct IdracServerImageStore {
    session: Arc<LegacyMediaSession>,
}

#[async_trait]
impl MsdMediaStore for IdracServerImageStore {
    async fn list(&self) -> Result<Vec<MsdStoredImage>, String> {
        self.session.refresh_server_storage().await?;
        Ok(self
            .session
            .status()
            .await
            .server_images
            .into_iter()
            .map(|image| MsdStoredImage {
                id: image.index.to_string(),
                name: String::from_utf8_lossy(&image.name).into_owned(),
                size: None,
                deletable: image.name.starts_with(b"bmc-aggregator-"),
                digests: Vec::new(),
            })
            .collect())
    }

    async fn upload(&self, upload: MsdMediaUpload) -> Result<MsdStoredImage, String> {
        let image_type = match upload.media_type {
            Some(MsdMediaKind::Optical) => 1,
            Some(MsdMediaKind::Disk) => 2,
            _ => return Err("iDRAC ServerImage upload requires optical or disk media".to_string()),
        };
        let size = upload.size;
        let wire_name = format!("bmc-aggregator-{}", upload.name);
        let digests = upload.digests;
        let image = self
            .session
            .upload_server_image(wire_name, upload.path, upload.size, image_type)
            .await?;
        Ok(MsdStoredImage {
            id: image.index.to_string(),
            name: String::from_utf8_lossy(&image.name).into_owned(),
            size: Some(size),
            deletable: true,
            digests,
        })
    }

    async fn delete(&self, image_id: &str) -> Result<(), String> {
        let index = image_id
            .parse::<u16>()
            .map_err(|_| "invalid iDRAC ServerImage id".to_string())?;
        self.session.refresh_server_storage().await?;
        if !self
            .session
            .status()
            .await
            .server_images
            .iter()
            .any(|image| image.index == index && image.name.starts_with(b"bmc-aggregator-"))
        {
            return Err("the selected ServerImage is not managed by this aggregator".to_string());
        }
        self.session.delete_server_image(index).await
    }
}

fn capabilities_for_media_types(
    media_types: Vec<MsdMediaKind>,
    removable_target_available: bool,
    dedicated_floppy_target_available: bool,
) -> MsdCapabilities {
    let storage_locations = if media_types.is_empty() {
        Vec::new()
    } else {
        vec![MsdStorageCapabilities::aggregator(media_types.clone())]
    };
    let mut slots = Vec::new();
    if media_types.contains(&MsdMediaKind::Optical) {
        slots.push(MsdSlotCapabilities::new(
            "optical",
            vec![MsdMediaKind::Optical],
        ));
    }
    if dedicated_floppy_target_available && media_types.contains(&MsdMediaKind::Floppy) {
        slots.push(MsdSlotCapabilities::new(
            "floppy",
            vec![MsdMediaKind::Floppy],
        ));
    }
    let mut removable_media_types = Vec::new();
    if !dedicated_floppy_target_available && media_types.contains(&MsdMediaKind::Floppy) {
        removable_media_types.push(MsdMediaKind::Floppy);
    }
    if media_types.contains(&MsdMediaKind::Flash) {
        removable_media_types.push(MsdMediaKind::Flash);
    }
    if removable_target_available && !removable_media_types.is_empty() {
        slots.push(MsdSlotCapabilities::new("removable", removable_media_types));
    }
    if media_types.contains(&MsdMediaKind::Disk) {
        slots.push(MsdSlotCapabilities::new("disk", vec![MsdMediaKind::Disk]));
    }
    MsdCapabilities {
        virtual_drive: false,
        slots,
        storage_locations,
        unavailable_storage_locations: Vec::new(),
        target_management: MsdTargetManagementCapabilities::default(),
    }
}

fn should_refresh_server_storage(requested_media_type: Option<MsdMediaKind>) -> bool {
    requested_media_type.is_none()
}

fn should_recycle_after_final_unmap(profile: AvmpGenerationProfile, mappings_empty: bool) -> bool {
    profile == AvmpGenerationProfile::Idrac6Java && mappings_empty
}

fn apply_server_storage_capabilities(capabilities: &mut MsdCapabilities, status: &MediaStatus) {
    let Some(space) = status.server_image_space else {
        return;
    };
    if space.total_blocks > 0 && space.free_blocks > 0 {
        capabilities
            .storage_locations
            .push(MsdStorageCapabilities::bmc(
                vec![MsdMediaKind::Optical, MsdMediaKind::Disk],
                vec![
                    MsdStorageOperation::List,
                    MsdStorageOperation::Upload,
                    MsdStorageOperation::Delete,
                ],
            ));
    } else {
        capabilities
            .unavailable_storage_locations
            .push(MsdUnavailableStorage {
                location: MsdStorageLocation::Bmc,
                reason: MsdStorageUnavailableReason::MediaNotPresent,
                hint_key: "msd.idracVflashUnavailableHint".to_string(),
            });
    }
}

#[async_trait]
impl MsdBackendTrait for IdracLegacyMediaBackend {
    async fn capabilities(&self) -> MsdCapabilities {
        let status = self.session.status().await;
        let mut media_types: Vec<_> = status.known_media_types.iter().copied().collect();
        if status.flash_activation_available && !media_types.contains(&MsdMediaKind::Flash) {
            media_types.push(MsdMediaKind::Flash);
        }
        for mapping in status.mappings.values() {
            if !media_types.contains(&mapping.media_type) {
                media_types.push(mapping.media_type);
            }
        }
        media_types.sort();
        debug!(
            profile = ?self.session.profile(),
            drives = ?status.remote_drives,
            media_types = ?media_types,
            "iDRAC legacy virtual-media capabilities"
        );
        let mut capabilities = capabilities_for_media_types(
            media_types.clone(),
            status.removable_target_available || status.flash_activation_available,
            status.dedicated_floppy_target_available,
        );
        apply_server_storage_capabilities(&mut capabilities, &status);
        let dynamic_media_types: Vec<_> = media_types
            .iter()
            .copied()
            .filter(|media_type| {
                dynamic_target_capacity(self.session.profile(), *media_type, &status.remote_drives)
                    > 1
            })
            .collect();
        let limits: Vec<_> = dynamic_media_types
            .iter()
            .map(|media_type| MsdTargetLimit {
                media_type: *media_type,
                max_targets: u16::try_from(dynamic_target_capacity(
                    self.session.profile(),
                    *media_type,
                    &status.remote_drives,
                ))
                .unwrap_or(u16::MAX),
            })
            .collect();
        let max_targets = limits.iter().map(|limit| limit.max_targets).max();
        capabilities.target_management = MsdTargetManagementCapabilities {
            deletable: !dynamic_media_types.is_empty(),
            media_types: dynamic_media_types,
            limits,
            max_targets,
        };
        capabilities.slots.extend(self.targets.slots().await);
        capabilities
    }

    async fn status(&self) -> Result<serde_json::Value, String> {
        let status = self.session.status().await;
        Ok(serde_json::json!({
            "connected": status.connected,
            "mapped": !status.mappings.is_empty(),
            "reconnecting": status.reconnecting,
            "enabled": true,
            "mappings": status.mappings.iter().map(|(target_id, mapping)| serde_json::json!({
                "slot_id": target_id,
                "media_type": mapping.media_type,
                "image": {"size": mapping.image_size},
                "read_requests": mapping.read_requests,
                "read_blocks": mapping.read_blocks,
            })).collect::<Vec<_>>(),
            "reconnect_available": status.reconnect_available,
            "reconnect_session_response_received": status.reconnect_session_response_received,
            "reconnect_check_interval_seconds": status.reconnect_check_interval_seconds,
            "read_requests": status.read_requests,
            "read_blocks": status.read_blocks,
        }))
    }

    async fn attachment_snapshot(&self) -> Result<Option<Vec<MsdAttachment>>, String> {
        let status = self.session.status().await;
        Ok(Some(
            status
                .mappings
                .into_iter()
                .map(|(slot_id, mapping)| MsdAttachment {
                    slot_id,
                    mode: MsdMode::Image,
                    media: None,
                    media_type: Some(mapping.media_type),
                })
                .collect(),
        ))
    }

    async fn validate_media(&self, media: &MsdMedia) -> Result<(), String> {
        let MsdMediaSource::AggregatorImage { path, size, .. } = &media.source else {
            return Err("Dell legacy virtual media requires aggregator-managed media".to_string());
        };
        match media.media_type {
            Some(MsdMediaKind::Optical) => {
                if *size == 0 || *size % 2048 != 0 {
                    return Err(
                        "optical image size must be a non-zero multiple of 2048 bytes".to_string(),
                    );
                }
                Ok(())
            }
            Some(MsdMediaKind::Floppy | MsdMediaKind::Flash) => {
                bmc_backend_support::removable_image::validate_removable_image(path, *size)
                    .await
                    .map(|_| ())
            }
            Some(MsdMediaKind::Disk) | None => {
                Err("the selected iDRAC media type is not mappable".to_string())
            }
        }
    }

    async fn mount(&self, slot_id: &str, media: MsdMedia) -> Result<(), String> {
        let MsdMediaSource::AggregatorImage {
            path,
            size,
            file: _file,
        } = media.source
        else {
            return Err("Dell legacy virtual media requires aggregator-managed media".to_string());
        };
        let media_type = media
            .media_type
            .ok_or_else(|| "Dell legacy virtual media requires a media type".to_string())?;
        let capabilities = self.capabilities().await;
        if !capabilities
            .slots
            .iter()
            .any(|slot| slot.id == slot_id && slot.media_types.contains(&media_type))
        {
            return Err("the selected media type does not match the requested slot".to_string());
        }
        // refresh_capabilities() has already verified a generation-compatible
        // drive for this operation. LegacyMediaSession consumes that exact
        // snapshot when mapping, matching the official mapDrive lifecycle;
        // re-reading the asynchronously updated status here creates a race
        // with shorter unsolicited iDRAC6 VDiskInfo tables.
        self.session
            .map(slot_id.to_string(), path, size, media_type)
            .await
            .inspect_err(|error| debug!("iDRAC legacy virtual-media map failed: {error}"))
    }

    async fn unmount(&self, slot_id: &str) -> Result<(), String> {
        self.session.unmap(slot_id.to_string()).await?;
        let mappings_empty = self.session.status().await.mappings.is_empty();
        if should_recycle_after_final_unmap(self.session.profile(), mappings_empty) {
            // The tested iDRAC6 stops publishing its type-1 removable drive
            // after release until the AVMP media session is closed. Recycle
            // only after the final mapping; an independently mounted optical
            // target must keep its session alive.
            self.session.close().await;
            (self.on_recycle)();
        }
        Ok(())
    }

    async fn create_target(
        &self,
        media_type: MsdMediaKind,
        label: Option<String>,
    ) -> Result<MsdSlotCapabilities, String> {
        let capabilities = self.capabilities().await;
        if !capabilities
            .target_management
            .media_types
            .contains(&media_type)
        {
            return Err("the selected media type is not available on this iDRAC".to_string());
        }
        let limit = capabilities
            .target_management
            .limit_for(media_type)
            .unwrap_or(0) as usize;
        self.targets.create_bounded(media_type, label, limit).await
    }

    async fn delete_target(&self, target_id: &str) -> Result<(), String> {
        self.targets.delete(target_id).await
    }

    async fn refresh_capabilities(
        &self,
        requested_media_type: Option<MsdMediaKind>,
    ) -> Result<(), String> {
        self.session.refresh_drives(requested_media_type).await?;
        // A mount only needs the live remote-drive table. ServerImage/vFlash
        // inventory is an independent persistent-storage feature, and some
        // legacy firmware emits delayed inventory responses that must not be
        // placed on the latency-sensitive map path.
        if should_refresh_server_storage(requested_media_type) {
            let _ = self.session.refresh_server_storage().await;
        }
        Ok(())
    }

    fn media_store(&self, location: MsdStorageLocation) -> Option<Arc<dyn MsdMediaStore>> {
        (location == MsdStorageLocation::Bmc)
            .then(|| self.server_images.clone() as Arc<dyn MsdMediaStore>)
    }
}

impl IdracLegacyMediaBackend {
    pub async fn connect(
        config: IdracLegacyMediaConfig,
        targets: Arc<dyn MsdTargetStore>,
        on_disconnect: Arc<dyn Fn() + Send + Sync>,
        on_recycle: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Arc<Self>, String> {
        let session = LegacyMediaSession::connect(config, {
            let callback = on_disconnect;
            move || callback()
        })
        .await?;
        Ok(Arc::new(Self {
            server_images: Arc::new(IdracServerImageStore {
                session: session.clone(),
            }),
            session,
            targets,
            on_recycle,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_mount_refresh_does_not_probe_independent_server_image_storage() {
        assert!(should_refresh_server_storage(None));
        assert!(!should_refresh_server_storage(Some(MsdMediaKind::Optical)));
        assert!(!should_refresh_server_storage(Some(MsdMediaKind::Floppy)));
        assert!(!should_refresh_server_storage(Some(MsdMediaKind::Flash)));
    }

    #[test]
    fn only_idrac6_final_unmap_recycles_the_media_session() {
        use crate::AvmpGenerationProfile::{Idrac6Java, Idrac8Native};

        assert!(should_recycle_after_final_unmap(Idrac6Java, true));
        assert!(!should_recycle_after_final_unmap(Idrac6Java, false));
        assert!(!should_recycle_after_final_unmap(Idrac8Native, true));
    }

    #[test]
    fn no_mappable_drive_does_not_publish_an_implicit_storage_target() {
        let capabilities = capabilities_for_media_types(Vec::new(), false, false);
        assert!(capabilities.storage_locations.is_empty());
    }

    #[test]
    fn shared_removable_target_exposes_floppy_and_flash_as_modes_of_one_slot() {
        let capabilities = capabilities_for_media_types(
            vec![
                MsdMediaKind::Optical,
                MsdMediaKind::Floppy,
                MsdMediaKind::Flash,
            ],
            true,
            false,
        );
        assert_eq!(capabilities.slots.len(), 2);
        assert_eq!(capabilities.slots[0].id, "optical");
        assert_eq!(capabilities.slots[1].id, "removable");
        assert_eq!(
            capabilities.slots[1].media_types,
            vec![MsdMediaKind::Floppy, MsdMediaKind::Flash]
        );
    }

    #[test]
    fn dedicated_floppy_and_removable_targets_remain_independent_slots() {
        let capabilities = capabilities_for_media_types(
            vec![MsdMediaKind::Floppy, MsdMediaKind::Flash],
            true,
            true,
        );
        assert_eq!(capabilities.slots.len(), 2);
        assert_eq!(capabilities.slots[0].id, "floppy");
        assert_eq!(capabilities.slots[1].id, "removable");
        assert_eq!(capabilities.slots[1].media_types, vec![MsdMediaKind::Flash]);
    }

    #[test]
    fn zero_server_image_space_exposes_an_unavailable_bmc_location_hint() {
        let mut capabilities = capabilities_for_media_types(Vec::new(), false, false);
        let status = MediaStatus {
            server_image_space: Some(ServerImageSpace {
                store: 0x10,
                total_blocks: 0,
                free_blocks: 0,
            }),
            server_storage_probed: true,
            ..Default::default()
        };
        apply_server_storage_capabilities(&mut capabilities, &status);
        assert!(capabilities.storage_locations.is_empty());
        assert_eq!(capabilities.unavailable_storage_locations.len(), 1);
        assert_eq!(
            capabilities.unavailable_storage_locations[0].reason,
            MsdStorageUnavailableReason::MediaNotPresent
        );
    }

    #[test]
    fn writable_server_image_space_exposes_bmc_catalog_management() {
        let mut capabilities = capabilities_for_media_types(Vec::new(), false, false);
        let status = MediaStatus {
            server_image_space: Some(ServerImageSpace {
                store: 0x10,
                total_blocks: 4096,
                free_blocks: 2048,
            }),
            server_storage_probed: true,
            ..Default::default()
        };
        apply_server_storage_capabilities(&mut capabilities, &status);
        let storage = capabilities.storage(MsdStorageLocation::Bmc).unwrap();
        assert!(storage.supports(MsdStorageOperation::List));
        assert!(storage.supports(MsdStorageOperation::Upload));
        assert!(storage.supports(MsdStorageOperation::Delete));
        assert!(!storage.supports(MsdStorageOperation::Select));
    }
}
