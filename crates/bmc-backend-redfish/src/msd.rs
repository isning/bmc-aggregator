use async_trait::async_trait;
use bmc_feature_api::msd::{
    MsdAttachment, MsdBackend, MsdCapabilities, MsdMedia, MsdMediaKind, MsdMediaReference,
    MsdMediaSource, MsdMode, MsdSlotCapabilities, MsdStorageCapabilities,
};
use reqwest::Url;
use serde_json::Value;

use crate::{RedfishClient, RedfishConfig};

const ROOT: &str = "/redfish/v1/";

#[derive(Clone)]
struct MediaResource {
    uri: String,
    insert_target: String,
    eject_target: String,
    media_types: Vec<MsdMediaKind>,
}

pub struct RedfishMsdBackend {
    service: RedfishClient,
}

impl RedfishMsdBackend {
    pub fn new(config: RedfishConfig) -> Result<Self, String> {
        Ok(Self {
            service: RedfishClient::new(config)?,
        })
    }

    pub async fn verify_capability(&self) -> Result<(), String> {
        self.media_resources().await.map(|_| ())
    }

    async fn verify_attachment(
        &self,
        uri: &str,
        inserted: bool,
        image: Option<&str>,
    ) -> Result<(), String> {
        use bmc_backend_model::transaction::{Event, State};
        let observed = self.service.get(uri).await?;
        let matches = observed.get("Inserted").and_then(Value::as_bool) == Some(inserted)
            && image
                .is_none_or(|image| observed.get("Image").and_then(Value::as_str) == Some(image));
        let operation = State::Sending
            .step(Event::Completion(0))
            .step(Event::Observed(matches));
        if operation != State::Confirmed {
            return Err("Redfish media readback mismatch; remote result unknown".into());
        }
        Ok(())
    }

    async fn media_resources(&self) -> Result<Vec<MediaResource>, String> {
        let root = self.service.get(ROOT).await?;
        let managers = root
            .pointer("/Managers/@odata.id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish ServiceRoot does not advertise Managers".to_string())?;
        let managers = self.service.collection_members(managers).await?;
        if managers.len() != 1 {
            return Err(format!(
                "Redfish Managers collection contains {} members; a unique manager is required",
                managers.len()
            ));
        }
        let manager = managers
            .first()
            .and_then(|item| item.get("@odata.id"))
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish Managers collection has no members".to_string())?;
        let manager = self.service.get(manager).await?;
        let collection = manager
            .pointer("/VirtualMedia/@odata.id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish Manager does not advertise VirtualMedia".to_string())?;
        let mut resources = Vec::new();
        for member in self.service.collection_members(collection).await? {
            let Some(uri) = member.get("@odata.id").and_then(Value::as_str) else {
                continue;
            };
            let media = self.service.get(uri).await?;
            let Some(insert_target) = media
                .pointer("/Actions/#VirtualMedia.InsertMedia/target")
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some(eject_target) = media
                .pointer("/Actions/#VirtualMedia.EjectMedia/target")
                .and_then(Value::as_str)
            else {
                continue;
            };
            resources.push(MediaResource {
                uri: uri.to_string(),
                insert_target: insert_target.to_string(),
                eject_target: eject_target.to_string(),
                media_types: generic_media_types(media.get("MediaTypes").unwrap_or(&Value::Null)),
            });
        }
        if resources.is_empty() {
            Err(
                "no Redfish VirtualMedia resource advertises both InsertMedia and EjectMedia"
                    .to_string(),
            )
        } else {
            Ok(resources)
        }
    }

    async fn media_resource(&self, slot_id: &str) -> Result<MediaResource, String> {
        self.media_resources()
            .await?
            .into_iter()
            .find(|resource| resource.uri == slot_id)
            .ok_or_else(|| format!("unknown Redfish VirtualMedia slot '{slot_id}'"))
    }
}

fn generic_media_types(value: &Value) -> Vec<MsdMediaKind> {
    let mut result = Vec::new();
    for value in value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let mapped = match value {
            "CD" | "DVD" => Some(MsdMediaKind::Optical),
            "Floppy" => Some(MsdMediaKind::Floppy),
            "USBStick" => Some(MsdMediaKind::Flash),
            _ => None,
        };
        if let Some(mapped) = mapped {
            if !result.contains(&mapped) {
                result.push(mapped);
            }
        }
    }
    result
}

