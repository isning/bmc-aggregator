use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentifyMode {
    Off,
    Temporary,
    Indefinite,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IdentifyCapabilities {
    pub modes: Vec<IdentifyMode>,
    pub max_temporary_seconds: u8,
    pub writable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IdentifyState {
    pub mode: IdentifyMode,
}

#[async_trait]
pub trait IdentifyBackend: Send + Sync {
    async fn capabilities(&self) -> IdentifyCapabilities;
    async fn state(&self) -> Result<IdentifyState, String>;
    async fn set(&self, mode: IdentifyMode, duration_seconds: Option<u8>) -> Result<(), String>;
    async fn set_outcome(
        &self,
        mode: IdentifyMode,
        duration_seconds: Option<u8>,
    ) -> Result<(), crate::mutation::MutationError> {
        self.set(mode, duration_seconds)
            .await
            .map_err(crate::mutation::MutationError::unknown)
    }
}
