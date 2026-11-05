use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;

use crate::bmc::configuration::{
    configuration_read_response, mutate_configuration_scope, ConfigurationHttpError,
    ConfigurationScope,
};
use crate::web::state::AppState;

fn parse_listen(listen: &str) -> (String, u16) {
    if let Some((host, port)) = listen.rsplit_once(':') {
        if let Ok(port) = port.parse::<u16>() {
            let host = host.trim_matches(['[', ']']);
            return (host.to_string(), port);
        }
    }
    ("0.0.0.0".to_string(), 8080)
}

fn web_config_response(config: &crate::config::Config) -> serde_json::Value {
    let (bind_address, http_port) = parse_listen(&config.server.listen);
    serde_json::json!({
        "http_port": http_port,
        "https_port": 8443,
        "bind_addresses": [bind_address.clone()],
        "bind_address": bind_address,
        "https_enabled": false,
        "has_custom_cert": false,
        "restart_required": true,
        "frontend_dir": config.server.frontend_dir,
    })
}

fn web_scope() -> ConfigurationScope {
    ConfigurationScope::Global {
        section: "server.web".to_string(),
    }
}

pub async fn server_web_config(State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.config.read().await;
    configuration_read_response(&state, &web_scope(), web_config_response(&cfg))
}

fn invalid(message: &str) -> ConfigurationHttpError {
    ConfigurationHttpError::bad_request("server_web_configuration_invalid", message)
}

fn normalize_host(value: &str) -> Result<String, ConfigurationHttpError> {
    let host = if value.starts_with('[') && value.ends_with(']') {
        &value[1..value.len() - 1]
    } else {
        value
    };
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(ip.to_string());
    }
    if host.is_empty()
        || host.len() > 253
        || !host.is_ascii()
        || host.trim_end_matches('.').split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(invalid(
            "bind_address must be an IP address or a valid hostname",
        ));
    }
    Ok(host.to_string())
}

fn format_listen(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn apply_web_update(
    config: &mut crate::config::Config,
    patch: &serde_json::Value,
) -> Result<(), ConfigurationHttpError> {
    let fields = patch
        .as_object()
        .ok_or_else(|| invalid("server web configuration must be an object"))?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "listen" | "bind_address" | "bind_addresses" | "http_port" | "frontend_dir"
        )
    }) {
        return Err(invalid("unsupported server web configuration field"));
    }
    let text = |key: &str| -> Result<Option<&str>, ConfigurationHttpError> {
        fields
            .get(key)
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| invalid("server web text fields must be strings"))
            })
            .transpose()
    };
    let port = fields
        .get("http_port")
        .map(|value| {
            value
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| invalid("http_port must be an integer between 0 and 65535"))
        })
        .transpose()?;
    let address = text("bind_address")?.map(normalize_host).transpose()?;
    let addresses = fields
        .get("bind_addresses")
        .map(|value| {
            let values = value
                .as_array()
                .filter(|values| values.len() == 1)
                .ok_or_else(|| invalid("exactly one bind address is supported"))?;
            normalize_host(
                values[0]
                    .as_str()
                    .ok_or_else(|| invalid("bind address must be a string"))?,
            )
        })
        .transpose()?;
    if address.is_some() && addresses.is_some() && address != addresses {
        return Err(invalid("bind_address and bind_addresses disagree"));
    }
    let listen = if let Some(listen) = text("listen")? {
        if address.is_some() || addresses.is_some() || port.is_some() {
            return Err(invalid(
                "listen cannot be combined with bind address or port fields",
            ));
        }
        if let Ok(address) = listen.parse::<std::net::SocketAddr>() {
            Some(address.to_string())
        } else {
            let (host, port) = listen
                .rsplit_once(':')
                .ok_or_else(|| invalid("listen must include a host and port"))?;
            if host.contains(':') {
                return Err(invalid("IPv6 listen addresses must use brackets"));
            }
            let port = port
                .parse::<u16>()
                .map_err(|_| invalid("invalid listen port"))?;
            Some(format_listen(&normalize_host(host)?, port))
        }
    } else if address.is_some() || addresses.is_some() || port.is_some() {
        let (current_host, current_port) = parse_listen(&config.server.listen);
        let host = match address.or(addresses) {
            Some(host) => host,
            None => normalize_host(&current_host)?,
        };
        Some(format_listen(&host, port.unwrap_or(current_port)))
    } else {
        None
    };
    let frontend = text("frontend_dir")?;
    if frontend.is_some_and(|path| path.trim().is_empty() || path.contains('\0')) {
        return Err(invalid("frontend_dir must be a nonempty valid path"));
    }
    if let Some(listen) = listen {
        config.server.listen = listen;
    }
    if let Some(frontend) = frontend {
        config.server.frontend_dir = frontend.into();
    }
    Ok(())
}

pub async fn server_web_config_update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<serde_json::Value>,
) -> Response {
    mutate_configuration_scope(
        &state,
        &headers,
        web_scope(),
        &patch,
        vec!["server.web".to_string()],
        |config| Ok(web_config_response(config)),
        |config| apply_web_update(config, &patch),
    )
    .await
}
