//! Wake-on-LAN protocol backend.

mod configuration;

pub use configuration::configuration_descriptors;

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use async_trait::async_trait;
use bmc_feature_api::power::{PowerBackend, PowerCapabilities};

#[derive(Clone, Copy, Debug)]
pub struct WolBackend {
    broadcast: Ipv4Addr,
}

impl WolBackend {
    pub fn new(broadcast: Ipv4Addr) -> Self {
        Self { broadcast }
    }

    async fn send_wake(&self, mac_address: &str) -> Result<(), String> {
        let mac = parse_mac(mac_address)?;
        let broadcast = self.broadcast;
        tokio::task::spawn_blocking(move || {
            let packet = magic_packet(mac);
            let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
                .map_err(|error| format!("bind UDP socket: {error}"))?;
            socket
                .set_broadcast(true)
                .map_err(|error| format!("enable UDP broadcast: {error}"))?;
            socket
                .set_write_timeout(Some(std::time::Duration::from_secs(2)))
                .map_err(|error| format!("set WOL send timeout: {error}"))?;
            let sent = socket
                .send_to(&packet, SocketAddrV4::new(broadcast, 9))
                .map_err(|error| format!("send WOL packet: {error}"))?;
            use bmc_backend_model::transaction::{Event, State};
            let state = State::Sending.step(if sent == packet.len() {
                Event::Sent
            } else {
                Event::Lost
            });
            if state != State::Dispatched {
                return Err("incomplete WOL datagram; device state unknown".to_string());
            }
            Ok(())
        })
        .await
        .map_err(|error| format!("WOL task failed: {error}"))?
    }
}

#[async_trait]
impl PowerBackend for WolBackend {
    async fn capabilities(&self) -> PowerCapabilities {
        PowerCapabilities::default()
    }

    async fn status(&self) -> Result<String, String> {
        Ok("unknown".to_string())
    }

    async fn control(&self, action: &str) -> Result<(), String> {
        Err(format!(
            "WOL backend does not support power action '{action}'; use wake with a MAC address"
        ))
    }

    async fn wake(&self, mac_address: &str) -> Result<(), String> {
        self.send_wake(mac_address).await
    }
}

fn parse_mac(value: &str) -> Result<[u8; 6], String> {
    let cleaned: String = value
        .chars()
        .filter(|character| *character != ':' && *character != '-' && *character != '.')
        .collect();
    if cleaned.len() != 12 || !cleaned.is_ascii() {
        return Err("invalid MAC address length".to_string());
    }

    let mut mac = [0u8; 6];
    for (index, byte) in mac.iter_mut().enumerate() {
        let start = index * 2;
        *byte = u8::from_str_radix(&cleaned[start..start + 2], 16)
            .map_err(|error| format!("invalid MAC address: {error}"))?;
    }
    Ok(mac)
}

fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0xffu8; 102];
    for index in 0..16 {
        packet[6 + index * 6..6 + (index + 1) * 6].copy_from_slice(&mac);
    }
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_mac_formats() {
        for value in ["00:11:22:aa:BB:cc", "00-11-22-aa-bb-cc", "001122aabbcc"] {
            assert_eq!(
                parse_mac(value).unwrap(),
                [0x00, 0x11, 0x22, 0xaa, 0xbb, 0xcc]
            );
        }
    }

    #[test]
    fn rejects_non_ascii_without_panicking() {
        assert!(parse_mac("aé000000000").is_err());
    }

    #[test]
    fn builds_magic_packet() {
        let packet = magic_packet([1, 2, 3, 4, 5, 6]);
        assert_eq!(&packet[0..6], &[0xff; 6]);
        assert_eq!(&packet[6..12], &[1, 2, 3, 4, 5, 6]);
        assert_eq!(&packet[96..102], &[1, 2, 3, 4, 5, 6]);
    }
}
