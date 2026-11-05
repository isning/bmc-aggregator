use std::sync::Arc;

use async_trait::async_trait;
use bmc_feature_api::boot::{
    BootBackend, BootCapabilities, BootDiscoverySnapshot, BootDiscoveryStore, BootEntry,
    BootEntryDiscoveryState, BootMode, BootSelection, BootStatus,
};
use serde_json::Value;

use crate::{RedfishClient, RedfishConfig};

pub struct RedfishBootBackend {
    client: RedfishClient,
    discovery: Arc<dyn BootDiscoveryStore>,
}

impl RedfishBootBackend {
    pub fn new(
        config: RedfishConfig,
        discovery: Arc<dyn BootDiscoveryStore>,
    ) -> Result<Self, String> {
        Ok(Self {
            client: RedfishClient::new(config)?,
            discovery,
        })
    }

    pub async fn discover(&self) {
        let result: Result<Vec<BootEntry>, String> = async {
            let (_, system) = self.client.system().await?;
            let collection_uri = boot_options_uri(&system).ok_or_else(|| {
                "unsupported: Redfish system does not advertise BootOptions".to_string()
            })?;
            let mut entries = Vec::new();
            for member in self.client.collection_members(collection_uri).await? {
                if let Some(entry) = parse_entry(&member) {
                    entries.push(entry);
                    continue;
                }
                let uri = member
                    .get("@odata.id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Redfish boot option has no resource link".to_string())?;
                let entry = parse_entry(&self.client.get(uri).await?).ok_or_else(|| {
                    "Redfish boot option has an invalid or missing reference".to_string()
                })?;
                entries.push(entry);
            }
            Ok(entries)
        }
        .await;
        let (entries, state) = match result {
            Ok(entries) if entries.is_empty() => (entries, BootEntryDiscoveryState::InventoryEmpty),
            Ok(entries) => (entries, BootEntryDiscoveryState::Available),
            Err(error) if error.starts_with("unsupported:") => {
                (Vec::new(), BootEntryDiscoveryState::Unsupported)
            }
            Err(error)
                if error.contains("request failed") || error.contains("response read failed") =>
            {
                (Vec::new(), BootEntryDiscoveryState::TransportUnavailable)
            }
            Err(_) => (Vec::new(), BootEntryDiscoveryState::InventoryUnavailable),
        };
        self.discovery
            .set_discovery(BootDiscoverySnapshot {
                entries,
                state,
                selected_entry_id: None,
            })
            .await;
    }

    async fn snapshot(&self) -> Result<(String, Value, BootDiscoverySnapshot), String> {
        let (uri, system) = self.client.system().await?;
        Ok((uri, system, self.discovery.discovery().await))
    }
}

#[async_trait]
impl BootBackend for RedfishBootBackend {
    async fn capabilities(&self) -> BootCapabilities {
        let discovery = self.discovery.discovery().await;
        let mut entries = match self.client.system().await {
            Ok((_, system)) => category_entries(&system),
            Err(_) => Vec::new(),
        };
        entries.extend(discovery.entries);
        BootCapabilities {
            entries,
            entry_discovery: discovery.state,
        }
    }

    async fn selection(&self) -> Result<BootSelection, String> {
        Ok(self.status().await?.selection)
    }

