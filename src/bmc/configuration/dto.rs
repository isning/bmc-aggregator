use std::collections::{BTreeMap, HashMap, HashSet};

use bmc_feature_api::configuration::{
    BackendAssignmentDto, BackendAssignmentMutation, ConfigFieldKind, ConfigValue,
    FeatureAssignmentDto, FeatureAssignmentMutation, FeatureBindingKind, ProviderAssignmentDto,
    SecretMutation, SecretState,
};
use serde::{Deserialize, Serialize};

use crate::config::{DeviceConfig, DeviceFeatureConfig, DeviceFeatureProviderConfig};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeviceEndpointDto {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeviceCredentialsDto {
    pub username: String,
    pub password: SecretState,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeviceConfigurationDto {
    pub id: String,
    pub name: String,
    pub endpoint: DeviceEndpointDto,
    pub credentials: DeviceCredentialsDto,
    pub assignments: BTreeMap<String, FeatureAssignmentDto>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceEndpointMutation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceCredentialsMutation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<SecretMutation>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceConfigurationMutation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<DeviceEndpointMutation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<DeviceCredentialsMutation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignments: Option<BTreeMap<String, FeatureAssignmentMutation>>,
}

fn explicit_value(
    params: &HashMap<String, serde_json::Value>,
    id: &str,
) -> Result<Option<ConfigValue>, String> {
    let Some(value) = params.get(id) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|_| format!("configuration parameter '{id}' is not a primitive value"))
}

fn secret_configured(
    dev: &DeviceConfig,
    params: &HashMap<String, serde_json::Value>,
    id: &str,
) -> bool {
    match params.get(id) {
        Some(serde_json::Value::String(value)) => !value.is_empty(),
        Some(serde_json::Value::Null) => false,
        Some(_) => false,
        None if matches!(id, "password" | "vnc_password") => !dev.auth.password.is_empty(),
        None => false,
    }
}

fn backend_dto(
    dev: &DeviceConfig,
    feature_id: &str,
    backend_id: &str,
    params: &HashMap<String, serde_json::Value>,
) -> Result<BackendAssignmentDto, String> {
    let catalog = super::configuration_catalog()?;
    let descriptor = catalog
        .backends
        .iter()
        .find(|descriptor| {
            descriptor.feature_id == feature_id && descriptor.backend_id == backend_id
        })
        .ok_or_else(|| {
            format!("backend '{backend_id}' is not registered for feature '{feature_id}'")
        })?;
    super::validate_backend_params(feature_id, backend_id, params)?;

    let mut values = BTreeMap::new();
    let mut secrets = BTreeMap::new();
    for field in &descriptor.fields {
        if matches!(field.field, ConfigFieldKind::Secret) {
            secrets.insert(
                field.id.clone(),
                SecretState {
                    configured: secret_configured(dev, params, &field.id),
                },
            );
        } else if let Some(value) = explicit_value(params, &field.id)? {
            values.insert(field.id.clone(), value);
        }
    }
    Ok(BackendAssignmentDto {
        backend_id: backend_id.to_string(),
        params: values,
        secrets,
    })
}

pub fn device_configuration_dto(dev: &DeviceConfig) -> Result<DeviceConfigurationDto, String> {
    let catalog = super::configuration_catalog()?;
    let mut assignments = BTreeMap::new();
    for descriptor in &catalog.features {
        let configured = dev.features.get(&descriptor.feature_id);
        let assignment = match configured {
            None => FeatureAssignmentDto::Disabled,
            Some(configured) if configured.disabled => FeatureAssignmentDto::Disabled,
            Some(configured) => match &descriptor.binding {
                FeatureBindingKind::Single => match configured.backend.as_deref() {
                    Some(backend_id) if !backend_id.trim().is_empty() => {
                        FeatureAssignmentDto::Single {
                            backend: backend_dto(
                                dev,
                                &descriptor.feature_id,
                                backend_id,
                                &configured.backend_params,
                            )?,
                        }
                    }
                    _ => FeatureAssignmentDto::Disabled,
                },
                FeatureBindingKind::ProviderSet { .. } => {
                    let providers = if configured.providers.is_empty() {
                        configured
                            .backend
                            .as_deref()
                            .filter(|backend| !backend.trim().is_empty())
                            .map(|backend_id| {
                                backend_dto(
                                    dev,
                                    &descriptor.feature_id,
                                    backend_id,
                                    &configured.backend_params,
                                )
                                .map(|backend| {
                                    vec![ProviderAssignmentDto {
                                        id: backend_id.to_string(),
                                        backend,
                                    }]
                                })
                            })
                            .transpose()?
                            .unwrap_or_default()
                    } else {
                        configured
                            .providers
                            .iter()
                            .enumerate()
                            .map(|(index, provider)| {
                                backend_dto(
                                    dev,
                                    &descriptor.feature_id,
                                    &provider.backend,
                                    &provider.backend_params,
                                )
                                .map(|backend| {
                                    ProviderAssignmentDto {
                                        id: if provider.id.is_empty() {
                                            format!("{}-{}", provider.backend, index + 1)
                                        } else {
                                            provider.id.clone()
                                        },
                                        backend,
                                    }
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?
                    };
                    if providers.is_empty() {
                        FeatureAssignmentDto::Disabled
                    } else {
                        FeatureAssignmentDto::ProviderSet { providers }
                    }
                }
            },
        };
        assignments.insert(descriptor.feature_id.clone(), assignment);
    }
    Ok(DeviceConfigurationDto {
        id: dev.id.clone(),
        name: dev.name.clone(),
        endpoint: DeviceEndpointDto {
            host: dev.host.clone(),
            port: dev.port,
        },
        credentials: DeviceCredentialsDto {
            username: dev.auth.username.clone(),
            password: SecretState {
                configured: !dev.auth.password.is_empty(),
            },
        },
        assignments,
    })
}

fn json_params(
    values: BTreeMap<String, ConfigValue>,
) -> Result<HashMap<String, serde_json::Value>, String> {
    values
        .into_iter()
        .map(|(id, value)| {
            serde_json::to_value(value)
                .map(|value| (id, value))
                .map_err(|error| format!("could not serialize configuration parameter: {error}"))
        })
        .collect()
}

fn apply_secret_mutations(
    target: &mut HashMap<String, serde_json::Value>,
    existing: Option<&HashMap<String, serde_json::Value>>,
    secrets: BTreeMap<String, SecretMutation>,
) {
    for (id, mutation) in secrets {
        match mutation {
            SecretMutation::Keep => {
                if let Some(value) = existing.and_then(|existing| existing.get(&id)) {
                    target.insert(id, value.clone());
                }
            }
            SecretMutation::Replace { value } => {
                target.insert(id, serde_json::Value::String(value));
            }
            SecretMutation::Clear => {
                target.insert(id, serde_json::Value::Null);
            }
        }
    }
}

fn backend_config(
    feature_id: &str,
    mutation: BackendAssignmentMutation,
    existing_backend: Option<&str>,
    existing_params: Option<&HashMap<String, serde_json::Value>>,
) -> Result<(String, HashMap<String, serde_json::Value>), String> {
    let catalog = super::configuration_catalog()?;
    let descriptor = catalog
        .backends
        .iter()
        .find(|descriptor| {
            descriptor.feature_id == feature_id && descriptor.backend_id == mutation.backend_id
        })
        .ok_or_else(|| {
            format!(
                "backend '{}' is not registered for feature '{feature_id}'",
                mutation.backend_id
            )
        })?;
    if mutation
        .secrets
        .values()
        .any(|mutation| matches!(mutation, SecretMutation::Replace { value } if value.is_empty()))
    {
        return Err("replacement secret must not be empty; use clear explicitly".to_string());
    }
    let mut params = json_params(descriptor.defaults.clone())?;
    params.extend(json_params(mutation.params)?);
    let existing = (existing_backend == Some(mutation.backend_id.as_str()))
        .then_some(existing_params)
        .flatten();
    apply_secret_mutations(&mut params, existing, mutation.secrets);
    super::validate_backend_params(feature_id, &mutation.backend_id, &params)?;
    Ok((mutation.backend_id, params))
}

fn validate_provider_count(
    feature_id: &str,
    min: usize,
    max: Option<usize>,
    count: usize,
) -> Result<(), String> {
    if count < min || max.is_some_and(|max| count > max) {
        return Err(format!(
            "feature '{feature_id}' requires between {min} and {} providers",
            max.map_or_else(|| "unbounded".to_string(), |max| max.to_string())
        ));
    }
    Ok(())
}

fn apply_assignment(
    dev: &mut DeviceConfig,
    feature_id: &str,
    mutation: FeatureAssignmentMutation,
) -> Result<(), String> {
    let catalog = super::configuration_catalog()?;
    let descriptor = catalog
        .features
        .iter()
        .find(|descriptor| descriptor.feature_id == feature_id)
        .ok_or_else(|| format!("unknown feature '{feature_id}'"))?;
    let existing = dev.features.get(feature_id).cloned().unwrap_or_default();
    let feature_params = existing.feature_params.clone();
    let next = match (mutation, &descriptor.binding) {
        (FeatureAssignmentMutation::Disabled, _) if descriptor.allow_disabled => {
            DeviceFeatureConfig {
                disabled: true,
                feature_params,
                ..Default::default()
            }
        }
        (FeatureAssignmentMutation::Disabled, _) => {
            return Err(format!("feature '{feature_id}' cannot be disabled"));
        }
        (FeatureAssignmentMutation::Single { backend }, FeatureBindingKind::Single) => {
            let (backend_id, backend_params) = backend_config(
                feature_id,
                backend,
                existing.backend.as_deref(),
                Some(&existing.backend_params),
            )?;
            DeviceFeatureConfig {
                disabled: false,
                backend: Some(backend_id),
                backend_params,
                feature_params,
                providers: Vec::new(),
            }
        }
        (
            FeatureAssignmentMutation::ProviderSet { providers },
            FeatureBindingKind::ProviderSet { min, max },
        ) => {
            validate_provider_count(feature_id, *min, *max, providers.len())?;
            let mut ids = HashSet::new();
            let mut next_providers = Vec::with_capacity(providers.len());
            for provider in providers {
                if provider.id.trim().is_empty() || !ids.insert(provider.id.clone()) {
                    return Err(format!(
                        "feature '{feature_id}' provider IDs must be non-empty and unique"
                    ));
                }
                let previous = existing
                    .providers
                    .iter()
                    .find(|candidate| candidate.id == provider.id);
                let (backend, backend_params) = backend_config(
                    feature_id,
                    provider.backend,
                    previous.map(|provider| provider.backend.as_str()),
                    previous.map(|provider| &provider.backend_params),
                )?;
                next_providers.push(DeviceFeatureProviderConfig {
                    id: provider.id,
                    backend,
                    backend_params,
                });
            }
            DeviceFeatureConfig {
                disabled: false,
                backend: None,
                backend_params: HashMap::new(),
                feature_params,
                providers: next_providers,
            }
        }
        (FeatureAssignmentMutation::Single { .. }, FeatureBindingKind::ProviderSet { .. }) => {
            return Err(format!("feature '{feature_id}' requires a provider set"));
        }
        (FeatureAssignmentMutation::ProviderSet { .. }, FeatureBindingKind::Single) => {
            return Err(format!("feature '{feature_id}' accepts one backend"));
        }
    };
    dev.features.insert(feature_id.to_string(), next);
    Ok(())
}

pub fn apply_device_configuration_mutation(
    dev: &mut DeviceConfig,
    mutation: DeviceConfigurationMutation,
) -> Result<(), String> {
    if let Some(id) = mutation.id {
        dev.id = id;
    }
    if let Some(name) = mutation.name {
        dev.name = name;
    }
    if let Some(endpoint) = mutation.endpoint {
        if let Some(host) = endpoint.host {
            dev.host = host;
        }
        if let Some(port) = endpoint.port {
            dev.port = port;
        }
    }
    if let Some(credentials) = mutation.credentials {
        if let Some(username) = credentials.username {
            dev.auth.username = username;
        }
        if let Some(password) = credentials.password {
            match password {
                SecretMutation::Keep => {}
                SecretMutation::Replace { value } => dev.auth.password = value,
                SecretMutation::Clear => dev.auth.password.clear(),
            }
        }
    }
    if let Some(assignments) = mutation.assignments {
        for (feature_id, assignment) in assignments {
            apply_assignment(dev, &feature_id, assignment)?;
        }
    }
    Ok(())
}

pub fn materialize_unmentioned_features_as_disabled(dev: &mut DeviceConfig) -> Result<(), String> {
    for feature in &super::configuration_catalog()?.features {
        dev.features
            .entry(feature.feature_id.clone())
            .or_insert_with(|| DeviceFeatureConfig {
                disabled: true,
                ..Default::default()
            });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthConfig;

    fn device() -> DeviceConfig {
        DeviceConfig {
            id: "device-1".to_string(),
            name: "Device".to_string(),
            protocol: "explicit".to_string(),
            host: "192.0.2.10".to_string(),
            port: 443,
            kvm_port: None,
            ipmi_host: None,
            ipmi_port: Some(623),
            auth: AuthConfig {
                username: "admin".to_string(),
                password: "shared-secret".to_string(),
            },
            max_fps: 60,
            video_backend: "mahogany".to_string(),
            encoder_backend: "software".to_string(),
            audio_backend: None,
            features: HashMap::new(),
            video_device: None,
            video_format: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            video_quality: None,
        }
    }

    #[test]
    fn tagged_assignments_preserve_provider_order_and_secret_state() {
        let mut dev = device();
        dev.features.insert(
            "boot".to_string(),
            DeviceFeatureConfig {
                providers: vec![
                    DeviceFeatureProviderConfig {
                        id: "primary".to_string(),
                        backend: "ipmi".to_string(),
                        backend_params: HashMap::new(),
                    },
                    DeviceFeatureProviderConfig {
                        id: "firmware".to_string(),
                        backend: "idrac-wsman".to_string(),
                        backend_params: HashMap::from([(
                            "idrac_generation".to_string(),
                            serde_json::json!(6),
                        )]),
                    },
                ],
                ..Default::default()
            },
        );
        let dto = device_configuration_dto(&dev).unwrap();
        let FeatureAssignmentDto::ProviderSet { providers } = &dto.assignments["boot"] else {
            panic!("boot must be a provider set");
        };
        assert_eq!(providers[0].id, "primary");
        assert_eq!(providers[1].id, "firmware");
        assert!(providers[0].backend.secrets["password"].configured);
        assert_eq!(
            providers[1].backend.params["idrac_generation"],
            ConfigValue::Integer(6)
        );
    }

    #[test]
    fn disabled_is_explicit_and_survives_legacy_normalization() {
        let mut dev = device();
        materialize_unmentioned_features_as_disabled(&mut dev).unwrap();
        dev.normalize_feature_config();
        assert!(dev.features["video"].disabled);
        assert!(dev.feature_backend("video").is_none());
        assert!(matches!(
            device_configuration_dto(&dev).unwrap().assignments["video"],
            FeatureAssignmentDto::Disabled
        ));
    }
}
