use bmc_backend_model::transaction::{Event, State, WritePermission};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bmc_feature_api::identify::{
    IdentifyBackend, IdentifyCapabilities, IdentifyMode, IdentifyState,
};

use crate::client::{blocking_client, checked_raw};
use crate::IpmiConfig;

pub struct IpmiIdentifyBackend {
    config: IpmiConfig,
    writable: Arc<Mutex<WritePermission>>,
}

impl IpmiIdentifyBackend {
    pub fn new(config: IpmiConfig) -> Self {
        Self {
            config,
            writable: Arc::new(Mutex::new(WritePermission::Allowed)),
        }
    }
}

#[async_trait]
impl IdentifyBackend for IpmiIdentifyBackend {
    async fn capabilities(&self) -> IdentifyCapabilities {
        IdentifyCapabilities {
            modes: vec![
                IdentifyMode::Off,
                IdentifyMode::Temporary,
                IdentifyMode::Indefinite,
            ],
            max_temporary_seconds: u8::MAX,
            writable: self
                .writable
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .writable(),
        }
    }

    async fn state(&self) -> Result<IdentifyState, String> {
        let config = crate::client::resolve_config(&self.config).await?;
        tokio::task::spawn_blocking(move || {
            let client = blocking_client(&config).map_err(|error| error.to_string())?;
            let data = checked_raw(
                client
                    .send_raw(0x00, 0x01, &[])
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            parse_identify_state(&data)
        })
        .await
        .map_err(|error| format!("IPMI identify task failed: {error}"))?
    }

    async fn set(&self, mode: IdentifyMode, duration_seconds: Option<u8>) -> Result<(), String> {
        if mode == IdentifyMode::Temporary && duration_seconds == Some(0) {
            return Err(
                "IPMI temporary identify duration must be between 1 and 255 seconds".into(),
            );
        }
        let config = crate::client::resolve_config(&self.config).await?;
        let data = identify_payload(mode, duration_seconds);
        let writable = self.writable.clone();
        tokio::task::spawn_blocking(move || {
            let client = blocking_client(&config).map_err(|error| error.to_string())?;
            let response = client
                .send_raw(0x00, 0x04, &data)
                .map_err(|error| error.to_string())?;
            {
                let mut permission = writable.lock().unwrap_or_else(|e| e.into_inner());
                *permission = permission.step(response.completion_code);
            }
            checked_raw(response).map_err(|error| error.to_string())?;
            let readback = checked_raw(
                client
                    .send_raw(0x00, 0x01, &[])
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let observed = parse_identify_state(&readback)?;
            if State::Sending
                .step(Event::Completion(0))
                .step(Event::Observed(observed.mode == mode))
                != State::Confirmed
            {
                return Err("IPMI identify readback mismatch; remote result unknown".to_string());
            }
            Ok(())
        })
        .await
        .map_err(|error| format!("IPMI identify task failed: {error}"))?
    }
}

/// Encodes the one-byte IPMI Chassis Identify interval.
///
/// A zero interval means off. Indefinite identification requires the optional
/// Force Identify On byte; a one-byte 0xff means a 255-second interval.  Keeping this mapping in one function
/// makes the wire contract testable without touching a BMC.
fn identify_payload(mode: IdentifyMode, duration_seconds: Option<u8>) -> Vec<u8> {
    match mode {
        IdentifyMode::Off => vec![0],
        IdentifyMode::Temporary => vec![duration_seconds.unwrap_or(15)],
        IdentifyMode::Indefinite => vec![0, 1],
    }
}

fn parse_identify_state(data: &[u8]) -> Result<IdentifyState, String> {
    let bits = data
        .get(2)
        .copied()
        .ok_or_else(|| "chassis status response too short".to_string())?;
    let mode = match (bits >> 4) & 0x03 {
        0 => IdentifyMode::Off,
        1 => IdentifyMode::Temporary,
        2 => IdentifyMode::Indefinite,
        _ => return Err("reserved chassis identify state".to_string()),
    };
    Ok(IdentifyState { mode })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chassis_status_identify_bits_are_typed() {
        assert_eq!(
            parse_identify_state(&[0, 0, 0x00]).unwrap().mode,
            IdentifyMode::Off
        );
        assert_eq!(
            parse_identify_state(&[0, 0, 0x10]).unwrap().mode,
            IdentifyMode::Temporary
        );
        assert_eq!(
            parse_identify_state(&[0, 0, 0x20]).unwrap().mode,
            IdentifyMode::Indefinite
        );
        assert!(parse_identify_state(&[0, 0, 0x30]).is_err());
    }

    #[test]
    fn chassis_identify_payload_uses_standard_interval_encoding() {
        assert_eq!(identify_payload(IdentifyMode::Off, None), vec![0]);
        assert_eq!(
            identify_payload(IdentifyMode::Temporary, Some(15)),
            vec![15]
        );
        assert_eq!(identify_payload(IdentifyMode::Temporary, None), vec![15]);
        assert_eq!(identify_payload(IdentifyMode::Indefinite, None), vec![0, 1]);
    }
}
