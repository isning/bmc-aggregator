use std::collections::BTreeMap;

use bmc_feature_api::configuration::{
    BackendAvailability, BackendConfigDescriptor, ConfigFieldDescriptor, ConfigFieldGroup,
    ConfigFieldKind, ConfigValue,
};

pub fn configuration_descriptors() -> Vec<BackendConfigDescriptor> {
    vec![BackendConfigDescriptor {
        feature_id: "power".to_string(),
        backend_id: "wol".to_string(),
        display_name: "Wake-on-LAN".to_string(),
        availability: BackendAvailability::Available,
        fields: vec![ConfigFieldDescriptor {
            id: "broadcast".to_string(),
            title_key: "configuration.fields.broadcast".to_string(),
            description_key: None,
            group: ConfigFieldGroup::Primary,
            required: true,
            default: Some(ConfigValue::String("255.255.255.255".to_string())),
            field: ConfigFieldKind::Text { trim: true },
        }],
        defaults: BTreeMap::from([(
            "broadcast".to_string(),
            ConfigValue::String("255.255.255.255".to_string()),
        )]),
    }]
}
