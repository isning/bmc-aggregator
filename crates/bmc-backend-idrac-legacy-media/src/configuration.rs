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

fn selection(id: &str, options: Vec<ConfigValue>, default: ConfigValue) -> ConfigFieldDescriptor {
    field(
        id,
        true,
        Some(default),
        ConfigFieldKind::Selection {
            options: ConfigOptionSource::Static {
                options: options
                    .into_iter()
                    .map(|value| ConfigOption {
                        label_key: format!(
                            "configuration.options.{id}.{}",
                            match &value {
                                ConfigValue::Boolean(value) => value.to_string(),
                                ConfigValue::Integer(value) => value.to_string(),
                                ConfigValue::Decimal(value) => value.to_string(),
                                ConfigValue::String(value) => value.clone(),
                            }
                        ),
                        value,
                    })
                    .collect(),
            },
            allow_custom: false,
        },
    )
}

pub fn configuration_descriptors() -> Vec<BackendConfigDescriptor> {
    let fields = vec![
        field("host", true, None, ConfigFieldKind::Text { trim: true }),
        field(
            "avct_vm_port",
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
            "avmp_heartbeat_interval_s",
            false,
            None,
            ConfigFieldKind::Integer {
                min: Some(1),
                max: Some(i64::from(u32::MAX)),
                step: Some(1),
            },
        ),
        selection(
            "avmp_reconnect_method",
            vec![
                ConfigValue::Integer(0),
                ConfigValue::Integer(1),
                ConfigValue::Integer(2),
            ],
            ConfigValue::Integer(0),
        ),
    ];
    vec![BackendConfigDescriptor {
        feature_id: "msd".to_string(),
        backend_id: "idrac-legacy-media".to_string(),
        display_name: "Dell iDRAC Legacy Virtual Media".to_string(),
        availability: BackendAvailability::Available,
        fields,
        defaults: BTreeMap::from([
            ("avct_vm_port".to_string(), ConfigValue::Integer(5900)),
            ("idrac_generation".to_string(), ConfigValue::Integer(8)),
            (
                "avct_tls_mode".to_string(),
                ConfigValue::String("idrac-legacy-rsa".to_string()),
            ),
            ("avmp_reconnect_method".to_string(), ConfigValue::Integer(0)),
        ]),
    }]
}
