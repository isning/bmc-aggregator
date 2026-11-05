use async_trait::async_trait;
use bmc_feature_api::identify::{
    IdentifyBackend, IdentifyCapabilities, IdentifyMode, IdentifyState,
};
use serde_json::Value;

use crate::{RedfishClient, RedfishConfig};

pub struct RedfishIdentifyBackend {
    client: RedfishClient,
    chassis_uri: String,
    writable: bool,
}

impl RedfishIdentifyBackend {
    pub async fn new(config: RedfishConfig) -> Result<Self, String> {
        let client = RedfishClient::new(config)?;
        let root = client.get("/redfish/v1/").await?;
        let collection_uri = root
            .pointer("/Chassis/@odata.id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish ServiceRoot does not advertise Chassis".to_string())?;
        let members = client.collection_members(collection_uri).await?;
        if members.len() != 1 {
            return Err(format!(
                "Redfish Chassis collection contains {} members; a unique chassis is required",
                members.len()
            ));
        }
        let chassis_uri = members
            .first()
            .and_then(|member| member.get("@odata.id"))
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish Chassis collection has no members".to_string())?
            .to_string();
        let chassis = client.get(&chassis_uri).await?;
        let writable = chassis
            .get("IndicatorLED@Redfish.AllowableValues")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values.iter().any(|value| value.as_str() == Some("Lit"))
                    && values.iter().any(|value| value.as_str() == Some("Off"))
            });
        Ok(Self {
            client,
            chassis_uri,
            writable,
        })
    }

    fn mode(value: Option<&str>) -> Result<IdentifyMode, String> {
        match value {
            Some("Lit" | "Blinking") => Ok(IdentifyMode::Indefinite),
            Some("Off") => Ok(IdentifyMode::Off),
            Some(_) | None => Err("unknown or missing Redfish IndicatorLED state".into()),
        }
    }
}

#[async_trait]
impl IdentifyBackend for RedfishIdentifyBackend {
    async fn capabilities(&self) -> IdentifyCapabilities {
        IdentifyCapabilities {
            modes: vec![IdentifyMode::Off, IdentifyMode::Indefinite],
            max_temporary_seconds: 0,
            writable: self.writable,
        }
    }

    async fn state(&self) -> Result<IdentifyState, String> {
        let chassis = self.client.get(&self.chassis_uri).await?;
        Ok(IdentifyState {
            mode: Self::mode(chassis.get("IndicatorLED").and_then(Value::as_str))?,
        })
    }

    async fn set(&self, mode: IdentifyMode, duration_seconds: Option<u8>) -> Result<(), String> {
        if mode == IdentifyMode::Temporary || duration_seconds.is_some() {
            return Err("Redfish IndicatorLED does not support temporary duration".to_string());
        }
        let value = match mode {
            IdentifyMode::Off => "Off",
            IdentifyMode::Indefinite => "Lit",
            IdentifyMode::Temporary => unreachable!(),
        };
        self.client
            .patch_verified(
                &self.chassis_uri,
                serde_json::json!({"IndicatorLED": value}),
            )
            .await
    }
}
