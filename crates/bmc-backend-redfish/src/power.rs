use async_trait::async_trait;
use bmc_feature_api::power::{PowerAction, PowerBackend, PowerCapabilities};
use serde_json::Value;

use crate::{RedfishClient, RedfishConfig};

pub struct RedfishPowerBackend {
    service: RedfishClient,
}

impl RedfishPowerBackend {
    pub fn new(config: RedfishConfig) -> Result<Self, String> {
        Ok(Self {
            service: RedfishClient::new(config)?,
        })
    }

    #[cfg(test)]
    fn for_test(service: RedfishClient) -> Self {
        Self { service }
    }

    async fn reset_target(&self) -> Result<(String, Vec<String>), String> {
        let system = self.service.system().await?.1;
        let reset = system
            .pointer("/Actions/#ComputerSystem.Reset")
            .ok_or_else(|| "Redfish system does not advertise ComputerSystem.Reset".to_string())?;
        let target = reset
            .get("target")
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish reset action has no target".to_string())?;
        let allowed = reset
            .get("ResetType@Redfish.AllowableValues")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok((target.to_string(), allowed))
    }
}

#[async_trait]
impl PowerBackend for RedfishPowerBackend {
    async fn capabilities(&self) -> PowerCapabilities {
        let Ok((_, allowed)) = self.reset_target().await else {
            return PowerCapabilities::default();
        };
        let advertised: Vec<_> = allowed.iter().map(String::as_str).collect();
        PowerCapabilities {
            actions: advertised
                .into_iter()
                .filter_map(|action| match action {
                    "On" => Some(PowerAction::On),
                    "ForceOff" => Some(PowerAction::Off),
                    "ForceRestart" => Some(PowerAction::Reset),
                    "PowerCycle" => Some(PowerAction::PowerCycle),
                    "GracefulShutdown" => Some(PowerAction::GracefulShutdown),
                    "GracefulRestart" => Some(PowerAction::GracefulRestart),
                    _ => None,
                })
                .collect(),
        }
    }

    async fn status(&self) -> Result<String, String> {
        let system = self.service.system().await?.1;
        Ok(match system
            .get("PowerState")
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
        {
            "On" => "on",
            "Off" => "off",
            _ => "unknown",
        }
        .to_string())
    }

    async fn control(&self, action: &str) -> Result<(), String> {
        let action = PowerAction::from_command(action)
            .ok_or_else(|| format!("unknown power action: {action}"))?;
        let reset_type = match action {
            PowerAction::On => "On",
            PowerAction::Off => "ForceOff",
            PowerAction::Reset => "ForceRestart",
            PowerAction::PowerCycle => "PowerCycle",
            PowerAction::GracefulShutdown => "GracefulShutdown",
            PowerAction::GracefulRestart => "GracefulRestart",
        };
        let (target, allowed) = self.reset_target().await?;
        if !allowed.iter().any(|value| value == reset_type) {
            return Err(format!(
                "Redfish reset type {reset_type} is not supported by this system"
            ));
        }
        self.service
            .post(&target, serde_json::json!({"ResetType": reset_type}))
            .await
    }

