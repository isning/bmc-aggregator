use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementId {
    FirmwareInventoryCollection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementCategory {
    Permission,
    Configuration,
    License,
    Service,
    Inventory,
    Hardware,
    Session,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementState {
    Satisfied,
    ActionRequired,
    Pending,
    Unavailable,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementImpact {
    Blocking,
    Degraded,
    Informational,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationAction {
    EnableInventoryCollection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationLevel {
    None,
    Required,
    Destructive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartRequirement {
    None,
    NextHostPost,
    HostRestart,
    BmcRestart,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Remediation {
    pub action: RemediationAction,
    pub confirmation: ConfirmationLevel,
    pub restart_requirement: RestartRequirement,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackendRequirement {
    pub id: RequirementId,
    pub category: RequirementCategory,
    pub state: RequirementState,
    pub impact: RequirementImpact,
    pub title_key: String,
    pub message_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<Remediation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RemediationResult {
    pub changed: bool,
    pub state: RequirementState,
    pub restart_requirement: RestartRequirement,
}
