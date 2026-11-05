use crate::config::DeviceConfig;

#[derive(Clone, Copy)]
pub(crate) enum PatchValue {
    Text,
    NullableText,
    Boolean,
    Integer { min: u64, max: u64 },
}

/// Admit the complete flat compatibility patch before applying any fields.
pub(crate) fn validate_patch(
    patch: &serde_json::Value,
    fields: &[(&str, PatchValue)],
) -> Result<(), crate::bmc::configuration::ConfigurationHttpError> {
    use crate::bmc::configuration::ConfigurationHttpError;
    let object = patch.as_object().ok_or_else(|| {
        ConfigurationHttpError::bad_request(
            "configuration_value_invalid",
            "configuration patch must be an object",
        )
    })?;
    for (name, value) in object {
        let kind = fields
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, kind)| kind)
            .ok_or_else(|| {
                ConfigurationHttpError::bad_request(
                    "configuration_field_unsupported",
                    format!("unsupported configuration field '{name}'"),
                )
            })?;
        let valid = match kind {
            PatchValue::Text => value.is_string(),
            PatchValue::NullableText => value.is_null() || value.is_string(),
            PatchValue::Boolean => value.is_boolean(),
            PatchValue::Integer { min, max } => value
                .as_u64()
                .is_some_and(|number| (*min..=*max).contains(&number)),
        };
        if !valid {
            return Err(ConfigurationHttpError::bad_request(
                "configuration_value_invalid",
                format!("invalid configuration field '{name}'"),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_aliases(
    patch: &serde_json::Value,
    names: &[&str],
) -> Result<(), crate::bmc::configuration::ConfigurationHttpError> {
    let mut first = None;
    for name in names {
        if let Some(value) = patch.get(*name) {
            if first.is_some_and(|first| first != value) {
                return Err(
                    crate::bmc::configuration::ConfigurationHttpError::bad_request(
                        "configuration_value_invalid",
                        "configuration aliases disagree",
                    ),
                );
            }
            first = Some(value);
        }
    }
    Ok(())
}

pub fn set_device_feature_backend(
    dev: &mut DeviceConfig,
    feature: &str,
    backend: &str,
) -> Result<(), String> {
    if feature == "audio" && backend == "none" {
        let entry = dev.features.entry(feature.to_string()).or_default();
        entry.disabled = true;
        entry.backend = None;
        entry.backend_params.clear();
        entry.providers.clear();
        dev.audio_backend = None;
        return Ok(());
    }

    let catalog = crate::bmc::configuration::configuration_catalog()?;
    if !catalog
        .backends
        .iter()
        .any(|descriptor| descriptor.feature_id == feature && descriptor.backend_id == backend)
    {
        return Err(format!(
            "backend '{backend}' is not registered for feature '{feature}'"
        ));
    }

    let entry = dev.features.entry(feature.to_string()).or_default();
    if entry.backend.as_deref() != Some(backend) {
        entry.backend_params.clear();
    }
    entry.disabled = false;
    entry.backend = Some(backend.to_string());
    if feature == "audio" {
        dev.audio_backend = Some(backend.to_string());
    }
    Ok(())
}

pub fn set_device_feature_param(
    dev: &mut DeviceConfig,
    feature: &str,
    key: &str,
    value: serde_json::Value,
) {
    dev.features
        .entry(feature.to_string())
        .or_default()
        .feature_params
        .insert(key.to_string(), value);
}

pub fn optional_string(patch: &serde_json::Value, key: &str) -> Option<Option<String>> {
    let value = patch.get(key)?;
    if value.is_null() {
        Some(None)
    } else {
        value.as_str().map(|s| Some(s.to_string()))
    }
}
