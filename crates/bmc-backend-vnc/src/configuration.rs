use std::collections::BTreeMap;

use bmc_feature_api::configuration::{
    BackendAvailability, BackendConfigDescriptor, ConfigFieldDescriptor, ConfigFieldGroup,
    ConfigFieldKind, ConfigOption, ConfigOptionSource, ConfigValue,
};

fn field(
    id: &str,
    required: bool,
    default: Option<ConfigValue>,
    kind: ConfigFieldKind,
) -> ConfigFieldDescriptor {
    ConfigFieldDescriptor {
        id: id.to_string(),
        title_key: format!("configuration.fields.{id}"),
        description_key: None,
        group: ConfigFieldGroup::Primary,
        required,
        default,
        field: kind,
    }
}

fn fields() -> Vec<ConfigFieldDescriptor> {
    vec![
        field("host", true, None, ConfigFieldKind::Text { trim: true }),
        field(
            "vnc_port",
            true,
            Some(ConfigValue::Integer(5900)),
            ConfigFieldKind::Integer {
                min: Some(1),
                max: Some(65_535),
                step: Some(1),
            },
        ),
        field(
            "vnc_security",
            true,
            Some(ConfigValue::String("vnc-auth".to_string())),
            ConfigFieldKind::Selection {
                options: ConfigOptionSource::Static {
                    options: ["vnc-auth", "none"]
                        .into_iter()
                        .map(|value| ConfigOption {
                            value: ConfigValue::String(value.to_string()),
                            label_key: format!("configuration.options.vnc_security.{value}"),
                        })
                        .collect(),
                },
                allow_custom: false,
            },
        ),
        field(
            "vnc_tls",
            false,
            Some(ConfigValue::Boolean(false)),
            ConfigFieldKind::Selection {
                options: ConfigOptionSource::Static {
                    options: vec![ConfigOption {
                        value: ConfigValue::Boolean(false),
                        label_key: "configuration.options.vnc_tls.disabled".to_string(),
                    }],
                },
                allow_custom: false,
            },
        ),
        field("vnc_password", false, None, ConfigFieldKind::Secret),
    ]
}

pub fn configuration_descriptors() -> Vec<BackendConfigDescriptor> {
    ["video", "hid"]
        .into_iter()
        .map(|feature_id| BackendConfigDescriptor {
            feature_id: feature_id.to_string(),
            backend_id: "vnc".to_string(),
            display_name: "VNC".to_string(),
            availability: BackendAvailability::Available,
            fields: fields(),
            defaults: BTreeMap::from([
                ("vnc_port".to_string(), ConfigValue::Integer(5900)),
                (
                    "vnc_security".to_string(),
                    ConfigValue::String("vnc-auth".to_string()),
                ),
                ("vnc_tls".to_string(), ConfigValue::Boolean(false)),
            ]),
        })
        .collect()
}