    async fn status(&self) -> Result<BootStatus, String> {
        let (_, system, discovery) = self.snapshot().await?;
        let mut entries = category_entries(&system);
        entries.extend(discovery.entries);
        let boot = system.get("Boot").unwrap_or(&Value::Null);
        use bmc_backend_model::boot::Override;
        let observed = Override::from_wire(
            boot.get("BootSourceOverrideEnabled")
                .and_then(Value::as_str),
        );
        let mode = match observed {
            Override::Unknown => return Err("Redfish boot override state is unknown".into()),
            Override::Disabled | Override::Once => BootMode::Once,
            Override::Persistent => BootMode::Persistent,
        };
        let entry_id = if observed == Override::Disabled {
            None
        } else {
            let target = boot
                .get("BootSourceOverrideTarget")
                .and_then(Value::as_str)
                .ok_or_else(|| "Redfish boot target is unknown".to_owned())?;
            Some(if target == "UefiBootNext" {
                format!(
                    "redfish:{}",
                    boot.get("BootNext")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| "Redfish BootNext is unknown".to_owned())?
                )
            } else {
                target_entry_id(target)
                    .ok_or_else(|| "Redfish boot target is unknown".to_owned())?
            })
        };
        Ok(BootStatus {
            capabilities: BootCapabilities {
                entries,
                entry_discovery: discovery.state,
            },
            selection: BootSelection { mode, entry_id },
        })
    }

    async fn set_selection(&self, selection: BootSelection) -> Result<(), String> {
        let (system_uri, system, discovery) = self.snapshot().await?;
        let mut supported = category_entries(&system);
        supported.extend(discovery.entries);
        if !supported.iter().any(|entry| {
            entry.enabled
                && entry.selectable
                && Some(&entry.id) == selection.entry_id.as_ref()
                && entry.supported_modes.contains(&selection.mode)
        }) {
            return Err("Redfish boot selection is not advertised as supported".into());
        }
        let entry_id = selection
            .entry_id
            .ok_or_else(|| "Redfish boot selection must identify an entry".to_string())?;
        let boot = if !entry_id.starts_with("redfish:target:") {
            if selection.mode != BootMode::Once {
                return Err("Redfish BootNext supports one-time boot only".to_string());
            }
            serde_json::json!({
                "BootSourceOverrideEnabled": "Once",
                "BootSourceOverrideTarget": "UefiBootNext",
                "BootNext": entry_id.strip_prefix("redfish:").unwrap_or(&entry_id),
            })
        } else {
            let target = redfish_target_for_entry_id(&entry_id)
                .ok_or_else(|| "unknown Redfish boot entry".to_string())?;
            serde_json::json!({
                "BootSourceOverrideEnabled": if selection.mode == BootMode::Once { "Once" } else { "Continuous" },
                "BootSourceOverrideTarget": target,
            })
        };
        self.client
            .patch_verified(&system_uri, serde_json::json!({"Boot": boot}))
            .await
    }
}

fn category_entries(system: &Value) -> Vec<BootEntry> {
    let boot = system.get("Boot").unwrap_or(&Value::Null);
    let targets = boot
        .get("BootSourceOverrideTarget@Redfish.AllowableValues")
        .and_then(Value::as_array);
    let modes = boot
        .get("BootSourceOverrideEnabled@Redfish.AllowableValues")
        .and_then(Value::as_array);
    STANDARD_TARGETS
        .iter()
        .filter(|(target, _, _)| {
            targets.is_some_and(|values| values.iter().any(|v| v.as_str() == Some(target)))
        })
        .map(|(_, slug, label)| BootEntry {
            id: format!("redfish:target:{slug}"),
            label: (*label).to_string(),
            group: Some("Boot targets".to_string()),
            badges: Vec::new(),
            enabled: true,
            selectable: true,
            supported_modes: {
                let mut supported = Vec::new();
                if modes.is_some_and(|values| values.iter().any(|v| v.as_str() == Some("Once"))) {
                    supported.push(BootMode::Once);
                }
                if modes
                    .is_some_and(|values| values.iter().any(|v| v.as_str() == Some("Continuous")))
                {
                    supported.push(BootMode::Persistent);
                }
                supported
            },
        })
        .collect()
}

const STANDARD_TARGETS: &[(&str, &str, &str)] = &[
    ("None", "default", "Default"),
    ("Pxe", "pxe", "Network (PXE)"),
    ("Hdd", "disk", "Local disk"),
    ("Cd", "optical", "Optical drive"),
    ("BiosSetup", "bios_setup", "BIOS setup"),
    ("Usb", "remote_removable", "Remote removable"),
    ("Floppy", "floppy", "Floppy"),
    ("Diags", "diagnostic", "Diagnostic partition"),
    ("RemoteDrive", "remote_disk", "Remote disk"),
];

