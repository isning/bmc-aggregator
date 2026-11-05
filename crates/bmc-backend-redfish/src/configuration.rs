use std::collections::BTreeMap;

use bmc_feature_api::configuration::{
    BackendAvailability, BackendConfigDescriptor, ConfigFieldDescriptor, ConfigFieldGroup,
    ConfigFieldKind, ConfigValue,
};

fn field(
    id: &str,
    required: bool,
    default: Option<ConfigValue>,
    field: ConfigFieldKind,
) -> ConfigFieldDescriptor {
    ConfigFieldDescriptor {
        id: id.to_string(),
        title_key: format!("configuration.fields.{id}"),
        description_key: None,
        group: ConfigFieldGroup::Primary,
        required,
        default,
        field,
    }
}

fn fields() -> Vec<ConfigFieldDescriptor> {
    vec![
        field("host", true, None, ConfigFieldKind::Text { trim: true }),
        field(
            "port",
            true,
            Some(ConfigValue::Integer(443)),
            ConfigFieldKind::Integer {
                min: Some(1),
                max: Some(65_535),
                step: Some(1),
            },
        ),
        field("username", true, None, ConfigFieldKind::Text { trim: true }),
        field("password", false, None, ConfigFieldKind::Secret),
        field(
            "redfish_insecure_tls",
            false,
            Some(ConfigValue::Boolean(false)),
            ConfigFieldKind::Boolean,
        ),
    ]
}

pub fn configuration_descriptors() -> Vec<BackendConfigDescriptor> {
    ["power", "boot", "identify", "firmware", "health", "msd"]
        .into_iter()
        .map(|feature_id| BackendConfigDescriptor {
            feature_id: feature_id.to_string(),
            backend_id: "redfish".to_string(),
            display_name: "Redfish".to_string(),
            availability: BackendAvailability::Available,
            fields: fields(),
            defaults: BTreeMap::from([
                ("port".to_string(), ConfigValue::Integer(443)),
                (
                    "redfish_insecure_tls".to_string(),
                    ConfigValue::Boolean(false),
                ),
            ]),
        })
        .collect()
}