    async fn wake(&self, _mac_address: &str) -> Result<(), String> {
        Err("Wake-on-LAN is not a Redfish power capability; configure the wol backend".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use reqwest::Url;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn absent_or_empty_allowable_values_never_authorize_reset() {
        for values in [None, Some(serde_json::json!([]))] {
            let mut action = serde_json::json!({"target": "/reset"});
            if let Some(values) = values {
                action["ResetType@Redfish.AllowableValues"] = values;
            }
            let resets = Arc::new(AtomicUsize::new(0));
            let system = serde_json::json!({"Actions": {"#ComputerSystem.Reset": action}});
            let app = Router::new()
                .route(
                    "/redfish/v1/",
                    get(|| async { Json(serde_json::json!({"Systems":{"@odata.id":"/systems"}})) }),
                )
                .route(
                    "/systems",
                    get(|| async {
                        Json(serde_json::json!({"Members":[{"@odata.id":"/system"}]}))
                    }),
                )
                .route(
                    "/system",
                    get(move || {
                        let system = system.clone();
                        async move { Json(system) }
                    }),
                )
                .route(
                    "/reset",
                    post({
                        let resets = resets.clone();
                        move || {
                            let resets = resets.clone();
                            async move {
                                resets.fetch_add(1, Ordering::SeqCst);
                                axum::http::StatusCode::NO_CONTENT
                            }
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            let backend = RedfishPowerBackend::for_test(RedfishClient::for_test(
                Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
            ));
            assert!(backend.capabilities().await.actions.is_empty());
            assert!(backend.control("reset").await.is_err());
            assert_eq!(resets.load(Ordering::SeqCst), 0);
            server.abort();
        }
    }

    #[tokio::test]
    async fn every_advertised_power_action_uses_its_canonical_name_and_wire_value() {
        let received = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let app = Router::new()
            .route("/redfish/v1/", get(|| async { Json(serde_json::json!({"Systems":{"@odata.id":"/systems"}})) }))
            .route("/systems", get(|| async { Json(serde_json::json!({"Members":[{"@odata.id":"/system"}]})) }))
            .route("/system", get(|| async { Json(serde_json::json!({"Actions":{"#ComputerSystem.Reset":{"target":"/reset","ResetType@Redfish.AllowableValues":["On","ForceOff","ForceRestart","PowerCycle","GracefulShutdown","GracefulRestart"]}}})) }))
            .route("/reset", post({
                let received = received.clone();
                move |Json(body): Json<Value>| {
                    let received = received.clone();
                    async move {
                        received.lock().await.push(body["ResetType"].as_str().unwrap().to_string());
                        axum::http::StatusCode::NO_CONTENT
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let backend = RedfishPowerBackend::for_test(RedfishClient::for_test(
            Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
        ));
        let advertised = backend.capabilities().await.actions;
        let expected = [
            "On",
            "ForceOff",
            "ForceRestart",
            "PowerCycle",
            "GracefulShutdown",
            "GracefulRestart",
        ];
        assert_eq!(advertised.len(), expected.len());
        for (action, expected) in advertised.into_iter().zip(expected) {
            let name = serde_json::to_value(action).unwrap();
            backend.control(name.as_str().unwrap()).await.unwrap();
            assert_eq!(received.lock().await.last().unwrap(), expected);
        }
        for (alias, expected) in [
            ("short", "On"),
            ("press", "On"),
            ("long", "ForceOff"),
            ("hold", "ForceOff"),
            ("cycle", "PowerCycle"),
            ("graceful_reset", "GracefulRestart"),
        ] {
            backend.control(alias).await.unwrap();
            assert_eq!(received.lock().await.last().unwrap(), expected);
        }
        let count = received.lock().await.len();
        assert!(backend.control("unknown").await.is_err());
        assert_eq!(received.lock().await.len(), count);
        server.abort();
    }

    #[tokio::test]
    async fn discovers_system_and_invokes_advertised_reset_action() {
        let resets = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/redfish/v1/", get(|| async { Json(serde_json::json!({"Systems":{"@odata.id":"/redfish/v1/Systems"}})) }))
            .route("/redfish/v1/Systems", get(|| async { Json(serde_json::json!({"Members":[{"@odata.id":"/redfish/v1/Systems/1"}]})) }))
            .route("/redfish/v1/Systems/1", get(|| async { Json(serde_json::json!({"PowerState":"On","Actions":{"#ComputerSystem.Reset":{"target":"/reset","ResetType@Redfish.AllowableValues":["ForceRestart"]}}})) }))
            .route("/reset", post({ let resets = resets.clone(); move || { let resets = resets.clone(); async move { resets.fetch_add(1, Ordering::SeqCst); axum::http::StatusCode::NO_CONTENT } } }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let backend = RedfishPowerBackend::for_test(RedfishClient::for_test(
            Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
        ));
        assert_eq!(backend.status().await.unwrap(), "on");
        backend.control("reset").await.unwrap();
        assert_eq!(resets.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
