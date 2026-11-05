pub mod handlers;
pub mod routes;
pub mod state;

use axum::http::{header, HeaderMap, StatusCode};

/// Reject cross-site WebSocket handshakes while allowing non-browser clients
/// that do not send Origin. HTTP APIs are same-origin because the router does
/// not install CORS.
pub(crate) fn validate_websocket_origin(headers: &HeaderMap) -> Result<(), StatusCode> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let origin = origin.to_str().map_err(|_| StatusCode::FORBIDDEN)?;
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    let url = reqwest::Url::parse(origin).map_err(|_| StatusCode::FORBIDDEN)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || host.contains(['/', '?', '#', '@'])
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let expected = reqwest::Url::parse(&format!("{}://{host}", url.scheme()))
        .map_err(|_| StatusCode::FORBIDDEN)?;
    (url.origin() == expected.origin())
        .then_some(())
        .ok_or(StatusCode::FORBIDDEN)
}

/// A stalled peer must not retain event subscriptions or feature leases forever.
pub(crate) async fn send_websocket(
    socket: &mut axum::extract::ws::WebSocket,
    message: axum::extract::ws::Message,
) -> Result<(), String> {
    tokio::time::timeout(std::time::Duration::from_secs(10), socket.send(message))
        .await
        .map_err(|_| "WebSocket write timed out".to_string())?
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_origin_must_match_host() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:8400".parse().unwrap());
        headers.insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        assert_eq!(
            validate_websocket_origin(&headers),
            Err(StatusCode::FORBIDDEN)
        );
        headers.insert(header::ORIGIN, "http://127.0.0.1:8400".parse().unwrap());
        assert_eq!(validate_websocket_origin(&headers), Ok(()));
    }

    #[test]
    fn websocket_origin_normalizes_default_ports_and_ipv6() {
        for (host, origin) in [
            ("example.test:80", "http://example.test"),
            ("EXAMPLE.test", "http://example.test:80"),
            ("example.test:443", "https://example.test"),
            ("[::1]:80", "http://[::1]"),
            ("[::1]:8400", "http://[::1]:8400"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse().unwrap());
            headers.insert(header::ORIGIN, origin.parse().unwrap());
            assert_eq!(validate_websocket_origin(&headers), Ok(()), "{origin}");
        }
    }

    #[test]
    fn websocket_origin_rejects_non_origins_and_different_ports() {
        for origin in [
            "null",
            "ftp://example.test",
            "http://user@example.test",
            "http://example.test/path",
            "http://example.test?query",
            "http://example.test#fragment",
            "http://example.test:8400",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, "example.test:80".parse().unwrap());
            headers.insert(header::ORIGIN, origin.parse().unwrap());
            assert_eq!(
                validate_websocket_origin(&headers),
                Err(StatusCode::FORBIDDEN)
            );
        }
    }
}
