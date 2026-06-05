use std::time::Duration;

use crate::bmc::backends::general::mahogany::protocol::{build_digest_auth, parse_digest_header};
use crate::config::DeviceConfig;

/// Fetch the KVM HTML page from the BMC, performing HTTP Digest auth.
pub async fn fetch_bmc_page(cfg: &DeviceConfig) -> Result<String, anyhow::Error> {
    let url = format!("http://{}:{}/kvms.html", cfg.host, cfg.port);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let resp = client.get(&url).send().await?;

    let www_auth = resp
        .headers()
        .get("WWW-Authenticate")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    if www_auth.is_empty() {
        anyhow::bail!("no WWW-Authenticate header from BMC (got status {})", resp.status());
    }
    let params = parse_digest_header(&www_auth)
        .ok_or_else(|| anyhow::anyhow!("failed to parse Digest auth header"))?;

    let auth_header = build_digest_auth(&cfg.auth.username, &cfg.auth.password, &params, "/kvms.html", "GET");

    let resp = client.get(&url).header("Authorization", &auth_header).send().await?;
    let status = resp.status();
    let body = resp.text().await?;

    if !status.is_success() {
        anyhow::bail!("BMC auth failed with status {}: {}", status, body);
    }

    Ok(body)
}
