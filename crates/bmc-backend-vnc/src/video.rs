use async_trait::async_trait;
use bmc_feature_api::video::VideoSource;
use tokio::sync::watch;

use crate::pool::VncLease;

pub struct VncVideoBackend {
    lease: VncLease,
    shutdown: watch::Sender<bool>,
}

impl VncVideoBackend {
    pub fn new(lease: VncLease, shutdown: watch::Sender<bool>) -> Self {
        Self { lease, shutdown }
    }
}

impl Drop for VncVideoBackend {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

#[async_trait]
impl VideoSource for VncVideoBackend {
    async fn request_full_frame(&self) -> Result<bool, String> {
        self.lease
            .session()
            .request_full_frame()
            .await
            .map(|_| true)
    }

    fn trace_snapshot(&self) -> serde_json::Value {
        self.lease.session().trace_snapshot()
    }
}
