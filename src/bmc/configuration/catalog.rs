use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bmc_feature_api::configuration::{
    ConfigFieldDescriptor, ConfigFieldKind, ConfigOptionResourceDescriptor, ConfigOptionSource,
    ConfigValue, ConfigurationCatalog, DeviceCreationProfileDescriptor,
};

static CATALOG: OnceLock<Result<ConfigurationCatalog, String>> = OnceLock::new();

pub(crate) fn feature_descriptor(
    feature_id: &str,
    binding: bmc_feature_api::configuration::FeatureBindingKind,
    allow_disabled: bool,
    feature_fields: Vec<ConfigFieldDescriptor>,
) -> bmc_feature_api::configuration::FeatureConfigDescriptor {
    bmc_feature_api::configuration::FeatureConfigDescriptor {
        feature_id: feature_id.to_string(),
        title_key: format!("features.{feature_id}.title"),
        description_key: Some(format!("features.{feature_id}.description")),
        binding,
        allow_disabled,
        feature_fields,
    }
}

pub(crate) fn configuration_field(
    id: &str,
    group: bmc_feature_api::configuration::ConfigFieldGroup,
    required: bool,
    default: Option<ConfigValue>,
    field: ConfigFieldKind,
) -> ConfigFieldDescriptor {
    ConfigFieldDescriptor {
        id: id.to_string(),
        title_key: format!("configuration.fields.{id}"),
        description_key: Some(format!("configuration.fields.{id}_description")),
        group,
        required,
        default,
        field,
    }
}

pub fn configuration_catalog() -> Result<&'static ConfigurationCatalog, String> {
    CATALOG
        .get_or_init(build_catalog)
        .as_ref()
        .map_err(Clone::clone)
}

fn build_catalog() -> Result<ConfigurationCatalog, String> {
    let mut features = vec![
        crate::bmc::feature::video::configuration_descriptor(),
        crate::bmc::feature::hid::configuration_descriptor(),
        crate::bmc::feature::msd::configuration_descriptor(),
        crate::bmc::feature::power::configuration_descriptor(),
        crate::bmc::feature::boot::configuration_descriptor(),
        crate::bmc::feature::identify::configuration_descriptor(),
        crate::bmc::feature::firmware::configuration_descriptor(),
        crate::bmc::feature::health::configuration_descriptor(),
        crate::bmc::feature::audio::configuration_descriptor(),
    ];
    let mut backends = Vec::new();
    backends.extend(bmc_backend_mahogany::configuration_descriptors());
    backends.extend(bmc_backend_vnc::configuration_descriptors());
    backends.extend(bmc_backend_idrac_avct::configuration_descriptors());
    backends.extend(bmc_backend_idrac_legacy_media::configuration_descriptors());
    backends.extend(bmc_backend_idrac_wsman::configuration_descriptors());
    backends.extend(bmc_backend_ipmi::configuration_descriptors());
    backends.extend(bmc_backend_redfish::configuration_descriptors());
    backends.extend(bmc_backend_wol::configuration_descriptors());
    let option_resources = [
        "local.video_inputs",
        "local.serial_ports",
        "local.atx_gpio_chips",
        "local.atx_usb_relays",
    ]
    .into_iter()
    .map(|resource_id| ConfigOptionResourceDescriptor {
        resource_id: resource_id.to_string(),
        title_key: format!("configuration.options.resources.{resource_id}"),
    })
    .collect();
    let creation_profiles = ["mahogany", "vnc", "idrac6", "idrac8", "redfish"]
        .into_iter()
        .map(|profile_id| DeviceCreationProfileDescriptor {
            profile_id: profile_id.to_string(),
            title_key: format!("configuration.profiles.{profile_id}.title"),
            description_key: format!("configuration.profiles.{profile_id}.description"),
        })
        .collect();

    features.sort_by(|left, right| left.feature_id.cmp(&right.feature_id));
    backends.sort_by(|left, right| {
        (&left.feature_id, &left.backend_id).cmp(&(&right.feature_id, &right.backend_id))
    });
    let catalog = ConfigurationCatalog {
        version: "catalog-v1".to_string(),
        features,
        backends,
        option_resources,
        creation_profiles,
    };
    validate_catalog(&catalog)?;
    Ok(catalog)
}

