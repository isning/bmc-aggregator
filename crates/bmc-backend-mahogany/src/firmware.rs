use async_trait::async_trait;
use bmc_feature_api::firmware::{FirmwareBackend, FirmwareRecord};

use crate::pool::PoolLease;

pub struct MahoganyFirmwareFeature {
    lease: PoolLease,
}

impl MahoganyFirmwareFeature {
    pub fn new(lease: PoolLease) -> Self {
        Self { lease }
    }
}

#[async_trait]
impl FirmwareBackend for MahoganyFirmwareFeature {
    async fn inventory(&self) -> Result<Vec<FirmwareRecord>, String> {
        let version = self.lease.entry().firmware_version().await.ok_or_else(|| {
            "Mahogany firmware version has not been reported by the BMC".to_string()
        })?;
        Ok(vec![FirmwareRecord {
            id: "bmc".to_string(),
            name: Some("BMC firmware".to_string()),
            version: Some(version),
            updateable: None,
            status: None,
        }])
    }
}
