use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsdStorageLocation {
    Aggregator,
    Bmc,
    Remote,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsdStorageOperation {
    Select,
    List,
    Upload,
    Delete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MsdStorageUnavailableReason {
    MediaNotPresent,
    NotInitialized,
    ProtocolUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MsdUnavailableStorage {
    pub location: MsdStorageLocation,
    pub reason: MsdStorageUnavailableReason,
    pub hint_key: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsdMediaKind {
    Optical,
    Floppy,
    Disk,
    Flash,
}

#[async_trait]
pub trait MsdTargetStore: Send + Sync {
    async fn slots(&self) -> Vec<MsdSlotCapabilities>;
    async fn create_bounded(
        &self,
        media_type: MsdMediaKind,
        label: Option<String>,
        limit: usize,
    ) -> Result<MsdSlotCapabilities, String>;
    async fn delete(&self, target_id: &str) -> Result<(), String>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MsdStorageCapabilities {
    pub location: MsdStorageLocation,
    pub operations: Vec<MsdStorageOperation>,
    pub media_types: Vec<MsdMediaKind>,
}

impl MsdStorageCapabilities {
    pub fn aggregator(media_types: Vec<MsdMediaKind>) -> Self {
        Self {
            location: MsdStorageLocation::Aggregator,
            operations: vec![
                MsdStorageOperation::Select,
                MsdStorageOperation::List,
                MsdStorageOperation::Upload,
                MsdStorageOperation::Delete,
            ],
            media_types,
        }
    }
    pub fn bmc(media_types: Vec<MsdMediaKind>, operations: Vec<MsdStorageOperation>) -> Self {
        Self {
            location: MsdStorageLocation::Bmc,
            operations,
            media_types,
        }
    }
    pub fn remote_url(media_types: Vec<MsdMediaKind>) -> Self {
        Self {
            location: MsdStorageLocation::Remote,
            operations: vec![MsdStorageOperation::Select],
            media_types,
        }
    }
    pub fn supports(&self, operation: MsdStorageOperation) -> bool {
        self.operations.contains(&operation)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "location", rename_all = "snake_case")]
pub enum MsdMediaReference {
    Aggregator { image_id: String },
    Bmc { image_id: String },
    Remote { url: String },
}
impl MsdMediaReference {
    pub const fn location(&self) -> MsdStorageLocation {
        match self {
            Self::Aggregator { .. } => MsdStorageLocation::Aggregator,
            Self::Bmc { .. } => MsdStorageLocation::Bmc,
            Self::Remote { .. } => MsdStorageLocation::Remote,
        }
    }
}

#[derive(Clone, Debug)]
pub enum MsdMediaSource {
    AggregatorImage {
        path: String,
        size: u64,
        /// Keeps the already validated local file alive while a backend
        /// duplicates `/proc/self/fd/<n>` into its own session-owned handle.
        file: Arc<std::fs::File>,
    },
    BmcImage {
        id: String,
    },
    RemoteUrl {
        url: String,
    },
}
impl MsdMediaSource {
    pub const fn location(&self) -> MsdStorageLocation {
        match self {
            Self::AggregatorImage { .. } => MsdStorageLocation::Aggregator,
            Self::BmcImage { .. } => MsdStorageLocation::Bmc,
            Self::RemoteUrl { .. } => MsdStorageLocation::Remote,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MsdMedia {
    pub source: MsdMediaSource,
    pub media_type: Option<MsdMediaKind>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MsdSlotCapabilities {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub media_types: Vec<MsdMediaKind>,
    #[serde(default)]
    pub dynamic: bool,
}
impl MsdSlotCapabilities {
    pub fn new(id: impl Into<String>, media_types: Vec<MsdMediaKind>) -> Self {
        Self {
            id: id.into(),
            label: None,
            media_types,
            dynamic: false,
        }
    }
    pub fn dynamic(id: impl Into<String>, label: Option<String>, media_type: MsdMediaKind) -> Self {
        Self {
            id: id.into(),
            label,
            media_types: vec![media_type],
            dynamic: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsdMode {
    #[default]
    None,
    Image,
    Drive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MsdAttachment {
    pub slot_id: String,
    pub mode: MsdMode,
    pub media: Option<MsdMediaReference>,
    pub media_type: Option<MsdMediaKind>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct MsdCapabilities {
    pub virtual_drive: bool,
    pub slots: Vec<MsdSlotCapabilities>,
    pub storage_locations: Vec<MsdStorageCapabilities>,
    pub unavailable_storage_locations: Vec<MsdUnavailableStorage>,
    pub target_management: MsdTargetManagementCapabilities,
}
impl MsdCapabilities {
    pub fn storage(&self, location: MsdStorageLocation) -> Option<&MsdStorageCapabilities> {
        self.storage_locations
            .iter()
            .find(|storage| storage.location == location)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MsdStatusSnapshot {
    pub capabilities: MsdCapabilities,
    pub attachments: Vec<MsdAttachment>,
    pub backend_status: serde_json::Value,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MsdOccupancyState {
    #[default]
    Idle,
    Managed,
    External,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MsdOccupancy {
    pub state: MsdOccupancyState,
    pub details_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_sessions: Option<u16>,
}
impl Default for MsdOccupancy {
    fn default() -> Self {
        Self {
            state: MsdOccupancyState::Idle,
            details_available: true,
            active_sessions: Some(0),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct MsdTargetManagementCapabilities {
    pub media_types: Vec<MsdMediaKind>,
    pub deletable: bool,
    pub limits: Vec<MsdTargetLimit>,
    pub max_targets: Option<u16>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct MsdTargetLimit {
    pub media_type: MsdMediaKind,
    pub max_targets: u16,
}
impl MsdTargetManagementCapabilities {
    pub fn limit_for(&self, media_type: MsdMediaKind) -> Option<u16> {
        self.limits
            .iter()
            .find(|limit| limit.media_type == media_type)
            .map(|limit| limit.max_targets)
            .or(self.max_targets)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MsdStoredImage {
    pub id: String,
    pub name: String,
    pub size: Option<u64>,
    pub deletable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub digests: Vec<MsdMediaDigest>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsdDigestAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MsdMediaDigest {
    pub algorithm: MsdDigestAlgorithm,
    pub value: String,
}
pub struct MsdMediaUpload {
    pub name: String,
    pub content_type: Option<String>,
    pub media_type: Option<MsdMediaKind>,
    pub path: PathBuf,
    pub size: u64,
    pub digests: Vec<MsdMediaDigest>,
}

#[async_trait]
pub trait MsdMediaStore: Send + Sync {
    async fn list(&self) -> Result<Vec<MsdStoredImage>, String>;
    async fn upload(&self, upload: MsdMediaUpload) -> Result<MsdStoredImage, String>;
    async fn delete(&self, image_id: &str) -> Result<(), String>;
}

#[async_trait]
pub trait MsdBackend: Send + Sync {
    async fn capabilities(&self) -> MsdCapabilities;
    async fn status(&self) -> Result<serde_json::Value, String>;
    async fn attachment_snapshot(&self) -> Result<Option<Vec<MsdAttachment>>, String> {
        Ok(None)
    }
    async fn validate_media(&self, _media: &MsdMedia) -> Result<(), String> {
        Ok(())
    }
    async fn mount(&self, slot_id: &str, media: MsdMedia) -> Result<(), String>;
    async fn unmount(&self, slot_id: &str) -> Result<(), String>;
    async fn create_target(
        &self,
        _media_type: MsdMediaKind,
        _label: Option<String>,
    ) -> Result<MsdSlotCapabilities, String> {
        Err("this MSD backend does not support dynamic targets".to_string())
    }
    async fn delete_target(&self, _target_id: &str) -> Result<(), String> {
        Err("this MSD backend does not support dynamic targets".to_string())
    }
    async fn refresh_capabilities(
        &self,
        _requested_media_type: Option<MsdMediaKind>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn media_store(&self, _location: MsdStorageLocation) -> Option<Arc<dyn MsdMediaStore>> {
        None
    }
}
