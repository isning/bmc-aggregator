use std::collections::BTreeMap;

use bmc_feature_api::configuration::{
    BackendAvailability, BackendConfigDescriptor, ConfigFieldDescriptor, ConfigFieldGroup,
    ConfigFieldKind, ConfigValue,
};

fn fields() -> Vec<ConfigFieldDescriptor> {
    vec![
        text("host", true),
        integer("port", 1, 65_535, Some(443)),
        field(
            "kvm_port",
            false,
            None,
            ConfigFieldKind::Integer {
                min: Some(1),
                max: Some(65_535),
                step: Some(1),
            },
        ),
        text("username", true),
        secret("password"),
    ]
}

fn text(id: &str, required: bool) -> ConfigFieldDescriptor {
    field(id, required, None, ConfigFieldKind::Text { trim: true })
}

fn secret(id: &str) -> ConfigFieldDescriptor {
    field(id, false, None, ConfigFieldKind::Secret)
}

fn integer(id: &str, min: i64, max: i64, default: Option<i64>) -> ConfigFieldDescriptor {
    field(
        id,
        true,
        default.map(ConfigValue::Integer),
        ConfigFieldKind::Integer {
            min: Some(min),
            max: Some(max),
            step: Some(1),
        },
    )
}

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

pub fn configuration_descriptors() -> Vec<BackendConfigDescriptor> {
    ["video", "hid", "msd", "firmware"]
        .into_iter()
        .map(|feature_id| BackendConfigDescriptor {
            feature_id: feature_id.to_string(),
            backend_id: "mahogany".to_string(),
            display_name: "Mahogany / One-KVM".to_string(),
            availability: BackendAvailability::Available,
            fields: fields(),
            defaults: BTreeMap::from([("port".to_string(), ConfigValue::Integer(443))]),
        })
        .collect()
}
