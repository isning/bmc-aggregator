use std::sync::Arc;


use crate::bmc::traits::{BmcInputSession, BmcMsdSession, BmcPowerSession, BmcSession};

#[derive(Clone)]
pub struct DeviceSession {
    pub base: Option<Arc<dyn BmcSession>>,
    pub input: Option<Arc<dyn BmcInputSession>>,
    pub msd: Option<Arc<dyn BmcMsdSession>>,
    pub power: Option<Arc<dyn BmcPowerSession>>,
}

impl DeviceSession {
    pub fn video(&self) -> Option<Arc<dyn BmcSession>> {
        self.base.clone()
    }

    pub fn input(&self) -> Option<Arc<dyn BmcInputSession>> {
        self.input.clone()
    }

    pub fn msd(&self) -> Option<Arc<dyn BmcMsdSession>> {
        self.msd.clone()
    }

    pub fn power(&self) -> Option<Arc<dyn BmcPowerSession>> {
        self.power.clone()
    }
}
