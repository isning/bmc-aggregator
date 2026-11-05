use async_trait::async_trait;
use bmc_feature_api::power::{PowerAction, PowerBackend, PowerCapabilities};

use crate::client::{blocking_client, checked_raw};
use crate::IpmiConfig;

pub struct IpmiPowerBackend {
    config: IpmiConfig,
}

impl IpmiPowerBackend {
    pub fn new(config: IpmiConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl PowerBackend for IpmiPowerBackend {
    async fn capabilities(&self) -> PowerCapabilities {
        PowerCapabilities {
            actions: vec![
                PowerAction::On,
                PowerAction::Off,
                PowerAction::Reset,
                PowerAction::GracefulShutdown,
            ],
        }
    }

    async fn status(&self) -> Result<String, String> {
        let config = crate::client::resolve_config(&self.config).await?;
        tokio::task::spawn_blocking(move || {
            let client = blocking_client(&config).map_err(|error| error.to_string())?;
            let data = checked_raw(
                client
                    .send_raw(0x00, 0x01, &[])
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let flags = data
                .first()
                .ok_or_else(|| "short IPMI chassis status".to_string())?;
            Ok(if flags & 1 != 0 { "on" } else { "off" }.to_string())
        })
        .await
        .map_err(|error| format!("IPMI status task failed: {error}"))?
    }

    async fn control(&self, action: &str) -> Result<(), String> {
        let action = chassis_control(action)?;
        let config = crate::client::resolve_config(&self.config).await?;
        tokio::task::spawn_blocking(move || {
            let client = blocking_client(&config).map_err(|error| error.to_string())?;
            checked_raw(
                client
                    .send_raw(0x00, 0x02, &[action])
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            Ok(())
        })
        .await
        .map_err(|error| format!("IPMI control task failed: {error}"))?
    }

    async fn wake(&self, _mac_address: &str) -> Result<(), String> {
        Err("IPMI backend does not implement Wake-on-LAN; select the wol backend".to_string())
    }
}

fn chassis_control(command: &str) -> Result<u8, String> {
    let action = PowerAction::from_command(command)
        .ok_or_else(|| format!("unknown power action: {command}"))?;
    match action {
        PowerAction::Off => Ok(0),
        PowerAction::On => Ok(1),
        PowerAction::PowerCycle => Ok(2),
        PowerAction::Reset => Ok(3),
        PowerAction::GracefulShutdown => Ok(5),
        PowerAction::GracefulRestart => {
            Err("IPMI has no standard graceful restart chassis action".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn advertised_actions_map_to_distinct_chassis_commands() {
        let backend = IpmiPowerBackend::new(IpmiConfig {
            host: "invalid host".into(),
            port: 623,
            username: String::new(),
            password: String::new(),
        });
        let actions = backend.capabilities().await.actions;
        assert_eq!(actions.len(), 4);
        for (action, expected) in actions.into_iter().zip([1, 0, 3, 5]) {
            let command = match action {
                PowerAction::On => "on",
                PowerAction::Off => "off",
                PowerAction::Reset => "reset",
                PowerAction::PowerCycle => "power_cycle",
                PowerAction::GracefulShutdown => "graceful_shutdown",
                PowerAction::GracefulRestart => "graceful_restart",
            };
            assert_eq!(chassis_control(command).unwrap(), expected);
        }
        assert_eq!(chassis_control("power_cycle").unwrap(), 2);
        assert_eq!(chassis_control("cycle").unwrap(), 2);
        assert_eq!(chassis_control("short").unwrap(), 1);
        assert_eq!(chassis_control("press").unwrap(), 1);
        assert_eq!(chassis_control("long").unwrap(), 0);
        assert_eq!(chassis_control("hold").unwrap(), 0);
        for command in ["graceful_restart", "graceful_reset", "unknown"] {
            let failure = backend.control(command).await.unwrap_err();
            assert_eq!(failure, chassis_control(command).unwrap_err());
        }
    }
}
