use async_trait::async_trait;
use bmc_feature_api::video::{VideoColorDepthProfile, VideoSource};
use tokio::sync::watch;

use crate::pool::AvctLease;

pub struct IdracAvctVideoBackend {
    lease: AvctLease,
    frame_shutdown_tx: watch::Sender<bool>,
}

impl IdracAvctVideoBackend {
    pub fn new(lease: AvctLease, frame_shutdown_tx: watch::Sender<bool>) -> Self {
        Self {
            lease,
            frame_shutdown_tx,
        }
    }
}

impl Drop for IdracAvctVideoBackend {
    fn drop(&mut self) {
        let _ = self.frame_shutdown_tx.send(true);
    }
}

#[async_trait]
impl VideoSource for IdracAvctVideoBackend {
    async fn request_full_frame(&self) -> Result<bool, String> {
        self.lease
            .session()
            .request_full_frame()
            .await
            .map(|_| true)
    }

    fn color_depth_profiles(&self) -> Vec<VideoColorDepthProfile> {
        self.lease
            .session()
            .color_depth_profiles()
            .iter()
            .map(|profile| VideoColorDepthProfile {
                id: profile.id.to_string(),
                depth_bpp: profile.depth_bpp,
                variant: profile.variant,
            })
            .collect()
    }

    fn active_color_depth_profile_id(&self) -> Option<String> {
        self.lease
            .session()
            .active_color_depth_profile_id()
            .map(str::to_string)
    }

    async fn set_color_depth(&self, profile_id: &str) -> Result<(), String> {
        self.lease.session().set_color_depth(profile_id).await
    }

    fn trace_snapshot(&self) -> serde_json::Value {
        self.lease.session().trace_snapshot()
    }
}
