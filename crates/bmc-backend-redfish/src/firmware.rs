use crate::{RedfishClient, RedfishConfig};
use async_trait::async_trait;
use bmc_feature_api::firmware::{FirmwareBackend, FirmwareRecord};
use serde_json::Value;

pub struct RedfishFirmwareBackend {
    client: RedfishClient,
}
impl RedfishFirmwareBackend {
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

#[async_trait]
impl FirmwareBackend for RedfishFirmwareBackend {
    async fn inventory(&self) -> Result<Vec<FirmwareRecord>, String> {
        let root = self.client.get("/redfish/v1/").await?;
        let update_service_uri = root
            .pointer("/UpdateService/@odata.id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish ServiceRoot does not advertise UpdateService".to_string())?;
        let update_service = self.client.get(update_service_uri).await?;
        let uri = update_service
            .pointer("/FirmwareInventory/@odata.id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "Redfish UpdateService does not advertise FirmwareInventory".to_string()
            })?;
        let mut records = Vec::new();
        for member in self.client.collection_members(uri).await? {
            let Some(id) = member.get("@odata.id").and_then(Value::as_str) else {
                continue;
            };
            let item = self.client.get(id).await?;
            records.push(FirmwareRecord {
                id: id.to_string(),
                name: item.get("Name").and_then(Value::as_str).map(str::to_owned),
                version: item
                    .get("Version")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                updateable: item.get("Updateable").and_then(Value::as_bool),
                status: item
                    .pointer("/Status/Health")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Json, Router};
    use reqwest::Url;

    #[tokio::test]
    async fn missing_member_links_cannot_become_a_successful_empty_inventory() {
        let app = Router::new()
            .route(
                "/redfish/v1/",
                get(|| async {
                    Json(serde_json::json!({"UpdateService":{"@odata.id":"/update"}}))
                }),
            )
            .route(
                "/update",
                get(|| async {
                    Json(serde_json::json!({"FirmwareInventory":{"@odata.id":"/inventory"}}))
                }),
            )
            .route(
                "/inventory",
                get(|| async { Json(serde_json::json!({"Members":[{"Id":"unreadable"}]})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let backend = RedfishFirmwareBackend::for_test(RedfishClient::for_test(
            Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
        ));
        let result = backend.inventory().await;
        server.abort();
        assert!(
            result.is_err(),
            "malformed inventory was reported as {result:?}"
        );
    }

    #[tokio::test]
    async fn malformed_member_resource_rejects_the_entire_inventory() {
        for body in [Value::Null, serde_json::json!([]), serde_json::json!(false)] {
            let response = body.clone();
            let app = Router::new()
                .route("/redfish/v1/", get(|| async { Json(serde_json::json!({"UpdateService":{"@odata.id":"/update"}})) }))
                .route("/update", get(|| async { Json(serde_json::json!({"FirmwareInventory":{"@odata.id":"/inventory"}})) }))
                .route("/inventory", get(|| async { Json(serde_json::json!({"Members":[{"@odata.id":"/valid"},{"@odata.id":"/invalid"}]})) }))
                .route("/valid", get(|| async { Json(serde_json::json!({"Name":"BMC","Version":"1"})) }))
                .route("/invalid", get(move || { let response = response.clone(); async move { Json(response) } }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            let backend = RedfishFirmwareBackend::for_test(RedfishClient::for_test(
                Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
            ));
            let result = backend.inventory().await;
            server.abort();
            assert!(
                result.is_err(),
                "malformed firmware resource was accepted: {body}"
            );
        }
    }

    #[tokio::test]
    async fn follows_update_service_and_inventory_links() {
        let app = Router::new()
            .route("/redfish/v1/", get(|| async { Json(serde_json::json!({"UpdateService":{"@odata.id":"/redfish/v1/UpdateService"}})) }))
            .route("/redfish/v1/UpdateService", get(|| async { Json(serde_json::json!({"FirmwareInventory":{"@odata.id":"/redfish/v1/UpdateService/FirmwareInventory"}})) }))
            .route("/redfish/v1/UpdateService/FirmwareInventory", get(|| async { Json(serde_json::json!({"Members":[{"@odata.id":"/redfish/v1/UpdateService/FirmwareInventory/BMC"}]})) }))
            .route("/redfish/v1/UpdateService/FirmwareInventory/BMC", get(|| async { Json(serde_json::json!({"Name":"iDRAC","Version":"2.83","Updateable":true,"Status":{"Health":"OK"}})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let backend = RedfishFirmwareBackend::for_test(RedfishClient::for_test(
            Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
        ));
        let records = backend.inventory().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].version.as_deref(), Some("2.83"));
        assert_eq!(records[0].updateable, Some(true));
        server.abort();
    }
}
