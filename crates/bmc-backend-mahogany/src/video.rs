use async_trait::async_trait;
use bmc_feature_api::video::VideoSource;
use tokio::sync::watch;

use crate::pool::PoolLease;

pub struct MahoganyVideoBackend {
    lease: PoolLease,
    shutdown: watch::Sender<bool>,
}

impl MahoganyVideoBackend {
    pub fn new(lease: PoolLease, shutdown: watch::Sender<bool>) -> Self {
        Self { lease, shutdown }
    }
}

impl Drop for MahoganyVideoBackend {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

#[async_trait]
impl VideoSource for MahoganyVideoBackend {
    async fn request_full_frame(&self) -> Result<bool, String> {
        Ok(self.lease.entry().request_full_frame().await)
    }

    fn trace_snapshot(&self) -> serde_json::Value {
        self.lease.entry().perf_snapshot()
    }
}
