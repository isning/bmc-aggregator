use std::collections::BTreeMap;

use bmc_feature_api::configuration::{
    BackendAvailability, BackendConfigDescriptor, ConfigFieldDescriptor, ConfigFieldGroup,
    ConfigFieldKind, ConfigOption, ConfigOptionSource, ConfigValue,
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

fn selection(id: &str, values: Vec<ConfigValue>, default: ConfigValue) -> ConfigFieldDescriptor {
    field(
        id,
        true,
        Some(default),
        ConfigFieldKind::Selection {
            options: ConfigOptionSource::Static {
                options: values
                    .iter()
                    .map(|value| ConfigOption {
                        label_key: format!(
                            "configuration.options.{id}.{}",
                            match value {
                                ConfigValue::Boolean(value) => value.to_string(),
                                ConfigValue::Integer(value) => value.to_string(),
                                ConfigValue::Decimal(value) => value.to_string(),
                                ConfigValue::String(value) => value.clone(),
                            }
                        ),
                        value: value.clone(),
                    })
                    .collect(),
            },
            allow_custom: false,
        },
    )
}

fn fields() -> Vec<ConfigFieldDescriptor> {
    vec![
        field("host", true, None, ConfigFieldKind::Text { trim: true }),
        field(
            "avct_port",
            true,
            Some(ConfigValue::Integer(5900)),
            ConfigFieldKind::Integer {
                min: Some(1),
                max: Some(65_535),
                step: Some(1),
            },
        ),
        field("username", true, None, ConfigFieldKind::Text { trim: true }),
        field("password", false, None, ConfigFieldKind::Secret),
        selection(
            "idrac_generation",
            vec![ConfigValue::Integer(6), ConfigValue::Integer(8)],
            ConfigValue::Integer(8),
        ),
        selection(
            "avct_tls_mode",
            vec![ConfigValue::String("idrac-legacy-rsa".to_string())],
            ConfigValue::String("idrac-legacy-rsa".to_string()),
        ),
        field(
            "avct_virtual_media_privilege",
            false,
            Some(ConfigValue::Boolean(false)),
            ConfigFieldKind::Boolean,
        ),
        field(
            "avct_apcp_selector",
            false,
            Some(ConfigValue::Integer(5)),
            ConfigFieldKind::Integer {
                min: Some(0),
                max: Some(i64::from(u32::MAX)),
                step: Some(1),
            },
        ),
        field(
            "avct_reconnect_capability",
            false,
            Some(ConfigValue::Boolean(false)),
            ConfigFieldKind::Boolean,
        ),
        field(
            "avct_reconnect_version",
            false,
            Some(ConfigValue::Integer(-1)),
            ConfigFieldKind::Integer {
                min: Some(i64::from(i16::MIN)),
                max: Some(i64::from(i16::MAX)),
                step: Some(1),
            },
        ),
        field(
            "avct_reconnect_preference",
            false,
            Some(ConfigValue::Integer(0)),
            ConfigFieldKind::Integer {
                min: Some(0),
                max: Some(i64::from(u16::MAX)),
                step: Some(1),
            },
        ),
    ]
}

pub fn configuration_descriptors() -> Vec<BackendConfigDescriptor> {
    ["video", "hid"]
        .into_iter()
        .map(|feature_id| BackendConfigDescriptor {
            feature_id: feature_id.to_string(),
            backend_id: "idrac-avct-console".to_string(),
            display_name: "Dell iDRAC AVCT Console".to_string(),
            availability: BackendAvailability::Available,
            fields: fields(),
            defaults: BTreeMap::from([
                ("avct_port".to_string(), ConfigValue::Integer(5900)),
                ("idrac_generation".to_string(), ConfigValue::Integer(8)),
                (
                    "avct_tls_mode".to_string(),
                    ConfigValue::String("idrac-legacy-rsa".to_string()),
                ),
                (
                    "avct_virtual_media_privilege".to_string(),
                    ConfigValue::Boolean(false),
                ),
                ("avct_apcp_selector".to_string(), ConfigValue::Integer(5)),
                (
                    "avct_reconnect_capability".to_string(),
                    ConfigValue::Boolean(false),
                ),
                (
                    "avct_reconnect_version".to_string(),
                    ConfigValue::Integer(-1),
                ),
                (
                    "avct_reconnect_preference".to_string(),
                    ConfigValue::Integer(0),
                ),
            ]),
        })
        .collect()
}