fn validate_catalog(catalog: &ConfigurationCatalog) -> Result<(), String> {
    let feature_ids: HashSet<_> = catalog
        .features
        .iter()
        .map(|feature| feature.feature_id.as_str())
        .collect();
    if feature_ids.len() != catalog.features.len() {
        return Err("configuration catalog has duplicate feature IDs".to_string());
    }
    let resources: HashSet<_> = catalog
        .option_resources
        .iter()
        .map(|resource| resource.resource_id.as_str())
        .collect();
    if resources.len() != catalog.option_resources.len() {
        return Err("configuration catalog has duplicate option resource IDs".to_string());
    }

    for feature in &catalog.features {
        validate_fields(
            &format!("feature '{}'", feature.feature_id),
            &feature.feature_fields,
            &resources,
            &std::collections::BTreeMap::new(),
        )?;
    }

    let mut catalog_pairs = BTreeSet::new();
    for backend in &catalog.backends {
        if !feature_ids.contains(backend.feature_id.as_str()) {
            return Err(format!(
                "backend '{}:{}' references an unknown feature",
                backend.feature_id, backend.backend_id
            ));
        }
        if !catalog_pairs.insert((backend.feature_id.clone(), backend.backend_id.clone())) {
            return Err(format!(
                "configuration catalog has duplicate backend '{}:{}'",
                backend.feature_id, backend.backend_id
            ));
        }
        validate_fields(
            &format!("backend '{}:{}'", backend.feature_id, backend.backend_id),
            &backend.fields,
            &resources,
            &backend.defaults,
        )?;
    }

    let runtime_pairs: BTreeSet<_> = crate::bmc::feature::registered_backends()
        .iter()
        .map(|backend| (backend.feature.to_string(), backend.backend.to_string()))
        .collect();
    if catalog_pairs != runtime_pairs {
        let missing: Vec<_> = runtime_pairs.difference(&catalog_pairs).cloned().collect();
        let extra: Vec<_> = catalog_pairs.difference(&runtime_pairs).cloned().collect();
        return Err(format!(
            "configuration catalog/runtime registry mismatch; missing={missing:?}, extra={extra:?}"
        ));
    }
    Ok(())
}

fn validate_fields(
    owner: &str,
    fields: &[ConfigFieldDescriptor],
    resources: &HashSet<&str>,
    defaults: &std::collections::BTreeMap<String, ConfigValue>,
) -> Result<(), String> {
    let mut ids = HashSet::new();
    for field in fields {
        if !ids.insert(field.id.as_str()) {
            return Err(format!("{owner} has duplicate field '{}'", field.id));
        }
        if matches!(field.field, ConfigFieldKind::Secret) && field.default.is_some() {
            return Err(format!("{owner} secret '{}' has a default", field.id));
        }
        if let ConfigFieldKind::Selection {
            options: ConfigOptionSource::RegisteredResource { resource_id },
            ..
        } = &field.field
        {
            if !resources.contains(resource_id.as_str()) {
                return Err(format!(
                    "{owner} field '{}' references unknown option resource '{resource_id}'",
                    field.id
                ));
            }
        }
        if let Some(default) = &field.default {
            validate_value(field, default)?;
        }
    }
    for (id, default) in defaults {
        let field = fields
            .iter()
            .find(|field| field.id == *id)
            .ok_or_else(|| format!("{owner} has a default for unknown field '{id}'"))?;
        if matches!(field.field, ConfigFieldKind::Secret) {
            return Err(format!("{owner} secret '{id}' has a default"));
        }
        validate_value(field, default)?;
        if field.default.as_ref() != Some(default) {
            return Err(format!("{owner} defaults disagree for field '{id}'"));
        }
    }
    Ok(())
}

