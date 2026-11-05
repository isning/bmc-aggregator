use async_trait::async_trait;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    On,
    Off,
    Reset,
    PowerCycle,
    GracefulShutdown,
    GracefulRestart,
}

impl PowerAction {
    /// Resolve public action names and retained compatibility aliases once.
    pub fn from_command(command: &str) -> Option<Self> {
        match command {
            "on" | "short" | "press" => Some(Self::On),
            "off" | "long" | "hold" => Some(Self::Off),
            "reset" => Some(Self::Reset),
            "power_cycle" | "cycle" => Some(Self::PowerCycle),
            "graceful_shutdown" => Some(Self::GracefulShutdown),
            "graceful_restart" | "graceful_reset" => Some(Self::GracefulRestart),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct PowerCapabilities {
    pub actions: Vec<PowerAction>,
}

#[async_trait]
pub trait PowerBackend: Send + Sync {
    async fn capabilities(&self) -> PowerCapabilities;
    async fn status(&self) -> Result<String, String>;
    async fn control(&self, action: &str) -> Result<(), String>;
    async fn wake(&self, mac_address: &str) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_serialized_action_is_an_accepted_command() {
        for action in [
            PowerAction::On,
            PowerAction::Off,
            PowerAction::Reset,
            PowerAction::PowerCycle,
            PowerAction::GracefulShutdown,
            PowerAction::GracefulRestart,
        ] {
            let name = serde_json::to_value(action).unwrap();
            assert_eq!(
                PowerAction::from_command(name.as_str().unwrap()),
                Some(action)
            );
        }
        for (alias, action) in [
            ("short", PowerAction::On),
            ("press", PowerAction::On),
            ("long", PowerAction::Off),
            ("hold", PowerAction::Off),
            ("cycle", PowerAction::PowerCycle),
            ("graceful_reset", PowerAction::GracefulRestart),
        ] {
            assert_eq!(PowerAction::from_command(alias), Some(action));
        }
        assert_eq!(PowerAction::from_command("unknown"), None);
    }
}
