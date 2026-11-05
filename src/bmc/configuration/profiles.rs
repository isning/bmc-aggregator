use std::collections::BTreeMap;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bmc_feature_api::configuration::{
    BackendAssignmentMutation, ConfigValue, FeatureAssignmentMutation, ProviderAssignmentMutation,
};
use serde::Deserialize;

use super::dto::{DeviceConfigurationMutation, DeviceCredentialsMutation, DeviceEndpointMutation};

#[derive(Clone, Debug, Deserialize)]
pub struct DeviceCreationDraftRequest {
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub username: Option<String>,
}

fn backend(backend_id: &str) -> BackendAssignmentMutation {
    BackendAssignmentMutation {
        backend_id: backend_id.to_string(),
        params: BTreeMap::new(),
        secrets: BTreeMap::new(),
    }
}

fn backend_with_params(
    backend_id: &str,
    params: impl IntoIterator<Item = (&'static str, ConfigValue)>,
) -> BackendAssignmentMutation {
    BackendAssignmentMutation {
        backend_id: backend_id.to_string(),
        params: params
            .into_iter()
            .map(|(id, value)| (id.to_string(), value))
            .collect(),
        secrets: BTreeMap::new(),
    }
}

fn single(backend_id: &str) -> FeatureAssignmentMutation {
    FeatureAssignmentMutation::Single {
        backend: backend(backend_id),
    }
}

fn idrac_backend(backend_id: &str, generation: i64) -> BackendAssignmentMutation {
    backend_with_params(
        backend_id,
        [("idrac_generation", ConfigValue::Integer(generation))],
    )
}

fn idrac_assignments(generation: i64) -> BTreeMap<String, FeatureAssignmentMutation> {
    BTreeMap::from([
        (
            "video".to_string(),
            FeatureAssignmentMutation::Single {
                backend: idrac_backend("idrac-avct-console", generation),
            },
        ),
        (
            "hid".to_string(),
            FeatureAssignmentMutation::Single {
                backend: idrac_backend("idrac-avct-console", generation),
            },
        ),
        (
            "msd".to_string(),
            FeatureAssignmentMutation::Single {
                backend: idrac_backend("idrac-legacy-media", generation),
            },
        ),
        ("power".to_string(), single("ipmi")),
        ("identify".to_string(), single("ipmi")),
        (
            "boot".to_string(),
            FeatureAssignmentMutation::ProviderSet {
                providers: vec![
                    ProviderAssignmentMutation {
                        id: "ipmi".to_string(),
                        backend: backend("ipmi"),
                    },
                    ProviderAssignmentMutation {
                        id: "idrac-wsman".to_string(),
                        backend: idrac_backend("idrac-wsman", generation),
                    },
                ],
            },
        ),
        ("firmware".to_string(), FeatureAssignmentMutation::Disabled),
        ("health".to_string(), FeatureAssignmentMutation::Disabled),
        ("audio".to_string(), FeatureAssignmentMutation::Disabled),
    ])
}

fn assignments(profile_id: &str) -> Result<BTreeMap<String, FeatureAssignmentMutation>, String> {
    Ok(match profile_id {
        "mahogany" => BTreeMap::from([
            ("video".to_string(), single("mahogany")),
            ("hid".to_string(), single("mahogany")),
            ("msd".to_string(), single("mahogany")),
            ("firmware".to_string(), single("mahogany")),
            ("power".to_string(), single("wol")),
            ("boot".to_string(), FeatureAssignmentMutation::Disabled),
            ("identify".to_string(), FeatureAssignmentMutation::Disabled),
            ("health".to_string(), FeatureAssignmentMutation::Disabled),
            ("audio".to_string(), FeatureAssignmentMutation::Disabled),
        ]),
        "vnc" => BTreeMap::from([
            ("video".to_string(), single("vnc")),
            ("hid".to_string(), single("vnc")),
            ("msd".to_string(), FeatureAssignmentMutation::Disabled),
            ("firmware".to_string(), FeatureAssignmentMutation::Disabled),
            ("power".to_string(), FeatureAssignmentMutation::Disabled),
            ("boot".to_string(), FeatureAssignmentMutation::Disabled),
            ("identify".to_string(), FeatureAssignmentMutation::Disabled),
            ("health".to_string(), FeatureAssignmentMutation::Disabled),
            ("audio".to_string(), FeatureAssignmentMutation::Disabled),
        ]),
        "idrac6" => idrac_assignments(6),
        "idrac8" => idrac_assignments(8),
        "redfish" => BTreeMap::from([
            ("video".to_string(), FeatureAssignmentMutation::Disabled),
            ("hid".to_string(), FeatureAssignmentMutation::Disabled),
            ("msd".to_string(), single("redfish")),
            ("power".to_string(), single("redfish")),
            (
                "boot".to_string(),
                FeatureAssignmentMutation::ProviderSet {
                    providers: vec![ProviderAssignmentMutation {
                        id: "redfish".to_string(),
                        backend: backend("redfish"),
                    }],
                },
            ),
            ("identify".to_string(), single("redfish")),
            ("firmware".to_string(), single("redfish")),
            ("health".to_string(), single("redfish")),
            ("audio".to_string(), FeatureAssignmentMutation::Disabled),
        ]),
        _ => return Err(format!("unknown device creation profile '{profile_id}'")),
    })
}

pub async fn device_creation_draft(
    Path(profile_id): Path<String>,
    Json(request): Json<DeviceCreationDraftRequest>,
) -> Response {
    if request.id.trim().is_empty()
        || request.name.trim().is_empty()
        || request.host.trim().is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "code": "invalid_creation_draft_request",
                "message": "id, name, and host are required",
            })),
        )
            .into_response();
    }
    let assignments = match assignments(&profile_id) {
        Ok(assignments) => assignments,
        Err(message) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "code": "creation_profile_not_found",
                    "message": message,
                })),
            )
                .into_response();
        }
    };
    Json(serde_json::json!({
        "draft": DeviceConfigurationMutation {
            id: Some(request.id),
            name: Some(request.name),
            endpoint: Some(DeviceEndpointMutation {
                host: Some(request.host),
                port: request.port.or(Some(443)),
            }),
            credentials: Some(DeviceCredentialsMutation {
                username: Some(request.username.unwrap_or_else(|| "admin".to_string())),
                password: None,
            }),
            assignments: Some(assignments),
        }
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advertised_profile_builds_an_explicit_complete_assignment_set() {
        let catalog = super::super::configuration_catalog().unwrap();
        for profile in &catalog.creation_profiles {
            let assignments = assignments(&profile.profile_id).unwrap();
            assert_eq!(assignments.len(), catalog.features.len());
            assert!(assignments.values().all(|assignment| !matches!(
                assignment,
                FeatureAssignmentMutation::ProviderSet { providers } if providers.is_empty()
            )));
        }
    }
}
