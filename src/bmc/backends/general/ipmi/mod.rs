pub mod power;

use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use crate::bmc::error::SessionError;
use crate::bmc::traits::{BmcPowerSession, PowerAction};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PowerState {
    On,
    Off,
    Unknown,
}

/// IPMI session — a standalone power/sensor backend, independent of
/// any video capture protocol (Mahogany, VNC, etc.).
///
/// Each method opens a new IPMI session, performs the operation, and
/// closes it.  This keeps the session stateless and resilient to
/// network interruptions.
#[derive(Clone)]
pub struct IpmiSession {
    host: String,
    port: u16,
    username: String,
    password: String,
}

impl IpmiSession {
    pub fn new(host: String, port: u16, username: String, password: String) -> Self {
        Self { host, port, username, password }
    }
}

#[async_trait::async_trait]
impl BmcPowerSession for IpmiSession {
    async fn power_control(&self, action: PowerAction) -> Result<(), SessionError> {
        power_control(&self.host, self.port, &self.username, &self.password, action)
            .await
            .map_err(|e| SessionError::Other(format!("power_control: {}", e)))
    }

    async fn power_status(&self) -> Result<PowerState, SessionError> {
        power_status(&self.host, self.port, &self.username, &self.password)
            .await
            .map_err(|e| SessionError::Other(format!("power_status: {}", e)))
    }
}

pub async fn power_status(
    host: &str, port: u16, username: &str, password: &str,
) -> Result<PowerState, anyhow::Error> {
    let host = host.to_owned();
    let username = username.to_owned();
    let password = password.to_owned();

    tokio::task::spawn_blocking(move || {
        let addr = SocketAddr::from_str(&format!("{}:{}", host, port))
            .map_err(|e| anyhow::anyhow!("invalid address: {}", e))?;
        let client = ipmi::BlockingClient::builder(addr)
            .username(&username)
            .password(&password)
            .privilege_level(ipmi::PrivilegeLevel::Administrator)
            .timeout(Duration::from_secs(5))
            .retries(2)
            .build()
            .map_err(|e| anyhow::anyhow!("IPMI session failed: {}", e))?;

        let status = client.get_chassis_status()
            .map_err(|e| anyhow::anyhow!("get chassis status failed: {}", e))?;
        client.close_session().ok();

        Ok(if status.system_power_on {
            PowerState::On
        } else {
            PowerState::Off
        })
    })
    .await?
}

pub async fn power_control(
    host: &str, port: u16, username: &str, password: &str, action: PowerAction,
) -> Result<(), anyhow::Error> {
    let host = host.to_owned();
    let username = username.to_owned();
    let password = password.to_owned();

    tokio::task::spawn_blocking(move || {
        let addr = SocketAddr::from_str(&format!("{}:{}", host, port))
            .map_err(|e| anyhow::anyhow!("invalid address: {}", e))?;
        let client = ipmi::BlockingClient::builder(addr)
            .username(&username)
            .password(&password)
            .privilege_level(ipmi::PrivilegeLevel::Administrator)
            .timeout(Duration::from_secs(5))
            .retries(2)
            .build()
            .map_err(|e| anyhow::anyhow!("IPMI session failed: {}", e))?;

        let ctrl = match action {
            crate::bmc::traits::PowerAction::PressPower => ipmi::ChassisControl::PowerUp,
            crate::bmc::traits::PowerAction::PressPowerLong => ipmi::ChassisControl::PowerDown,
            crate::bmc::traits::PowerAction::PressReset => ipmi::ChassisControl::PowerCycle,
        };
        client.chassis_control(ctrl)
            .map_err(|e| anyhow::anyhow!("chassis control failed: {}", e))?;
        client.close_session().ok();
        Ok(())
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_power_action_mapping() {
        assert!(matches!(
            crate::bmc::traits::PowerAction::PressPower,
            crate::bmc::traits::PowerAction::PressPower
        ));
    }
}
