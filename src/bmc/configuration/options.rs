use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bmc_feature_api::configuration::{
    ConfigOptionResourceSnapshot, ConfigOptionResourceState, ConfigValue, DiscoveredConfigOption,
};

use super::{configuration_payload_digest, configuration_read_response, ConfigurationScope};
use crate::web::state::AppState;

const RESOURCE_IDS: [&str; 4] = [
    "local.video_inputs",
    "local.serial_ports",
    "local.atx_gpio_chips",
    "local.atx_usb_relays",
];

fn matches_resource(resource_id: &str, name: &str) -> bool {
    match resource_id {
        "local.video_inputs" => name.strip_prefix("video").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
        }),
        "local.serial_ports" => ["ttyUSB", "ttyACM", "ttyAMA", "ttyS"].iter().any(|prefix| {
            name.strip_prefix(prefix).is_some_and(|suffix| {
                !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
            })
        }),
        "local.atx_gpio_chips" => name.strip_prefix("gpiochip").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
        }),
        "local.atx_usb_relays" => {
            name.starts_with("usbrelay")
                || name.strip_prefix("hidraw").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
                })
        }
        _ => false,
    }
}

async fn scan_directory(
    directory: &Path,
    resource_id: &str,
    options: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        if matches_resource(resource_id, &name.to_string_lossy()) {
            options.push(entry.path());
        }
    }
    Ok(())
}

async fn discover(resource_id: &str) -> (ConfigOptionResourceState, Vec<DiscoveredConfigOption>) {
    if !RESOURCE_IDS.contains(&resource_id) {
        return (
            ConfigOptionResourceState::Failed {
                code: "option_resource_unknown".to_string(),
                message: "The requested option resource is not registered".to_string(),
            },
            Vec::new(),
        );
    }

    let mut paths = Vec::new();
    if let Err(error) = scan_directory(Path::new("/dev"), resource_id, &mut paths).await {
        return (
            ConfigOptionResourceState::Failed {
                code: "option_discovery_failed".to_string(),
                message: format!("Local device discovery failed: {error}"),
            },
            Vec::new(),
        );
    }
    paths.sort();
    paths.dedup();
    let options = paths
        .into_iter()
        .map(|path| {
            let label = path.to_string_lossy().to_string();
            DiscoveredConfigOption {
                value: ConfigValue::String(label.clone()),
                label,
            }
        })
        .collect();
    (ConfigOptionResourceState::Ready, options)
}

async fn refresh_option_snapshot(
    state: &AppState,
    resource_id: &str,
) -> Result<ConfigOptionResourceSnapshot, String> {
    let (resource_state, options) = discover(resource_id).await;
    let digest = configuration_payload_digest(&serde_json::json!({
        "state": resource_state,
        "options": options,
    }))?;
    let (revision, changed) = state
        .configuration
        .observe_options(resource_id, digest)
        .await;
    let scope = ConfigurationScope::Options {
        resource_id: resource_id.to_string(),
    };
    if changed {
        state.configuration.advance_scope(&scope);
        state.configuration.publish(
            scope,
            format!("option-discovery:{resource_id}:{revision}"),
            vec!["options".to_string()],
            Vec::new(),
        );
    }
    Ok(ConfigOptionResourceSnapshot {
        resource_id: resource_id.to_string(),
        epoch: state.configuration.epoch().to_string(),
        revision,
        state: resource_state,
        options,
    })
}

pub async fn configuration_options_get(
    AxumPath(resource_id): AxumPath<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !RESOURCE_IDS.contains(&resource_id.as_str()) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "success": false,
                "code": "option_resource_unknown",
                "message": "The requested option resource is not registered",
            })),
        )
            .into_response();
    }
    match refresh_option_snapshot(&state, &resource_id).await {
        Ok(snapshot) => {
            let scope = ConfigurationScope::Options {
                resource_id: resource_id.clone(),
            };
            match serde_json::to_value(snapshot) {
                Ok(value) => configuration_read_response(&state, &scope, value),
                Err(error) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "success": false,
                        "code": "option_snapshot_invalid",
                        "message": error.to_string(),
                    })),
                )
                    .into_response(),
            }
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "success": false,
                "code": "option_snapshot_failed",
                "message": error,
            })),
        )
            .into_response(),
    }
}

pub fn start_configuration_option_monitor(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            for resource_id in RESOURCE_IDS {
                if let Err(error) = refresh_option_snapshot(&state, resource_id).await {
                    tracing::warn!(resource_id, %error, "configuration option discovery failed");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_are_classified_by_registered_resource() {
        assert!(matches_resource("local.video_inputs", "video0"));
        assert!(!matches_resource("local.video_inputs", "video"));
        assert!(matches_resource("local.serial_ports", "ttyUSB12"));
        assert!(matches_resource("local.atx_gpio_chips", "gpiochip2"));
        assert!(matches_resource("local.atx_usb_relays", "hidraw3"));
        assert!(!matches_resource("local.atx_usb_relays", "event3"));
    }
}
