//! Protocol-neutral contracts for describing and editing backend configuration.
//!
//! These types describe the administrative boundary only. They do not perform
//! validation, persist configuration, select a runtime backend, or expose a
//! generic feature operation seam.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigValue {
    Boolean(bool),
    Integer(i64),
    Decimal(f64),
    String(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFieldGroup {
    Primary,
    Advanced,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigOption {
    pub value: ConfigValue,
    pub label_key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ConfigOptionSource {
    Static { options: Vec<ConfigOption> },
    RegisteredResource { resource_id: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigFieldKind {
    Text {
        #[serde(default)]
        trim: bool,
    },
    Secret,
    Integer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<i64>,
    },
    Decimal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
    },
    Boolean,
    Selection {
        options: ConfigOptionSource,
        #[serde(default)]
        allow_custom: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigFieldDescriptor {
    pub id: String,
    pub title_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_key: Option<String>,
    pub group: ConfigFieldGroup,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ConfigValue>,
    #[serde(flatten)]
    pub field: ConfigFieldKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeatureBindingKind {
    Single,
    ProviderSet {
        min: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<usize>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureConfigDescriptor {
    pub feature_id: String,
    pub title_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_key: Option<String>,
    pub binding: FeatureBindingKind,
    #[serde(default)]
    pub allow_disabled: bool,
    #[serde(default)]
    pub feature_fields: Vec<ConfigFieldDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BackendAvailability {
    Available,
    Unavailable { reason_key: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackendConfigDescriptor {
    pub feature_id: String,
    pub backend_id: String,
    pub display_name: String,
    pub availability: BackendAvailability,
    #[serde(default)]
    pub fields: Vec<ConfigFieldDescriptor>,
    #[serde(default)]
    pub defaults: BTreeMap<String, ConfigValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfigOptionResourceDescriptor {
    pub resource_id: String,
    pub title_key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredConfigOption {
    pub value: ConfigValue,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ConfigOptionResourceState {
    Ready,
    Failed { code: String, message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigOptionResourceSnapshot {
    pub resource_id: String,
    pub epoch: String,
    pub revision: u64,
    pub state: ConfigOptionResourceState,
    #[serde(default)]
    pub options: Vec<DiscoveredConfigOption>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceCreationProfileDescriptor {
    pub profile_id: String,
    pub title_key: String,
    pub description_key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigurationCatalog {
    pub version: String,
    #[serde(default)]
    pub features: Vec<FeatureConfigDescriptor>,
    #[serde(default)]
    pub backends: Vec<BackendConfigDescriptor>,
    #[serde(default)]
    pub option_resources: Vec<ConfigOptionResourceDescriptor>,
    #[serde(default)]
    pub creation_profiles: Vec<DeviceCreationProfileDescriptor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SecretState {
    pub configured: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum SecretMutation {
    Keep,
    Replace { value: String },
    Clear,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackendAssignmentDto {
    pub backend_id: String,
    #[serde(default)]
    pub params: BTreeMap<String, ConfigValue>,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackendAssignmentMutation {
    pub backend_id: String,
    #[serde(default)]
    pub params: BTreeMap<String, ConfigValue>,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretMutation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderAssignmentDto {
    pub id: String,
    #[serde(flatten)]
    pub backend: BackendAssignmentDto,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderAssignmentMutation {
    pub id: String,
    #[serde(flatten)]
    pub backend: BackendAssignmentMutation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeatureAssignmentDto {
    Disabled,
    Single {
        backend: BackendAssignmentDto,
    },
    ProviderSet {
        providers: Vec<ProviderAssignmentDto>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeatureAssignmentMutation {
    Disabled,
    Single {
        backend: BackendAssignmentMutation,
    },
    ProviderSet {
        providers: Vec<ProviderAssignmentMutation>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap()
    }

    #[test]
    fn catalog_serialization_has_a_bounded_field_vocabulary() {
        let catalog = ConfigurationCatalog {
            version: "catalog-v1".to_string(),
            features: vec![FeatureConfigDescriptor {
                feature_id: "video".to_string(),
                title_key: "features.video".to_string(),
                description_key: None,
                binding: FeatureBindingKind::Single,
                allow_disabled: false,
                feature_fields: Vec::new(),
            }],
            backends: vec![BackendConfigDescriptor {
                feature_id: "video".to_string(),
                backend_id: "vnc".to_string(),
                display_name: "VNC".to_string(),
                availability: BackendAvailability::Available,
                fields: vec![ConfigFieldDescriptor {
                    id: "port".to_string(),
                    title_key: "configuration.port".to_string(),
                    description_key: None,
                    group: ConfigFieldGroup::Primary,
                    required: true,
                    default: Some(ConfigValue::Integer(5900)),
                    field: ConfigFieldKind::Integer {
                        min: Some(1),
                        max: Some(65535),
                        step: Some(1),
                    },
                }],
                defaults: BTreeMap::from([("port".to_string(), ConfigValue::Integer(5900))]),
            }],
            option_resources: Vec::new(),
            creation_profiles: Vec::new(),
        };

        let json = serde_json::to_value(&catalog).unwrap();
        assert_eq!(json["features"][0]["binding"]["kind"], "single");
        assert_eq!(json["backends"][0]["fields"][0]["kind"], "integer");
        assert_eq!(json["backends"][0]["fields"][0]["default"], 5900);
        assert_eq!(round_trip(&catalog), catalog);
    }

    #[test]
    fn feature_assignments_round_trip_without_ambiguous_shapes() {
        let disabled = FeatureAssignmentDto::Disabled;
        let single = FeatureAssignmentDto::Single {
            backend: BackendAssignmentDto {
                backend_id: "vnc".to_string(),
                params: BTreeMap::from([("port".to_string(), ConfigValue::Integer(5900))]),
                secrets: BTreeMap::from([(
                    "password".to_string(),
                    SecretState { configured: true },
                )]),
            },
        };
        let providers = FeatureAssignmentDto::ProviderSet {
            providers: vec![ProviderAssignmentDto {
                id: "primary".to_string(),
                backend: BackendAssignmentDto {
                    backend_id: "ipmi".to_string(),
                    params: BTreeMap::new(),
                    secrets: BTreeMap::new(),
                },
            }],
        };

        assert_eq!(serde_json::to_value(&disabled).unwrap()["kind"], "disabled");
        assert_eq!(serde_json::to_value(&single).unwrap()["kind"], "single");
        assert_eq!(
            serde_json::to_value(&providers).unwrap()["kind"],
            "provider_set"
        );
        assert_eq!(round_trip(&disabled), disabled);
        assert_eq!(round_trip(&single), single);
        assert_eq!(round_trip(&providers), providers);
    }

    #[test]
    fn secret_reads_never_have_a_value_and_mutations_are_explicit() {
        assert_eq!(
            serde_json::to_value(SecretState { configured: true }).unwrap(),
            serde_json::json!({"configured": true})
        );
        assert_eq!(
            serde_json::to_value(SecretMutation::Keep).unwrap(),
            serde_json::json!({"operation": "keep"})
        );
        assert_eq!(
            serde_json::to_value(SecretMutation::Replace {
                value: "write-only".to_string()
            })
            .unwrap(),
            serde_json::json!({"operation": "replace", "value": "write-only"})
        );
        assert_eq!(
            serde_json::to_value(SecretMutation::Clear).unwrap(),
            serde_json::json!({"operation": "clear"})
        );
    }
}
