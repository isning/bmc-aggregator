use std::sync::Arc;
use futures::stream::BoxStream;
use tokio::sync::watch;

use crate::bmc::stream::pipeline::SharedVideoPipeline;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub width: u16,
    pub height: u16,
    pub online: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum PowerAction {
    PressPower,
    PressReset,
    PressPowerLong,
}

#[derive(Debug, Clone, Default)]
pub struct MsdStatusInfo {
    pub connected: bool,
    pub enabled: bool,
    pub image: String,
    pub image_size: u64,
}

// ── Core session (always present) ──────────────────────────────────

#[async_trait::async_trait]
pub trait BmcSession: Send + Sync {
    fn subscribe_encoded_frames(&self) -> BoxStream<'static, Vec<u8>>;
    async fn info(&self) -> DeviceInfo;
    async fn close(&self) -> Result<(), crate::bmc::error::SessionError>;
    fn fps(&self) -> u32 { 0 }
    fn clients(&self) -> usize { 0 }
    fn perf_snapshot(&self) -> serde_json::Value { serde_json::json!({}) }
    fn set_pipeline(&self, _pipeline: Arc<SharedVideoPipeline>) {}
    /// Returns a receiver that signals when the underlying transport disconnects.
    /// Returns `None` if the backend doesn't support disconnect notification.
    fn on_disconnect(&self) -> Option<watch::Receiver<bool>> { None }
}

// ── Input devices (keyboard / mouse) ───────────────────────────────

#[async_trait::async_trait]
pub trait BmcInputSession: Send + Sync {
    async fn send_keyboard(&self, scancode: u16, pressed: bool) -> Result<(), crate::bmc::error::SessionError>;
    async fn send_mouse(
        &self, x: i16, y: i16, button_mask: u8, wheel_delta: i8,
    ) -> Result<(), crate::bmc::error::SessionError>;
}

// ── Power control (IPMI) ───────────────────────────────────────────

#[async_trait::async_trait]
pub trait BmcPowerSession: Send + Sync {
    async fn power_control(&self, action: PowerAction) -> Result<(), crate::bmc::error::SessionError>;
    async fn power_status(&self) -> Result<crate::bmc::backends::general::ipmi::PowerState, crate::bmc::error::SessionError>;
}

// ── Mass Storage Device (MSD) ──────────────────────────────────────

#[async_trait::async_trait]
pub trait BmcMsdSession: Send + Sync {
    async fn msd_status(&self) -> MsdStatusInfo;
    async fn msd_connect(&self) -> Result<(), crate::bmc::error::SessionError>;
    async fn msd_disconnect(&self) -> Result<(), crate::bmc::error::SessionError>;
    async fn msd_set_image(&self, path: String, size: u64);
}