fn validate_value(field: &ConfigFieldDescriptor, value: &ConfigValue) -> Result<(), String> {
    let valid = match (&field.field, value) {
        (ConfigFieldKind::Text { .. }, ConfigValue::String(_))
        | (ConfigFieldKind::Secret, ConfigValue::String(_))
        | (ConfigFieldKind::Boolean, ConfigValue::Boolean(_)) => true,
        (ConfigFieldKind::Integer { min, max, .. }, ConfigValue::Integer(value)) => {
            min.is_none_or(|min| *value >= min) && max.is_none_or(|max| *value <= max)
        }
        (ConfigFieldKind::Decimal { min, max, .. }, ConfigValue::Decimal(value)) => {
            min.is_none_or(|min| *value >= min) && max.is_none_or(|max| *value <= max)
        }
        (
            ConfigFieldKind::Selection {
                options: ConfigOptionSource::Static { options },
                allow_custom,
            },
            value,
        ) => *allow_custom || options.iter().any(|option| option.value == *value),
        (
            ConfigFieldKind::Selection {
                options: ConfigOptionSource::RegisteredResource { .. },
                ..
            },
            _,
        ) => true,
        _ => false,
    };
    valid.then_some(()).ok_or_else(|| {
        format!(
            "configuration field '{}' has an invalid value {value:?}",
            field.id
        )
    })
}

pub fn validate_backend_params(
    feature_id: &str,
    backend_id: &str,
    params: &HashMap<String, serde_json::Value>,
) -> Result<(), String> {
    let catalog = configuration_catalog()?;
    let descriptor = catalog
        .backends
        .iter()
        .find(|descriptor| {
            descriptor.feature_id == feature_id && descriptor.backend_id == backend_id
        })
        .ok_or_else(|| {
            format!("backend '{backend_id}' is not registered for feature '{feature_id}'")
        })?;
    for (id, value) in params {
        let field = descriptor
            .fields
            .iter()
            .find(|field| field.id == *id)
            .ok_or_else(|| {
                format!("unknown parameter '{id}' for backend '{backend_id}' and feature '{feature_id}'")
            })?;
        if value.is_null() && !field.required {
            continue;
        }
        let value: ConfigValue = serde_json::from_value(value.clone())
            .map_err(|_| format!("parameter '{id}' has the wrong primitive type"))?;
        validate_value(field, &value)?;
    }
    Ok(())
}

pub async fn configuration_catalog_response() -> Response {
    let catalog = match configuration_catalog() {
        Ok(catalog) => catalog,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    serde_json::json!({"code": "configuration_catalog_invalid", "message": error}),
                ),
            )
                .into_response();
        }
    };
    let etag = match super::configuration_etag(catalog) {
        Ok(etag) => etag,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response();
        }
    };
    let mut response = Json(catalog).into_response();
    match HeaderValue::from_str(&etag) {
        Ok(value) => {
            response.headers_mut().insert(header::ETAG, value);
            response
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("invalid catalog ETag: {error}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_valid_and_matches_every_runtime_registration() {
        let catalog = configuration_catalog().unwrap();
        assert_eq!(
            catalog.backends.len(),
            crate::bmc::feature::registered_backends().len()
        );
        assert!(catalog
            .option_resources
            .iter()
            .any(|resource| resource.resource_id == "local.video_inputs"));
    }

    #[test]
    fn backend_parameter_validation_is_descriptor_driven() {
        assert!(validate_backend_params(
            "video",
            "vnc",
            &HashMap::from([("vnc_port".to_string(), serde_json::json!(5900))])
        )
        .is_ok());
        assert!(validate_backend_params(
            "video",
            "vnc",
            &HashMap::from([("vnc_port".to_string(), serde_json::json!(0))])
        )
        .is_err());
        assert!(validate_backend_params(
            "video",
            "vnc",
            &HashMap::from([("redfish_insecure_tls".to_string(), serde_json::json!(true))])
        )
        .is_err());
    }
}
