//! Validate a configured host before embedding it in a transport authority.

pub fn authority(host: &str, port: u16) -> Result<String, String> {
    let host = host.trim();
    let unwrapped = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = unwrapped.parse::<std::net::IpAddr>() {
        return Ok(std::net::SocketAddr::new(ip, port).to_string());
    }
    if host.is_empty()
        || !host.is_ascii()
        || !host
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '.' | '_'))
    {
        return Err("invalid endpoint host: expected a hostname or IP address".into());
    }
    Ok(format!("{host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_dns_ipv4_and_both_ipv6_spellings() {
        for (host, expected) in [
            ("bmc.example", "bmc.example:443"),
            ("127.0.0.1", "127.0.0.1:443"),
            ("::1", "[::1]:443"),
            ("[::1]", "[::1]:443"),
            ("  bmc.example  ", "bmc.example:443"),
        ] {
            assert_eq!(authority(host, 443).unwrap(), expected);
        }
    }

    #[test]
    fn host_cannot_inject_another_url_component_or_port() {
        for host in [
            "",
            " ",
            "user@bmc",
            "bmc/path",
            "bmc\\path",
            "bmc?query",
            "bmc#fragment",
            "bmc:80",
            "[::1",
            "::1]",
            "bmc\nheader",
            "bmc%2fpath",
        ] {
            assert!(authority(host, 443).is_err(), "{host:?}");
        }
    }
}
