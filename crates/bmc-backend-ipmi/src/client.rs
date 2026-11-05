use std::net::SocketAddr;
use std::time::Duration;

use crate::IpmiConfig;

pub(crate) fn blocking_client(config: &IpmiConfig) -> Result<SessionClient, anyhow::Error> {
    let address = config
        .host
        .parse()
        .map(|host| SocketAddr::new(host, config.port))
        .map_err(|error| anyhow::anyhow!("invalid address: {error}"))?;
    let client = ipmi::BlockingClient::builder(address)
        .username(&config.username)
        .password(&config.password)
        .privilege_level(ipmi::PrivilegeLevel::Administrator)
        .timeout(Duration::from_secs(5))
        .retries(1) // One attempt: never replay a possibly applied chassis mutation.
        .build()
        .map_err(|error| anyhow::anyhow!("IPMI session failed: {error}"))?;

    let client = SessionClient(client);
    checked_raw(client.send_raw(0x06, 0x3b, &[ipmi::PrivilegeLevel::Administrator as u8])?)
        .map_err(|error| anyhow::anyhow!("set IPMI session privilege failed: {error}"))?;
    Ok(client)
}

pub(crate) fn checked_raw(response: ipmi::RawResponse) -> Result<Vec<u8>, anyhow::Error> {
    use bmc_backend_model::transaction::{Event, State};
    if State::Sending.step(Event::Completion(response.completion_code)) != State::Accepted {
        tracing::debug!(
            completion_code = response.completion_code,
            "IPMI raw command rejected"
        );
        return Err(anyhow::anyhow!(
            "IPMI completion code 0x{:02x}",
            response.completion_code
        ));
    }
    Ok(response.data)
}

/// The blocking closure owns the session even if its async caller is cancelled.
pub(crate) struct SessionClient(ipmi::BlockingClient);
impl core::ops::Deref for SessionClient {
    type Target = ipmi::BlockingClient;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for SessionClient {
    fn drop(&mut self) {
        let _ = self.0.close_session();
    }
}

pub(crate) async fn resolve_config(config: &IpmiConfig) -> Result<IpmiConfig, String> {
    let mut resolved = config.clone();
    let addresses = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((config.host.trim_matches(['[', ']']), config.port)),
    )
    .await
    .map_err(|_| "IPMI DNS resolution timed out".to_string())?
    .map_err(|_| "IPMI host could not be resolved".to_string())?;
    let address = addresses
        .into_iter()
        .next()
        .ok_or_else(|| "IPMI host has no addresses".to_string())?;
    resolved.host = address.ip().to_string();
    Ok(resolved)
}

#[cfg(test)]
mod resolution_tests {
    use super::*;
    #[tokio::test]
    async fn resolves_dns_ipv4_and_both_ipv6_spellings_without_opening_a_session() {
        for host in ["localhost", "127.0.0.1", "::1", "[::1]"] {
            let config = IpmiConfig {
                host: host.into(),
                port: 623,
                username: String::new(),
                password: String::new(),
            };
            let resolved = resolve_config(&config).await.unwrap();
            assert!(resolved
                .host
                .parse::<std::net::IpAddr>()
                .unwrap()
                .is_loopback());
            assert_eq!(resolved.port, 623);
        }
    }
}
