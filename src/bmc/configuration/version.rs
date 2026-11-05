use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(values) => {
            let mut entries: Vec<_> = values.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize(value)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        value => value,
    }
}

fn digest<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    let value = serde_json::to_value(value)
        .map(canonicalize)
        .map_err(|error| format!("configuration serialization failed: {error}"))?;
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| format!("configuration serialization failed: {error}"))?;
    let bytes = Sha256::digest(bytes);
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn configuration_payload_digest<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    digest(value)
}

pub fn configuration_etag<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    digest(value).map(|digest| format!("\"cfg-sha256-{digest}\""))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn etag_is_stable_across_object_and_hash_map_order() {
        let first = HashMap::from([("beta", 2), ("alpha", 1)]);
        let second = HashMap::from([("alpha", 1), ("beta", 2)]);

        assert_eq!(
            configuration_etag(&first).unwrap(),
            configuration_etag(&second).unwrap()
        );
    }

    #[test]
    fn etag_changes_with_owned_scope_content() {
        assert_ne!(
            configuration_etag(&serde_json::json!({"name": "first"})).unwrap(),
            configuration_etag(&serde_json::json!({"name": "second"})).unwrap()
        );
    }
}