fn redact_media_url(value: &Value) -> Value {
    let Some(raw) = value.as_str() else {
        return value.clone();
    };
    let Ok(mut url) = Url::parse(raw) else {
        return Value::String("<redacted invalid URL>".to_string());
    };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Value::String("<redacted invalid URL>".to_string());
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Value::String(url.to_string())
}

#[async_trait]
impl MsdBackend for RedfishMsdBackend {
    async fn capabilities(&self) -> MsdCapabilities {
        let resources = self.media_resources().await.unwrap_or_default();
        let mut media_types = Vec::new();
        for resource in &resources {
            for kind in &resource.media_types {
                if !media_types.contains(kind) {
                    media_types.push(*kind);
                }
            }
        }
        MsdCapabilities {
            virtual_drive: false,
            slots: resources
                .into_iter()
                .map(|resource| MsdSlotCapabilities::new(resource.uri, resource.media_types))
                .collect(),
            storage_locations: vec![MsdStorageCapabilities::remote_url(media_types)],
            unavailable_storage_locations: Vec::new(),
            target_management: Default::default(),
        }
    }

    async fn status(&self) -> Result<Value, String> {
        let attachments = self.attachment_snapshot().await?.unwrap_or_default();
        Ok(
            serde_json::json!({"connected": !attachments.is_empty(), "attachments": attachments, "enabled": true}),
        )
    }

    async fn attachment_snapshot(&self) -> Result<Option<Vec<MsdAttachment>>, String> {
        let mut attachments = Vec::new();
        for media in self.media_resources().await? {
            let value = self.service.get(&media.uri).await?;
            let inserted = value
                .get("Inserted")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    "Redfish VirtualMedia has missing or unknown Inserted state".to_string()
                })?;
            if !inserted {
                continue;
            }
            let reference = value
                .get("Image")
                .map(redact_media_url)
                .and_then(|value| value.as_str().map(str::to_owned))
                .filter(|url| !url.is_empty())
                .map(|url| MsdMediaReference::Remote { url });
            attachments.push(MsdAttachment {
                slot_id: media.uri,
                mode: MsdMode::Image,
                media: reference,
                media_type: generic_media_types(value.get("MediaTypes").unwrap_or(&Value::Null))
                    .into_iter()
                    .next(),
            });
        }
        Ok(Some(attachments))
    }

    async fn mount(&self, slot_id: &str, media: MsdMedia) -> Result<(), String> {
        let resource = self.media_resource(slot_id).await?;
        if media
            .media_type
            .is_some_and(|kind| !resource.media_types.contains(&kind))
        {
            return Err(
                "the selected media type is not advertised by Redfish VirtualMedia".to_string(),
            );
        }
        let MsdMediaSource::RemoteUrl { url } = media.source else {
            return Err(
                "Redfish VirtualMedia requires an HTTPS image_url reachable by the BMC".to_string(),
            );
        };
        self.service
            .post(
                &resource.insert_target,
                serde_json::json!({"Image": url, "Inserted": true, "WriteProtected": true}),
            )
            .await?;
        self.verify_attachment(&resource.uri, true, Some(&url))
            .await
    }

    async fn unmount(&self, slot_id: &str) -> Result<(), String> {
        let media = self.media_resource(slot_id).await?;
        self.service
            .post(&media.eject_target, serde_json::json!({}))
            .await?;
        self.verify_attachment(&media.uri, false, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_media_types_map_to_generic_enums() {
        assert_eq!(
            generic_media_types(&serde_json::json!(["CD", "DVD", "Floppy", "USBStick"])),
            vec![
                MsdMediaKind::Optical,
                MsdMediaKind::Floppy,
                MsdMediaKind::Flash
            ]
        );
    }
    #[test]
    fn signed_media_url_is_redacted() {
        assert_eq!(
            redact_media_url(&Value::String(
                "https://example.test/a.iso?token=x#f".into()
            )),
            Value::String("https://example.test/a.iso".into())
        );
    }

    #[test]
    fn remote_media_status_never_exposes_url_credentials_or_opaque_payloads() {
        for raw in [
            "https://user:private@example.test/a.iso?token=private#private",
            "https://user%40domain:p%40ss@example.test/a.iso",
            "https://user@example.test/a.iso",
        ] {
            assert_eq!(
                redact_media_url(&Value::String(raw.into())),
                Value::String("https://example.test/a.iso".into())
            );
        }
        for raw in ["data:private", "file:///private.iso", "not a URL"] {
            assert_eq!(
                redact_media_url(&Value::String(raw.into())),
                Value::String("<redacted invalid URL>".into())
            );
        }
    }
}