fn target_entry_id(value: &str) -> Option<String> {
    STANDARD_TARGETS
        .iter()
        .find(|(target, _, _)| *target == value)
        .map(|(_, slug, _)| format!("redfish:target:{slug}"))
}

fn redfish_target_for_entry_id(entry_id: &str) -> Option<&'static str> {
    STANDARD_TARGETS
        .iter()
        .find(|(_, slug, _)| entry_id == format!("redfish:target:{slug}"))
        .map(|(target, _, _)| *target)
}

fn boot_options_uri(system: &Value) -> Option<&str> {
    [
        "/BootOptions/@odata.id",
        "/Boot/BootOptions/@odata.id",
        "/Links/BootOptions/@odata.id",
    ]
    .into_iter()
    .find_map(|pointer| system.pointer(pointer).and_then(Value::as_str))
}

fn parse_entry(value: &Value) -> Option<BootEntry> {
    let reference = value
        .get("BootOptionReference")?
        .as_str()
        .filter(|reference| !reference.trim().is_empty())?;
    Some(BootEntry {
        id: format!("redfish:{reference}"),
        label: value
            .get("DisplayName")
            .or_else(|| value.get("Name"))
            .and_then(Value::as_str)
            .unwrap_or(reference)
            .to_string(),
        group: Some("Specific boot entries".to_string()),
        badges: Vec::new(),
        enabled: value
            .get("BootOptionEnabled")
            // Keep the legacy alias only when the standard property is absent.
            // Null, malformed or false standard values must not be overridden.
            .or_else(|| value.get("Enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        selectable: true,
        supported_modes: vec![BootMode::Once],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Discovery(tokio::sync::Mutex<BootDiscoverySnapshot>);
    #[async_trait]
    impl BootDiscoveryStore for Discovery {
        async fn discovery(&self) -> BootDiscoverySnapshot {
            self.0.lock().await.clone()
        }
        async fn set_discovery(&self, snapshot: BootDiscoverySnapshot) {
            *self.0.lock().await = snapshot;
        }
    }
    #[tokio::test]
    async fn malformed_boot_entries_never_become_empty_or_partial_discovery_success() {
        use axum::{routing::get, Json, Router};
        for reference in [
            None,
            Some(Value::Null),
            Some(serde_json::json!(7)),
            Some(serde_json::json!("")),
            Some(serde_json::json!("  ")),
        ] {
            for include_valid in [false, true] {
                let reference = reference.clone();
                let invalid = reference.is_some();
                let app = Router::new()
                    .route("/redfish/v1/", get(|| async { Json(serde_json::json!({"Systems":{"@odata.id":"/systems"}})) }))
                    .route("/systems", get(|| async { Json(serde_json::json!({"Members":[{"@odata.id":"/system"}]})) }))
                    .route("/system", get(|| async { Json(serde_json::json!({"BootOptions":{"@odata.id":"/options"}})) }))
                    .route("/options", get(move || async move {
                        let mut members = vec![];
                        if include_valid { members.push(serde_json::json!({"@odata.id":"/good","BootOptionReference":"Boot0001","Enabled":true})); }
                        if invalid { members.push(serde_json::json!({"@odata.id":"/bad"})); }
                        Json(serde_json::json!({"Members":members}))
                    }))
                    .route("/bad", get(move || {
                        let reference = reference.clone();
                        async move { Json(serde_json::json!({"BootOptionReference":reference,"Enabled":true})) }
                    }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    let _ = axum::serve(listener, app).await;
                });
                let discovery =
                    Arc::new(Discovery(tokio::sync::Mutex::new(BootDiscoverySnapshot {
                        entries: vec![],
                        state: BootEntryDiscoveryState::Discovering,
                        selected_entry_id: None,
                    })));
                let backend = RedfishBootBackend {
                    client: RedfishClient::for_test(
                        reqwest::Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
                    ),
                    discovery: discovery.clone(),
                };
                backend.discover().await;
                let snapshot = discovery.discovery().await;
                server.abort();
                if invalid {
                    assert_eq!(
                        snapshot.state,
                        BootEntryDiscoveryState::InventoryUnavailable
                    );
                    assert!(snapshot.entries.is_empty());
                } else {
                    assert_eq!(
                        snapshot.state,
                        if include_valid {
                            BootEntryDiscoveryState::Available
                        } else {
                            BootEntryDiscoveryState::InventoryEmpty
                        }
                    );
                    assert_eq!(snapshot.entries.len(), usize::from(include_valid));
                }
            }
        }
    }

    #[tokio::test]
    async fn missing_or_unknown_remote_override_is_not_a_successful_snapshot() {
        use axum::{routing::get, Json, Router};
        for enabled in [None, Some("VendorUnknown"), Some("Disabled"), Some("Once")] {
            let system = match enabled {
                None => serde_json::json!({}),
                Some(enabled) => {
                    serde_json::json!({"Boot": {"BootSourceOverrideEnabled": enabled, "BootSourceOverrideTarget": "Pxe", "BootNext": "stale-reference"}})
                }
            };
            let app = Router::new()
                .route(
                    "/redfish/v1/",
                    get(|| async { Json(serde_json::json!({"Systems":{"@odata.id":"/systems"}})) }),
                )
                .route(
                    "/systems",
                    get(|| async {
                        Json(serde_json::json!({"Members":[{"@odata.id":"/system"}]}))
                    }),
                )
                .route(
                    "/system",
                    get(move || {
                        let system = system.clone();
                        async move { Json(system) }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            let backend = RedfishBootBackend {
                client: RedfishClient::for_test(
                    reqwest::Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
                ),
                discovery: Arc::new(Discovery(tokio::sync::Mutex::new(BootDiscoverySnapshot {
                    entries: Vec::new(),
                    state: BootEntryDiscoveryState::Unsupported,
                    selected_entry_id: None,
                }))),
            };
            let observed = backend.status().await;
            match enabled {
                None | Some("VendorUnknown") => assert!(observed.is_err()),
                Some("Disabled") => assert!(observed.unwrap().selection.entry_id.is_none()),
                Some("Once") => assert_eq!(
                    observed.unwrap().selection.entry_id.as_deref(),
                    Some("redfish:target:pxe")
                ),
                Some(_) => unreachable!(),
            }
            assert!(backend.capabilities().await.entries.is_empty());
            assert!(backend
                .set_selection(BootSelection {
                    entry_id: Some("redfish:target:pxe".into()),
                    mode: BootMode::Once
                })
                .await
                .is_err());
            server.abort();
        }
    }
    #[test]
    fn absent_capabilities_and_unknown_entry_enabled_are_not_selectable() {
        assert!(category_entries(&serde_json::json!({"Boot": {}})).is_empty());
        assert!(
            !parse_entry(&serde_json::json!({"BootOptionReference":"Boot0007"}))
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn maps_only_standard_boot_source_values() {
        assert_eq!(
            target_entry_id("Pxe").as_deref(),
            Some("redfish:target:pxe")
        );
        assert_eq!(
            redfish_target_for_entry_id("redfish:target:disk"),
            Some("Hdd")
        );
        assert_eq!(redfish_target_for_entry_id("redfish:target:missing"), None);
    }
    #[test]
    fn parses_standard_boot_option_reference() {
        let entry = parse_entry(&serde_json::json!({"BootOptionReference":"Boot0007","DisplayName":"UEFI disk","BootOptionEnabled":true})).unwrap();
        assert_eq!(entry.id, "redfish:Boot0007");
        assert!(entry.enabled);
        assert_eq!(entry.supported_modes, vec![BootMode::Once]);
    }

    #[test]
    fn standard_boot_enabled_field_is_authoritative_and_unknown_never_enables() {
        for (fields, enabled) in [
            (serde_json::json!({"BootOptionEnabled":true}), true),
            (serde_json::json!({"BootOptionEnabled":false}), false),
            (
                serde_json::json!({"BootOptionEnabled":true,"Enabled":false}),
                true,
            ),
            (
                serde_json::json!({"BootOptionEnabled":false,"Enabled":true}),
                false,
            ),
            (
                serde_json::json!({"BootOptionEnabled":null,"Enabled":true}),
                false,
            ),
            (
                serde_json::json!({"BootOptionEnabled":"true","Enabled":true}),
                false,
            ),
            (
                serde_json::json!({"BootOptionEnabled":1,"Enabled":true}),
                false,
            ),
            (serde_json::json!({"Enabled":true}), true),
            (serde_json::json!({"Enabled":false}), false),
            (serde_json::json!({}), false),
        ] {
            let mut value = fields.clone();
            value["BootOptionReference"] = serde_json::json!("Boot0007");
            assert_eq!(parse_entry(&value).unwrap().enabled, enabled, "{fields}");
        }
    }

    #[tokio::test]
    async fn discovery_preserves_standard_enabled_state_for_inline_and_linked_options() {
        use axum::{routing::get, Json, Router};
        for inline in [false, true] {
            let app = Router::new()
                .route("/redfish/v1/", get(|| async { Json(serde_json::json!({"Systems":{"@odata.id":"/systems"}})) }))
                .route("/systems", get(|| async { Json(serde_json::json!({"Members":[{"@odata.id":"/system"}]})) }))
                .route("/system", get(|| async { Json(serde_json::json!({"BootOptions":{"@odata.id":"/options"}})) }))
                .route("/options", get(move || async move {
                    let members = if inline {
                        serde_json::json!([
                            {"@odata.id":"/on","BootOptionReference":"Boot0001","BootOptionEnabled":true},
                            {"@odata.id":"/off","BootOptionReference":"Boot0002","BootOptionEnabled":false}
                        ])
                    } else {
                        serde_json::json!([{"@odata.id":"/on"},{"@odata.id":"/off"}])
                    };
                    Json(serde_json::json!({"Members":members}))
                }))
                .route("/on", get(|| async { Json(serde_json::json!({"BootOptionReference":"Boot0001","BootOptionEnabled":true})) }))
                .route("/off", get(|| async { Json(serde_json::json!({"BootOptionReference":"Boot0002","BootOptionEnabled":false})) }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            let discovery = Arc::new(Discovery(tokio::sync::Mutex::new(BootDiscoverySnapshot {
                entries: vec![],
                state: BootEntryDiscoveryState::Discovering,
                selected_entry_id: None,
            })));
            let backend = RedfishBootBackend {
                client: RedfishClient::for_test(
                    reqwest::Url::parse(&format!("http://{address}/redfish/v1/")).unwrap(),
                ),
                discovery: discovery.clone(),
            };
            backend.discover().await;
            let snapshot = discovery.discovery().await;
            server.abort();
            assert_eq!(snapshot.state, BootEntryDiscoveryState::Available);
            assert_eq!(snapshot.entries.len(), 2);
            assert_eq!(snapshot.entries[0].id, "redfish:Boot0001");
            assert!(snapshot.entries[0].enabled, "inline={inline}");
            assert!(!snapshot.entries[1].enabled);
        }
    }

    #[test]
    fn category_capabilities_follow_allowable_values() {
        let system = serde_json::json!({"Boot": {
            "BootSourceOverrideTarget@Redfish.AllowableValues": ["Pxe", "Hdd"],
            "BootSourceOverrideEnabled@Redfish.AllowableValues": ["Once"]
        }});
        let entries = category_entries(&system);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["redfish:target:pxe", "redfish:target:disk"]
        );
        assert!(entries
            .iter()
            .all(|entry| entry.supported_modes == vec![BootMode::Once]));
    }
}
