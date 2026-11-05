use std::time::Duration;

use bmc_backend_support::http_digest::{build_digest_auth, find_digest_challenge};

const MAX_KVM_PAGE_BYTES: usize = 2 * 1024 * 1024;

/// Fetch the KVM HTML page from the BMC, performing HTTP Digest auth.
pub async fn fetch_bmc_page(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
) -> Result<String, anyhow::Error> {
    let authority =
        bmc_backend_support::endpoint::authority(host, port).map_err(anyhow::Error::msg)?;
    let url = format!("http://{authority}/kvms.html");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()?;

    let resp = client.get(&url).send().await?;

    let params = resp
        .headers()
        .get_all("WWW-Authenticate")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(find_digest_challenge)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "BMC supplied no supported Digest challenge (status {})",
                resp.status()
            )
        })?;

    let auth_header = build_digest_auth(username, password, &params, "/kvms.html", "GET")
        .map_err(anyhow::Error::msg)?;

    let mut resp = client
        .get(&url)
        .header("Authorization", &auth_header)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        // The BMC page may echo session material or other diagnostic fields;
        // never include its response body in an error that can reach API logs.
        anyhow::bail!("BMC auth failed with status {}", status);
    }

    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len().saturating_add(chunk.len()) > MAX_KVM_PAGE_BYTES {
            anyhow::bail!("BMC KVM page exceeds the safe response limit");
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| anyhow::anyhow!("BMC KVM page is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn authentication_does_not_follow_remote_redirects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/other\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ).as_bytes()).await.unwrap();
            drop(socket);
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_ok()
        });
        assert!(fetch_bmc_page("127.0.0.1", port, "fixture", "fixture")
            .await
            .is_err());
        assert!(
            !server.await.unwrap(),
            "an authentication redirect must not be followed"
        );
    }
}
