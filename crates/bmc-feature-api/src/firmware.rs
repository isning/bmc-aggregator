use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FirmwareRecord {
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub updateable: Option<bool>,
    pub status: Option<String>,
}

#[async_trait]
pub trait FirmwareBackend: Send + Sync {
    async fn inventory(&self) -> Result<Vec<FirmwareRecord>, String>;
}
