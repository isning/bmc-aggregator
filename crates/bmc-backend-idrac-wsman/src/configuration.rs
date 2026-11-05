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

pub fn configuration_descriptors() -> Vec<BackendConfigDescriptor> {
    let generation = field(
        "idrac_generation",
        true,
        Some(ConfigValue::Integer(8)),
        ConfigFieldKind::Selection {
            options: ConfigOptionSource::Static {
                options: [6, 8]
                    .into_iter()
                    .map(|value| ConfigOption {
                        value: ConfigValue::Integer(value),
                        label_key: format!("configuration.options.idrac_generation.{value}"),
                    })
                    .collect(),
            },
            allow_custom: false,
        },
    );
    vec![BackendConfigDescriptor {
        feature_id: "boot".to_string(),
        backend_id: "idrac-wsman".to_string(),
        display_name: "Dell iDRAC WS-MAN".to_string(),
        availability: BackendAvailability::Available,
        fields: vec![
            field("host", true, None, ConfigFieldKind::Text { trim: true }),
            field("username", true, None, ConfigFieldKind::Text { trim: true }),
            field("password", false, None, ConfigFieldKind::Secret),
            generation,
        ],
        defaults: BTreeMap::from([("idrac_generation".to_string(), ConfigValue::Integer(8))]),
    }]
}
