use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SensorRecord {
    pub id: String,
    pub name: Option<String>,
    pub category: Option<String>,
    pub reading: Option<f64>,
    pub unit: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub overall_status: Option<String>,
    pub sensors: Vec<SensorRecord>,
}

#[async_trait]
pub trait HealthBackend: Send + Sync {
    async fn snapshot(&self) -> Result<HealthSnapshot, String>;
}
