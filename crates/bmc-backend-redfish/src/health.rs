use crate::{RedfishClient, RedfishConfig};
use async_trait::async_trait;
use bmc_feature_api::health::{HealthBackend, HealthSnapshot, SensorRecord};
use serde_json::Value;

pub struct RedfishHealthBackend {
    client: RedfishClient,
}
impl RedfishHealthBackend {
    pub fn new(config: RedfishConfig) -> Result<Self, String> {
        Ok(Self {
            client: RedfishClient::new(config)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(client: RedfishClient) -> Self {
        Self { client }
    }
}

fn first_number(item: &Value, fields: &[(&str, &str)]) -> (Option<f64>, Option<String>) {
    fields
        .iter()
        .find_map(|(field, unit)| {
            item.get(*field)
                .and_then(Value::as_f64)
                .map(|value| (Some(value), Some((*unit).to_string())))
        })
        .unwrap_or((None, None))
}

fn sensor_record(item: &Value, fallback: &str, category: &str) -> Result<SensorRecord, String> {
    if !item.is_object() {
        return Err("Redfish sensor entry must be an object".into());
    }
    let (reading, inferred_unit) = first_number(
        item,
        &[
            ("Reading", ""),
            ("ReadingCelsius", "°C"),
            ("ReadingFahrenheit", "°F"),
            ("ReadingRPM", "RPM"),
            ("ReadingVolts", "V"),
            ("PowerInputWatts", "W"),
            ("PowerOutputWatts", "W"),
            ("LineInputVoltage", "V"),
        ],
    );
    Ok(SensorRecord {
        id: item
            .get("MemberId")
            .or_else(|| item.get("Id"))
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_string(),
        name: item.get("Name").and_then(Value::as_str).map(str::to_owned),
        category: Some(category.to_string()),
        reading,
        unit: item
            .get("ReadingUnits")
            .and_then(Value::as_str)
            .filter(|unit| !unit.is_empty())
            .map(str::to_owned)
            .or_else(|| inferred_unit.filter(|unit| !unit.is_empty())),
        status: item
            .pointer("/Status/Health")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

#[async_trait]
impl HealthBackend for RedfishHealthBackend {
    async fn snapshot(&self) -> Result<HealthSnapshot, String> {
        let root = self.client.get("/redfish/v1/").await?;
        let cu = root
            .pointer("/Chassis/@odata.id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish ServiceRoot does not advertise Chassis".to_string())?;
        let chassis_members = self.client.collection_members(cu).await?;
        if chassis_members.len() != 1 {
            return Err(format!(
                "Redfish Chassis collection contains {} members; a unique chassis is required",
                chassis_members.len()
            ));
        }
        let chassis = chassis_members
            .first()
            .and_then(|m| m.get("@odata.id"))
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish Chassis collection has no members".to_string())?;
        let c = self.client.get(chassis).await?;
        let overall_status = c
            .pointer("/Status/Health")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut sensors = Vec::new();
        for (link, category) in [
            ("Thermal", "thermal"),
            ("Power", "power"),
            ("Sensors", "sensor"),
        ] {
            let Some(resource) = c.get(link) else {
                continue;
            };
            let uri = resource
                .get("@odata.id")
                .and_then(Value::as_str)
                .filter(|uri| !uri.trim().is_empty())
                .ok_or_else(|| format!("Redfish {link} has an invalid resource link"))?;
            let obj = self.client.get(uri).await?;
            for key in [
                "Temperatures",
                "Fans",
                "Voltages",
                "PowerSupplies",
                "Sensors",
            ] {
                if let Some(value) = obj.get(key) {
                    let entries = value
                        .as_array()
                        .ok_or_else(|| format!("Redfish {link}.{key} must be an array"))?;
                    for item in entries {
                        sensors.push(sensor_record(item, key, category)?);
                    }
                }
            }
            if link == "Sensors" {
                for member in self.client.collection_members(uri).await? {
                    let Some(member_uri) = member.get("@odata.id").and_then(Value::as_str) else {
                        continue;
                    };
                    let item = self.client.get(member_uri).await?;
                    sensors.push(sensor_record(&item, member_uri, category)?);
                }
            }
        }
        Ok(HealthSnapshot {
            overall_status,
            sensors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Json, Router};
    use reqwest::Url;

    #[tokio::test]
    async fn malformed_advertised_sensor_inventory_is_not_a_successful_snapshot() {
        let resources = [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({"Fans": {}}),
            serde_json::json!({"Fans": null}),
            serde_json::json!({"Fans": [null]}),
            serde_json::json!({"Temperatures": [{"Name": "valid"}, false]}),
        ];
        let cases = resources
            .into_iter()
            .map(|body| {
                (
                    serde_json::json!({"Thermal":{"@odata.id":"/thermal"}}),
                    body,
                )
            })
            .chain([
                (serde_json::json!(null), serde_json::json!({})),
                (serde_json::json!({"Thermal": null}), serde_json::json!({})),
                (serde_json::json!({"Thermal": {}}), serde_json::json!({})),
                (
                    serde_json::json!({"Thermal": {"@odata.id":" "}}),
                    serde_json::json!({}),
                ),
                (
                    serde_json::json!({"Sensors":{"@odata.id":"/thermal"}}),
                    serde_json::json!({"Members":[{"@odata.id":"/sensor"}]}),
                ),
            ]);
        for (chassis, body) in cases {
            let response = body.clone();
            let app = Router::new()
                .route(
                    "/redfish/v1/",
                    get(|| async { Json(serde_json::json!({"Chassis":{"@odata.id":"/chassis"}})) }),
                )
                .route(
                    "/chassis",
                    get(|| async {
                        Json(serde_json::json!({"Members":[{"@odata.id":"/chassis/1"}]}))
                    }),
                )
                .route(
                    "/chassis/1",
                    get(move || {
                        let chassis = chassis.clone();
                        async move { Json(chassis) }
                    }),
                )
                .route("/sensor", get(|| async { Json(Value::Null) }))
                .route(
                    "/thermal",
                    get(move || {
                        let response = response.clone();
                        async move { Json(response) }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            let backend = RedfishHealthBackend::for_test(RedfishClient::for_test(
                Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
            ));
            let result = backend.snapshot().await;
            server.abort();
            assert!(result.is_err(), "malformed inventory was accepted: {body}");
        }
    }

    #[tokio::test]
    async fn reads_legacy_thermal_arrays_and_sensor_collections() {
        let app = Router::new()
            .route("/redfish/v1/", get(|| async { Json(serde_json::json!({"Chassis":{"@odata.id":"/redfish/v1/Chassis"}})) }))
            .route("/redfish/v1/Chassis", get(|| async { Json(serde_json::json!({"Members":[{"@odata.id":"/redfish/v1/Chassis/1"}]})) }))
            .route("/redfish/v1/Chassis/1", get(|| async { Json(serde_json::json!({"Status":{"Health":"Warning"},"Thermal":{"@odata.id":"/thermal"},"Sensors":{"@odata.id":"/sensors"}})) }))
            .route("/thermal", get(|| async { Json(serde_json::json!({"Temperatures":[{"MemberId":"temp1","Name":"Inlet","ReadingCelsius":23.5,"Status":{"Health":"OK"}}],"Fans":[{"MemberId":"fan1","ReadingRPM":4800}]})) }))
            .route("/sensors", get(|| async { Json(serde_json::json!({"Members":[{"@odata.id":"/sensors/voltage"}]})) }))
            .route("/sensors/voltage", get(|| async { Json(serde_json::json!({"Id":"voltage","Reading":12.1,"ReadingUnits":"V"})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let backend = RedfishHealthBackend::for_test(RedfishClient::for_test(
            Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
        ));
        let snapshot = backend.snapshot().await.unwrap();
        assert_eq!(snapshot.overall_status.as_deref(), Some("Warning"));
        assert_eq!(snapshot.sensors.len(), 3);
        assert_eq!(snapshot.sensors[0].reading, Some(23.5));
        assert_eq!(snapshot.sensors[0].unit.as_deref(), Some("°C"));
        assert_eq!(snapshot.sensors[2].id, "voltage");
        server.abort();
    }
}
