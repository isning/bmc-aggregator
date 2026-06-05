use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::stream::BoxStream;

use crate::bmc::error::SessionError;
use crate::bmc::session::types::DeviceSession;
use crate::bmc::traits::{
    BmcInputSession, BmcMsdSession, BmcPowerSession, BmcSession, DeviceInfo,
    MsdStatusInfo, PowerAction,
};

pub struct MockSession {
    pub online: AtomicBool,
    pub msd_connected: AtomicBool,
    pub msd_image: tokio::sync::Mutex<String>,
    pub msd_image_size: tokio::sync::Mutex<u64>,
}

impl MockSession {
    pub fn new() -> Self {
        Self {
            online: AtomicBool::new(true),
            msd_connected: AtomicBool::new(false),
            msd_image: tokio::sync::Mutex::new(String::new()),
            msd_image_size: tokio::sync::Mutex::new(0),
        }
    }

    pub fn new_offline() -> Self {
        Self {
            online: AtomicBool::new(false),
            msd_connected: AtomicBool::new(false),
            msd_image: tokio::sync::Mutex::new(String::new()),
            msd_image_size: tokio::sync::Mutex::new(0),
        }
    }

    pub fn to_session(self: Arc<Self>) -> DeviceSession {
        DeviceSession {
            base: Some(self.clone()),
            input: Some(self.clone()),
            power: Some(self.clone()),
            msd: Some(self.clone()),
        }
    }
}

#[async_trait::async_trait]
impl BmcSession for MockSession {
    fn subscribe_encoded_frames(&self) -> BoxStream<'static, Vec<u8>> {
        Box::pin(futures::stream::pending())
    }

    async fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: String::new(),
            name: String::new(),
            width: 1920,
            height: 1080,
            online: self.online.load(Ordering::SeqCst),
        }
    }

    async fn close(&self) -> Result<(), SessionError> {
        self.online.store(false, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait::async_trait]
impl BmcInputSession for MockSession {
    async fn send_keyboard(&self, _scancode: u16, _pressed: bool) -> Result<(), SessionError> {
        Ok(())
    }
    async fn send_mouse(
        &self, _x: i16, _y: i16, _button_mask: u8, _wheel_delta: i8,
    ) -> Result<(), SessionError> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl BmcPowerSession for MockSession {
    async fn power_control(&self, _action: PowerAction) -> Result<(), SessionError> {
        Ok(())
    }
    async fn power_status(
        &self,
    ) -> Result<crate::bmc::backends::general::ipmi::PowerState, SessionError> {
        Ok(crate::bmc::backends::general::ipmi::PowerState::On)
    }
}

#[async_trait::async_trait]
impl BmcMsdSession for MockSession {
    async fn msd_status(&self) -> MsdStatusInfo {
        let image = self.msd_image.lock().await.clone();
        let image_size = *self.msd_image_size.lock().await;
        MsdStatusInfo {
            connected: self.msd_connected.load(Ordering::SeqCst),
            enabled: !image.is_empty(),
            image,
            image_size,
        }
    }

    async fn msd_connect(&self) -> Result<(), SessionError> {
        self.msd_connected.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn msd_disconnect(&self) -> Result<(), SessionError> {
        self.msd_connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn msd_set_image(&self, path: String, size: u64) {
        *self.msd_image.lock().await = path;
        *self.msd_image_size.lock().await = size;
    }
}
