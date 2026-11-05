use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::requirements::{
    BackendRequirement, RemediationAction, RemediationResult, RequirementId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootMode {
    Once,
    Persistent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BootEntry {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<String>,
    pub enabled: bool,
    pub selectable: bool,
    #[serde(default)]
    pub supported_modes: Vec<BootMode>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootEntryDiscoveryState {
    Discovering,
    Available,
    #[default]
    Unsupported,
    InventoryEmpty,
    InventoryUnavailable,
    TransportUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootCapabilities {
    #[serde(default)]
    pub entries: Vec<BootEntry>,
    #[serde(default)]
    pub entry_discovery: BootEntryDiscoveryState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootSelection {
    pub mode: BootMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootStatus {
    pub capabilities: BootCapabilities,
    pub selection: BootSelection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootProviderPhase {
    Loading,
    Ready,
    Faulted,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BootProviderFault {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootProviderSnapshot<T> {
    pub provider: String,
    pub phase: BootProviderPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault: Option<BootProviderFault>,
}

impl<T> BootProviderSnapshot<T> {
    pub fn loading(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            phase: BootProviderPhase::Loading,
            data: None,
            fault: None,
        }
    }

    pub fn ready(provider: impl Into<String>, data: T) -> Self {
        Self {
            provider: provider.into(),
            phase: BootProviderPhase::Ready,
            data: Some(data),
            fault: None,
        }
    }

    pub fn faulted(
        provider: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            phase: BootProviderPhase::Faulted,
            data: None,
            fault: Some(BootProviderFault {
                code: code.into(),
                message: message.into(),
                retryable: true,
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootEntryCatalog {
    pub entries: Vec<BootEntry>,
    pub discovery: BootEntryDiscoveryState,
    pub complete: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootEntrySelection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<BootMode>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootOptionEntry {
    pub id: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<String>,
    pub supported_modes: Vec<BootMode>,
    pub enabled: bool,
    pub selectable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootOptionSelection {
    pub entry_id: String,
    pub mode: BootMode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootOptionProviderStatus {
    pub provider: String,
    pub phase: BootProviderPhase,
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault: Option<BootProviderFault>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootOptionsSnapshot {
    pub entries: Vec<BootOptionEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<BootOptionSelection>,
    pub providers: Vec<BootOptionProviderStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootDiscoverySnapshot {
    pub entries: Vec<BootEntry>,
    pub state: BootEntryDiscoveryState,
    pub selected_entry_id: Option<String>,
}

#[async_trait]
pub trait BootDiscoveryStore: Send + Sync {
    async fn discovery(&self) -> BootDiscoverySnapshot;
    async fn set_discovery(&self, discovery: BootDiscoverySnapshot);
}

/// Feature-owned state required by boot backends that also expose firmware
/// prerequisites. Keeping this port here prevents protocol crates from
/// depending on the application's concrete runtime.
#[async_trait]
pub trait BootStateStore: BootDiscoveryStore {
    async fn requirements(&self) -> Vec<BackendRequirement>;
    async fn set_requirements(&self, requirements: Vec<BackendRequirement>);
    async fn set_selected_entry_id(&self, selected_entry_id: Option<String>);
    async fn clear_selected_entry_id_if(&self, expected_entry_id: &str);
}

#[async_trait]
pub trait BootBackend: Send + Sync {
    async fn capabilities(&self) -> BootCapabilities;
    async fn selection(&self) -> Result<BootSelection, String>;
    async fn status(&self) -> Result<BootStatus, String> {
        let selection = self.selection().await?;
        let capabilities = self.capabilities().await;
        Ok(BootStatus {
            capabilities,
            selection,
        })
    }
    async fn set_selection(&self, selection: BootSelection) -> Result<(), String>;
    /// Legacy transports cannot distinguish lost acknowledgements from rejection.
    async fn set_selection_outcome(
        &self,
        selection: BootSelection,
    ) -> Result<(), crate::mutation::MutationError> {
        self.set_selection(selection)
            .await
            .map_err(crate::mutation::MutationError::unknown)
    }
    async fn requirements(&self) -> Result<Vec<BackendRequirement>, String> {
        Ok(Vec::new())
    }
    async fn remediate_requirement(
        &self,
        _requirement_id: RequirementId,
        _action: RemediationAction,
    ) -> Result<RemediationResult, String> {
        Err("boot backend exposes no requirement remediation".to_string())
    }
}
