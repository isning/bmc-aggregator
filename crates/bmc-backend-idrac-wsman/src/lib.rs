//! Isolated Dell WS-MAN backend for concrete firmware boot entries.

mod configuration;

pub use configuration::configuration_descriptors;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_openssl::SslStream;

use bmc_backend_support::http_digest::{build_digest_auth, find_digest_challenge};
use bmc_feature_api::boot::{
    BootBackend, BootCapabilities, BootDiscoverySnapshot, BootEntry, BootEntryDiscoveryState,
    BootMode, BootSelection, BootStateStore, BootStatus,
};
use bmc_feature_api::requirements::{
    BackendRequirement, ConfirmationLevel, Remediation, RemediationAction, RemediationResult,
    RequirementCategory, RequirementId, RequirementImpact, RequirementState, RestartRequirement,
};

const BOOT_SOURCE_URI: &str =
    "http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_BootSourceSetting";
const BOOT_CONFIG_URI: &str =
    "http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_BootConfigSetting";
const BIOS_ENUMERATION_URI: &str =
    "http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_BIOSEnumeration";
const LC_ENUMERATION_URI: &str =
    "http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_LCEnumeration";
const LC_SERVICE_URI: &str = "http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_LCService";
const MANAGEMENT_TCP_TIMEOUT: Duration = Duration::from_secs(10);
const MANAGEMENT_TLS_TIMEOUT: Duration = Duration::from_secs(45);
const MANAGEMENT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_MANAGEMENT_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const ENTRY_DISCOVERY_BUDGET: Duration = Duration::from_secs(180);

#[derive(Clone, Debug)]
struct DiscoveredEntry {
    public: BootEntry,
    source_instance_id: Option<String>,
    one_time_target_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WsmanBootConfig {
    instance_id: String,
    is_current: Option<u8>,
    is_next: Option<u8>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct WsmanOneTimeBiosState {
    mode: Option<String>,
    boot_device: Option<String>,
    hdd_device: Option<String>,
    uefi_device: Option<String>,
}

impl WsmanBootConfig {
    fn is_one_time(&self) -> bool {
        self.instance_id == "OneTime"
    }

    fn is_one_time_active(&self) -> bool {
        self.is_one_time() && matches!(self.is_next, Some(1 | 3))
    }
}

fn wsman_supports_one_time(
    configs: Option<&[WsmanBootConfig]>,
    previously_confirmed: bool,
) -> bool {
    configs
        .map(|configs| configs.iter().any(WsmanBootConfig::is_one_time))
        .unwrap_or(previously_confirmed)
}

fn apply_wsman_one_time_capability(entries: &mut [DiscoveredEntry], supported: bool) {
    for entry in entries {
        let source_family = entry
            .source_instance_id
            .as_deref()
            .and_then(|source| source.split(':').next());
        entry.public.selectable = supported
            && entry.public.enabled
            && matches!(source_family, Some("IPL" | "BCV" | "UEFI"));
    }
}

fn wsman_boot_label(label: &str) -> String {
    const SEQUENCE_SUFFIXES: [&str; 3] = ["UefiBootSeq", "HddSeq", "BootSeq"];
    let trimmed = label.trim();
    SEQUENCE_SUFFIXES
        .iter()
        .find_map(|suffix| {
            trimmed
                .strip_suffix(suffix)
                .map(str::trim_end)
                .filter(|label| !label.is_empty())
        })
        .unwrap_or(trimmed)
        .to_string()
}

fn wsman_boot_badges(instance_id: &str) -> Vec<String> {
    match instance_id.split(':').next() {
        Some("UEFI") => vec!["UEFI".to_string()],
        Some("IPL" | "BCV") => vec!["Legacy".to_string()],
        _ => Vec::new(),
    }
}

fn wsman_source_matches_device(source_instance_id: &str, device: &str) -> bool {
    source_instance_id
        .split([':', '#'])
        .any(|component| component == device)
}

fn wsman_bios_selected_entry_id(
    state: &WsmanOneTimeBiosState,
    entries: &[DiscoveredEntry],
) -> Option<String> {
    let (source_family, device) = match state.mode.as_deref()? {
        "OneTimeBootSeq" => ("IPL", state.boot_device.as_deref()),
        "OneTimeHddSeq" => ("BCV", state.hdd_device.as_deref()),
        "OneTimeUefiBootSeq" => ("UEFI", state.uefi_device.as_deref()),
        _ => return None,
    };
    let device = device?;
    let mut matches = entries.iter().filter(|entry| {
        entry.source_instance_id.as_deref().is_some_and(|source| {
            source.split(':').next() == Some(source_family)
                && wsman_source_matches_device(source, device)
        })
    });
    let selected = matches.next()?.public.id.clone();
    matches.next().is_none().then_some(selected)
}

#[derive(Clone)]
pub struct IdracWsmanBootBackend {
    host: String,
    username: String,
    password: String,
    generation: u8,
    runtime: Arc<dyn BootStateStore>,
    wsman_sources: Arc<tokio::sync::RwLock<HashMap<String, String>>>,
    wsman_reported_selection: Arc<tokio::sync::RwLock<Option<String>>>,
    wsman_one_time_active_state: Arc<tokio::sync::RwLock<Option<bool>>>,
    management_connection: Arc<tokio::sync::Mutex<Option<LegacyHttpConnection>>>,
    control_lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CsiorStatus {
    current_enabled: Option<bool>,
    pending_enabled: Option<bool>,
    lifecycle_licensed: Option<bool>,
}

fn inventory_observation(status: CsiorStatus) -> bmc_idrac_model::wsman::InventoryObservation {
    bmc_idrac_model::wsman::InventoryObservation {
        current_enabled: status.current_enabled,
        pending_enabled: status.pending_enabled,
        licensed: status.lifecycle_licensed,
    }
}
fn inventory_status(status: bmc_idrac_model::wsman::InventoryStatus) -> RequirementState {
    use bmc_idrac_model::wsman::InventoryStatus;
    match status {
        InventoryStatus::Satisfied => RequirementState::Satisfied,
        InventoryStatus::Unavailable => RequirementState::Unavailable,
        InventoryStatus::Pending => RequirementState::Pending,
        InventoryStatus::ActionRequired => RequirementState::ActionRequired,
        InventoryStatus::Unknown => RequirementState::Unknown,
    }
}
fn inventory_requirement_state(status: Option<CsiorStatus>) -> RequirementState {
    status
        .map(|status| inventory_status(inventory_observation(status).status()))
        .unwrap_or(RequirementState::Unknown)
}

#[derive(Debug)]
enum DiscoveryError {
    InventoryEmpty(String),
    InventoryUnavailable(String),
    Unavailable(String),
}

impl DiscoveryError {
    fn state(&self) -> BootEntryDiscoveryState {
        match self {
            Self::InventoryEmpty(_) => BootEntryDiscoveryState::InventoryEmpty,
            Self::InventoryUnavailable(_) => BootEntryDiscoveryState::InventoryUnavailable,
            Self::Unavailable(_) => BootEntryDiscoveryState::TransportUnavailable,
        }
    }
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InventoryEmpty(message)
            | Self::InventoryUnavailable(message)
            | Self::Unavailable(message) => formatter.write_str(message),
        }
    }
}

impl From<String> for DiscoveryError {
    fn from(message: String) -> Self {
        Self::Unavailable(message)
    }
}

struct LegacyHttpResponse {
    status: StatusCode,
    headers: hyper::HeaderMap,
    body: Bytes,
}

struct LegacyHttpConnection {
    sender: http1::SendRequest<Full<Bytes>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LegacyHttpConnection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl IdracWsmanBootBackend {
    async fn open_management_connection(&self) -> Result<LegacyHttpConnection, String> {
        let address = bmc_backend_support::endpoint::authority(&self.host, 443)?;
        let tcp = tokio::time::timeout(MANAGEMENT_TCP_TIMEOUT, TcpStream::connect(&address))
            .await
            .map_err(|_| "iDRAC management TCP connect timed out".to_string())?
            .map_err(|error| format!("iDRAC management TCP connect failed: {error}"))?;
        let mut builder = SslConnector::builder(SslMethod::tls())
            .map_err(|error| format!("iDRAC management TLS setup failed: {error}"))?;
        builder.set_verify(SslVerifyMode::NONE);
        builder
            .set_max_proto_version(Some(SslVersion::TLS1_2))
            .map_err(|error| format!("iDRAC legacy TLS version policy failed: {error}"))?;
        builder
            .set_cipher_list("DEFAULT@SECLEVEL=0")
            .map_err(|error| format!("iDRAC legacy cipher policy failed: {error}"))?;
        let ssl = builder
            .build()
            .configure()
            .and_then(|config| config.into_ssl(self.host.trim_matches(['[', ']'])))
            .map_err(|error| format!("iDRAC management TLS setup failed: {error}"))?;
        let mut tls = SslStream::new(ssl, tcp)
            .map_err(|error| format!("iDRAC management TLS stream failed: {error}"))?;
        tokio::time::timeout(
            MANAGEMENT_TLS_TIMEOUT,
            std::pin::Pin::new(&mut tls).connect(),
        )
        .await
        .map_err(|_| "iDRAC management TLS handshake timed out".to_string())?
        .map_err(|error| format!("iDRAC management TLS handshake failed: {error}"))?;
        let (sender, connection) = http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|error| format!("iDRAC HTTP handshake failed: {error}"))?;
        let task = tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::debug!(%error, "iDRAC management HTTP connection ended");
            }
        });
        Ok(LegacyHttpConnection { sender, task })
    }

    fn inventory_requirement(state: RequirementState) -> BackendRequirement {
        BackendRequirement {
            id: RequirementId::FirmwareInventoryCollection,
            category: RequirementCategory::Inventory,
            state,
            impact: RequirementImpact::Degraded,
            title_key: "requirements.firmwareInventory.title".to_string(),
            message_key: match state {
                RequirementState::Satisfied => "requirements.firmwareInventory.satisfied",
                RequirementState::ActionRequired => "requirements.firmwareInventory.actionRequired",
                RequirementState::Pending => "requirements.firmwareInventory.pendingRestart",
                RequirementState::Unavailable => "requirements.firmwareInventory.unavailable",
                RequirementState::Unknown => "requirements.firmwareInventory.unknown",
            }
            .to_string(),
            remediation: (state == RequirementState::ActionRequired).then_some(Remediation {
                action: RemediationAction::EnableInventoryCollection,
                confirmation: ConfirmationLevel::Required,
                restart_requirement: RestartRequirement::NextHostPost,
            }),
        }
    }

    async fn legacy_request(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, String)],
        body: Vec<u8>,
    ) -> Result<LegacyHttpResponse, String> {
        let mut cached_connection = self.management_connection.lock().await;
        // Taking ownership makes every error/cancellation discard this HTTP
        // stream. A partially consumed response must never be reused.
        let mut connection = cached_connection.take();
        use bmc_idrac_model::management::{Exchange, ExchangeEffect, ExchangeEvent};
        let mut model = Exchange::default();
        let result = async {
        let must_reconnect = match connection.as_mut() {
            Some(connection) => !matches!(
                tokio::time::timeout(MANAGEMENT_RESPONSE_TIMEOUT, connection.sender.ready()).await,
                Ok(Ok(_))
            ),
            None => true,
        };
        let transition = model.step(ExchangeEvent::ConnectionReady { ready: !must_reconnect });
        model = transition.state;
        if transition.effect == ExchangeEffect::Open {
            connection = Some(self.open_management_connection().await?);
            let transition = model.step(ExchangeEvent::Connected);
            model = transition.state;
            debug_assert_eq!(transition.effect, ExchangeEffect::Send);
        }
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, &self.host);
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let request = request
            .body(Full::new(Bytes::from(body)))
            .map_err(|error| format!("iDRAC HTTP request build failed: {error}"))?;
        let sender = connection
            .as_mut()
            .ok_or_else(|| "iDRAC management connection was not initialized".to_string())?;
        let response = tokio::time::timeout(
            MANAGEMENT_RESPONSE_TIMEOUT,
            sender.sender.send_request(request),
        )
        .await
        .map_err(|_| "iDRAC management HTTP response timed out".to_string())?
        .map_err(|error| format!("iDRAC management HTTP request failed: {error}"))?;
        let status = StatusCode::from_u16(response.status().as_u16())
            .map_err(|error| format!("invalid iDRAC HTTP status: {error}"))?;
        let headers = response.headers().clone();
        let close_after_response = headers
            .get(hyper::header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(',').any(|token| token.trim().eq_ignore_ascii_case("close")));
        let transition = model.step(ExchangeEvent::Headers { close: close_after_response });
        model = transition.state;
        debug_assert_eq!(transition.effect, ExchangeEffect::ReadBody);
        let body = tokio::time::timeout(MANAGEMENT_RESPONSE_TIMEOUT, async move {
            let mut incoming = response.into_body();
            let mut bytes = bytes::BytesMut::new();
            while let Some(frame) = incoming.frame().await {
                let frame = frame.map_err(|error| format!("iDRAC management response body failed: {error}"))?;
                if let Ok(data) = frame.into_data() {
                    if bytes.len().saturating_add(data.len()) > MAX_MANAGEMENT_RESPONSE_BYTES {
                        return Err(format!("iDRAC management response exceeds the {MAX_MANAGEMENT_RESPONSE_BYTES}-byte limit"));
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            Ok::<_, String>(bytes.freeze())
        }).await.map_err(|_| "iDRAC management response body timed out".to_string())??;
        let transition = model.step(ExchangeEvent::BodyComplete);
        model = transition.state;
        if transition.effect == ExchangeEffect::Reuse {
            *cached_connection = connection.take();
        }
        Ok(LegacyHttpResponse {
            status,
            headers,
            body,
        })
        }.await;
        if result.is_err() {
            let _ = model.step(ExchangeEvent::Fault);
        }
        result
    }

    fn basic_auth(&self) -> String {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.username, self.password));
        format!("Basic {encoded}")
    }

    async fn wsman_post(&self, body: String) -> Result<String, String> {
        tokio::time::timeout(ENTRY_DISCOVERY_BUDGET, async {
            use bmc_idrac_model::wsman::{AuthEffect, AuthEvent, Authentication};
            let mut state = Authentication::default();
            let mut authorization = self.basic_auth();
            loop {
                let response = self
                    .legacy_request(
                        Method::POST,
                        "/wsman",
                        &[
                            (
                                "content-type",
                                "application/soap+xml;charset=UTF-8".to_string(),
                            ),
                            ("authorization", authorization),
                        ],
                        body.clone().into_bytes(),
                    )
                    .await?;
                let transition = state.step(AuthEvent::Response(response.status.as_u16()));
                state = transition.state;
                match transition.effect {
                    AuthEffect::RetryDigest => {
                        let params = response
                            .headers
                            .get_all(hyper::header::WWW_AUTHENTICATE)
                            .iter()
                            .filter_map(|value| value.to_str().ok())
                            .find_map(find_digest_challenge)
                            .ok_or_else(|| {
                                "iDRAC WS-MAN supplied no supported Digest challenge".to_string()
                            })?;
                        authorization = build_digest_auth(
                            &self.username,
                            &self.password,
                            &params,
                            "/wsman",
                            "POST",
                        )?;
                    }
                    AuthEffect::Accept => {
                        return String::from_utf8(response.body.to_vec()).map_err(|error| {
                            format!("iDRAC WS-MAN response is not UTF-8: {error}")
                        })
                    }
                    AuthEffect::Reject | AuthEffect::IgnoreTerminal => {
                        return Err(format!("iDRAC WS-MAN returned HTTP {}", response.status))
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            "iDRAC WS-MAN request exceeded its absolute deadline; remote result may be unknown"
                .to_string()
        })?
    }

    async fn wsman_source_entries(
        &self,
        partial_supports_one_time: Option<bool>,
    ) -> Result<Vec<DiscoveredEntry>, DiscoveryError> {
        let body = wsman_enumerate_request(&self.host, BOOT_SOURCE_URI);
        let mut response = self.wsman_post(body).await?;
        let mut entries = Vec::new();
        let mut enumeration = bmc_idrac_model::wsman::Enumeration::default();
        loop {
            entries.extend(parse_wsman_boot_entries_page(&response).map_err(DiscoveryError::from)?);
            if partial_supports_one_time.is_some() && !entries.is_empty() {
                self.publish_partial_wsman_entries(
                    &entries,
                    partial_supports_one_time.unwrap_or(false),
                )
                .await;
            }
            let Some(context) =
                next_enumeration_context(&mut enumeration, &response, "enumeration")?
            else {
                break;
            };
            response = self
                .wsman_post(wsman_pull_request(&self.host, BOOT_SOURCE_URI, &context))
                .await?;
        }
        if entries.is_empty() {
            if self
                .wsman_csior_status()
                .await
                .ok()
                .flatten()
                .is_some_and(|status| {
                    status.current_enabled == Some(false) && status.pending_enabled != Some(true)
                })
            {
                return Err(DiscoveryError::InventoryUnavailable(
                    "iDRAC Lifecycle Controller inventory collection is disabled".to_string(),
                ));
            }
            let detail = xml_text(&response, "Text")
                .or_else(|| xml_text(&response, "Reason"))
                .unwrap_or_else(|| response.chars().take(160).collect());
            return Err(DiscoveryError::InventoryEmpty(format!(
                "iDRAC WS-MAN returned no boot entries: {detail}"
            )));
        }
        let reported_selection = take_wsman_reported_selection(&mut entries);
        *self.wsman_reported_selection.write().await = reported_selection;
        Ok(entries)
    }

    async fn publish_partial_wsman_entries(
        &self,
        entries: &[DiscoveredEntry],
        supports_one_time: bool,
    ) {
        let mut partial = entries.to_vec();
        apply_wsman_one_time_capability(&mut partial, supports_one_time);
        self.remember_wsman_sources(&partial).await;
        let selected_entry_id = self.entries().await.selected_entry_id;
        self.runtime
            .set_discovery(BootDiscoverySnapshot {
                entries: partial.into_iter().map(|entry| entry.public).collect(),
                state: BootEntryDiscoveryState::Discovering,
                selected_entry_id,
            })
            .await;
    }

    async fn remember_wsman_sources(&self, entries: &[DiscoveredEntry]) {
        let sources = entries
            .iter()
            .filter_map(|entry| {
                entry
                    .source_instance_id
                    .as_ref()
                    .map(|source| (entry.public.id.clone(), source.clone()))
            })
            .collect();
        *self.wsman_sources.write().await = sources;
    }

    async fn wsman_entries(&self) -> Result<Vec<DiscoveredEntry>, DiscoveryError> {
        let previously_confirmed = self
            .entries()
            .await
            .entries
            .iter()
            .any(|entry| entry.selectable);
        let (supports_one_time, one_time_active) = match self.wsman_boot_configs().await {
            Ok(configs) => {
                let active = configs.iter().any(WsmanBootConfig::is_one_time_active);
                *self.wsman_one_time_active_state.write().await = Some(active);
                (
                    wsman_supports_one_time(Some(&configs), previously_confirmed),
                    active,
                )
            }
            Err(error) => {
                tracing::debug!(%error, "iDRAC WS-MAN one-time boot capability discovery unavailable");
                *self.wsman_one_time_active_state.write().await = None;
                (wsman_supports_one_time(None, previously_confirmed), false)
            }
        };
        let mut entries = self.wsman_source_entries(Some(supports_one_time)).await?;
        if one_time_active && self.wsman_reported_selection.read().await.is_none() {
            match self.wsman_one_time_bios_state().await {
                Ok(state) => {
                    let selected = wsman_bios_selected_entry_id(&state, &entries);
                    tracing::debug!(
                        mode = ?state.mode,
                        boot_device = ?state.boot_device,
                        hdd_device = ?state.hdd_device,
                        uefi_device = ?state.uefi_device,
                        selected_entry_id = ?selected,
                        "iDRAC WS-MAN BIOS one-time selection readback"
                    );
                    *self.wsman_reported_selection.write().await = selected;
                }
                Err(error) => {
                    tracing::debug!(%error, "iDRAC WS-MAN BIOS one-time selection readback unavailable");
                }
            }
        }
        self.remember_wsman_sources(&entries).await;
        apply_wsman_one_time_capability(&mut entries, supports_one_time);
        Ok(entries)
    }

    async fn wsman_boot_configs(&self) -> Result<Vec<WsmanBootConfig>, String> {
        let mut response = self
            .wsman_post(wsman_enumerate_request(&self.host, BOOT_CONFIG_URI))
            .await?;
        let mut configs = Vec::new();
        let mut enumeration = bmc_idrac_model::wsman::Enumeration::default();
        loop {
            configs.extend(parse_wsman_boot_configs_page(&response)?);
            let Some(context) = next_enumeration_context(
                &mut enumeration,
                &response,
                "boot configuration enumeration",
            )?
            else {
                return Ok(configs);
            };
            response = self
                .wsman_post(wsman_pull_request(&self.host, BOOT_CONFIG_URI, &context))
                .await?;
        }
    }

    async fn wsman_one_time_active(&self) -> Result<bool, String> {
        Ok(self
            .wsman_boot_configs()
            .await?
            .iter()
            .any(WsmanBootConfig::is_one_time_active))
    }

    async fn wsman_one_time_bios_state(&self) -> Result<WsmanOneTimeBiosState, String> {
        let mut response = self
            .wsman_post(wsman_enumerate_request(&self.host, BIOS_ENUMERATION_URI))
            .await?;
        let mut state = WsmanOneTimeBiosState::default();
        let mut enumeration = bmc_idrac_model::wsman::Enumeration::default();
        loop {
            let page = parse_wsman_one_time_bios_state_page(&response)?;
            if page.mode.is_some() {
                state.mode = page.mode;
            }
            if page.boot_device.is_some() {
                state.boot_device = page.boot_device;
            }
            if page.hdd_device.is_some() {
                state.hdd_device = page.hdd_device;
            }
            if page.uefi_device.is_some() {
                state.uefi_device = page.uefi_device;
            }
            let Some(context) =
                next_enumeration_context(&mut enumeration, &response, "BIOS enumeration")?
            else {
                return Ok(state);
            };
            response = self
                .wsman_post(wsman_pull_request(
                    &self.host,
                    BIOS_ENUMERATION_URI,
                    &context,
                ))
                .await?;
        }
    }

    async fn wsman_csior_status(&self) -> Result<Option<CsiorStatus>, String> {
        let response = self
            .wsman_post(wsman_enumerate_request(&self.host, LC_ENUMERATION_URI))
            .await?;
        Ok(parse_wsman_csior_status(&response))
    }

    async fn required_csior_status(&self) -> Result<CsiorStatus, String> {
        self.wsman_csior_status().await?.ok_or_else(|| {
            "iDRAC Lifecycle Controller did not report the inventory collection setting".to_string()
        })
    }

    async fn publish_inventory_requirement(&self, status: CsiorStatus) -> RequirementState {
        let state = inventory_requirement_state(Some(status));
        self.runtime
            .set_requirements(vec![Self::inventory_requirement(state)])
            .await;
        state
    }

    async fn refresh_inventory_requirement(&self) -> Result<RequirementState, String> {
        match self.required_csior_status().await {
            Ok(status) => Ok(self.publish_inventory_requirement(status).await),
            Err(error) => {
                self.runtime
                    .set_requirements(vec![Self::inventory_requirement(RequirementState::Unknown)])
                    .await;
                Err(error)
            }
        }
    }

    async fn discover_entries(&self) -> Result<Vec<DiscoveredEntry>, DiscoveryError> {
        self.wsman_entries().await
    }

    async fn refresh_entries(&self) {
        // A new observation cycle cannot publish the previous boot choice
        // while fresh inventory/activation queries are still in flight.
        let mut cached = self.entries().await;
        cached.selected_entry_id = None;
        cached.state = BootEntryDiscoveryState::Discovering;
        self.runtime.set_discovery(cached).await;
        *self.wsman_reported_selection.write().await = None;
        *self.wsman_one_time_active_state.write().await = None;
        let (entries, state) =
            match tokio::time::timeout(ENTRY_DISCOVERY_BUDGET, self.discover_entries()).await {
                Ok(Ok(entries)) => (entries, BootEntryDiscoveryState::Available),
                Ok(Err(error)) => (Vec::new(), error.state()),
                Err(_) => (Vec::new(), BootEntryDiscoveryState::TransportUnavailable),
            };
        let selected_entry_id = match *self.wsman_one_time_active_state.read().await {
            Some(true) => self
                .wsman_reported_selection
                .read()
                .await
                .clone()
                .filter(|selected| entries.iter().any(|entry| entry.public.id == *selected)),
            Some(false) => None,
            None => None,
        };
        let requirement_state = match state {
            BootEntryDiscoveryState::Available => RequirementState::Satisfied,
            BootEntryDiscoveryState::InventoryUnavailable => RequirementState::ActionRequired,
            BootEntryDiscoveryState::InventoryEmpty => {
                inventory_requirement_state(self.wsman_csior_status().await.ok().flatten())
            }
            BootEntryDiscoveryState::TransportUnavailable => RequirementState::Unavailable,
            BootEntryDiscoveryState::Discovering | BootEntryDiscoveryState::Unsupported => {
                RequirementState::Unknown
            }
        };
        self.runtime
            .set_requirements(vec![Self::inventory_requirement(requirement_state)])
            .await;
        self.runtime
            .set_discovery(BootDiscoverySnapshot {
                entries: entries.into_iter().map(|entry| entry.public).collect(),
                state,
                selected_entry_id,
            })
            .await;
    }

    async fn entries(&self) -> BootDiscoverySnapshot {
        self.runtime.discovery().await
    }

    fn capabilities_from(
        entries: &[BootEntry],
        entry_discovery: BootEntryDiscoveryState,
    ) -> BootCapabilities {
        BootCapabilities {
            entries: entries.to_vec(),
            entry_discovery,
        }
    }

    async fn status_snapshot(&self) -> Result<BootStatus, String> {
        let cached = self.entries().await;
        let selection = BootSelection {
            mode: BootMode::Once,
            entry_id: cached.selected_entry_id,
        };
        Ok(BootStatus {
            capabilities: Self::capabilities_from(&cached.entries, cached.state),
            selection,
        })
    }

    async fn set_wsman_entry(&self, entry_id: &str) -> Result<(), String> {
        let cached = self.entries().await;
        if !cached
            .entries
            .iter()
            .any(|entry| entry.id == entry_id && entry.selectable && entry.enabled)
        {
            return Err("unknown or non-selectable iDRAC boot entry".to_string());
        }
        let mut source_instance_id = self.wsman_sources.read().await.get(entry_id).cloned();
        if source_instance_id.is_none() {
            let entries = self
                .wsman_source_entries(None)
                .await
                .map_err(|error| error.to_string())?;
            self.remember_wsman_sources(&entries).await;
            source_instance_id = self.wsman_sources.read().await.get(entry_id).cloned();
        }
        let source_instance_id = source_instance_id.ok_or_else(|| {
            "selected iDRAC WS-MAN boot entry is no longer present in BMC inventory".to_string()
        })?;
        use bmc_idrac_model::operation::{Operation, OperationEffect, OperationEvent};
        let origin = tokio::time::Instant::now();
        let mut model = Operation::new(Duration::ZERO, ENTRY_DISCOVERY_BUDGET);
        use bmc_idrac_model::wsman::{BootReadback, BootReadbackEffect, BootReadbackEvent};
        let mut readback = BootReadback::default();
        // Invalidate the old observation before issuing a remote mutation. If
        // this future is cancelled, no stale selection remains authoritative.
        self.runtime.set_selected_entry_id(None).await;
        *self.wsman_reported_selection.write().await = None;
        *self.wsman_one_time_active_state.write().await = None;
        let result = tokio::time::timeout(ENTRY_DISCOVERY_BUDGET, async {
            let response = self
                .wsman_post(wsman_change_one_time_boot_request(
                    &self.host,
                    &source_instance_id,
                ))
                .await?;
            let accepted = require_wsman_return_value(&response, "0", "set one-time boot entry");
            let transition = model.step(OperationEvent::WriteFinished {
                now: origin.elapsed(),
                accepted: accepted.is_ok(),
            });
            model = transition.state;
            accepted?;
            if transition.effect != OperationEffect::Observe {
                return Err(
                    "iDRAC boot operation expired before readback; remote result is unknown"
                        .to_string(),
                );
            }
            let mut event = BootReadbackEvent::Activation {
                active: self.wsman_one_time_active().await?,
            };
            loop {
                let transition = readback.step(event);
                readback = transition.state;
                event = match transition.effect {
                    BootReadbackEffect::QuerySelection => {
                        let entries = self
                            .wsman_source_entries(None)
                            .await
                            .map_err(|error| error.to_string())?;
                        let alias = self.wsman_reported_selection.read().await.clone();
                        let selected = if alias.is_some() {
                            alias
                        } else {
                            let bios = self.wsman_one_time_bios_state().await?;
                            wsman_bios_selected_entry_id(&bios, &entries)
                        };
                        BootReadbackEvent::Selection {
                            matches: selected.as_deref() == Some(entry_id)
                                && entries.iter().any(|entry| {
                                    entry.public.id == entry_id && entry.public.enabled
                                }),
                        }
                    }
                    BootReadbackEffect::QueryActivation => BootReadbackEvent::Activation {
                        active: self.wsman_one_time_active().await?,
                    },
                    BootReadbackEffect::Retry => {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        BootReadbackEvent::Activation {
                            active: self.wsman_one_time_active().await?,
                        }
                    }
                    BootReadbackEffect::Confirmed => {
                        let transition = model.step(OperationEvent::Observation {
                            now: origin.elapsed(),
                            matches: true,
                        });
                        model = transition.state;
                        if transition.effect != OperationEffect::Confirm {
                            return Err(
                                "iDRAC boot readback expired; remote result is unknown".to_string()
                            );
                        }
                        *self.wsman_one_time_active_state.write().await = Some(true);
                        *self.wsman_reported_selection.write().await = Some(entry_id.to_string());
                        self.runtime
                            .set_selected_entry_id(Some(entry_id.to_string()))
                            .await;
                        return Ok(());
                    }
                    BootReadbackEffect::ReportUnknown | BootReadbackEffect::IgnoreTerminal => {
                        return Err(
                            "iDRAC boot readback could not confirm a current selection".to_string()
                        )
                    }
                };
            }
        })
        .await;
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                let _ = readback.step(BootReadbackEvent::Unavailable);
                let _ = model.step(OperationEvent::TransportLost);
                self.runtime.set_selected_entry_id(None).await;
                *self.wsman_reported_selection.write().await = None;
                Err(format!("{error}; boot selection is unconfirmed"))
            }
            Err(_) => {
                let _ = readback.step(BootReadbackEvent::Unavailable);
                let _ = model.step(OperationEvent::Tick {
                    now: origin.elapsed(),
                });
                self.runtime.set_selected_entry_id(None).await;
                *self.wsman_reported_selection.write().await = None;
                Err("iDRAC boot selection deadline exceeded; remote result is unknown".to_string())
            }
        }
    }

    async fn set_concrete_entry(&self, entry_id: &str) -> Result<(), String> {
        if entry_id.starts_with("wsman:") {
            return self.set_wsman_entry(entry_id).await;
        }
        Err("unknown iDRAC boot-entry identifier family".to_string())
    }

    async fn enable_inventory_collection(&self) -> Result<RemediationResult, String> {
        use bmc_idrac_model::wsman::{InventoryEffect, InventoryEvent, InventoryRemediation};
        let requests = wsman_lc_remediation_requests(&self.host, self.generation);
        let mut model = InventoryRemediation::new(requests.commit.is_some());
        let result = tokio::time::timeout(ENTRY_DISCOVERY_BUDGET, async {
            let status = self.required_csior_status().await?;
            let mut event = InventoryEvent::Observed(inventory_observation(status));
            loop {
                let transition = model.step(event);
                model = transition.state;
                event = match transition.effect {
                    InventoryEffect::Set => {
                        // Invalidate cached certainty before the first write.
                        self.runtime
                            .set_requirements(vec![Self::inventory_requirement(
                                RequirementState::Unknown,
                            )])
                            .await;
                        let response = self.wsman_post(requests.set.clone()).await?;
                        let accepted = require_wsman_return_value(
                            &response,
                            "0",
                            "enable inventory collection",
                        )
                        .is_ok();
                        InventoryEvent::SetFinished { accepted }
                    }
                    InventoryEffect::Commit => {
                        let request = requests.commit.as_ref().ok_or_else(|| {
                            "inventory model requested an unavailable commit".to_string()
                        })?;
                        let response = self.wsman_post(request.clone()).await?;
                        InventoryEvent::CommitFinished {
                            accepted: require_wsman_return_value(
                                &response,
                                "4096",
                                "commit inventory collection",
                            )
                            .is_ok(),
                        }
                    }
                    InventoryEffect::ReadBack => InventoryEvent::Observed(inventory_observation(
                        self.required_csior_status().await?,
                    )),
                    InventoryEffect::NoChange(status) | InventoryEffect::Complete(status) => {
                        let state = inventory_status(status);
                        self.runtime
                            .set_requirements(vec![Self::inventory_requirement(state)])
                            .await;
                        return Ok(RemediationResult {
                            changed: matches!(transition.effect, InventoryEffect::Complete(_)),
                            state,
                            restart_requirement: if state == RequirementState::Pending {
                                RestartRequirement::NextHostPost
                            } else {
                                RestartRequirement::None
                            },
                        });
                    }
                    InventoryEffect::ReportUnknown => {
                        return Err(
                            "iDRAC inventory state is unknown; setting was not confirmed"
                                .to_string(),
                        )
                    }
                    InventoryEffect::Reject | InventoryEffect::IgnoreTerminal => return Err(
                        "iDRAC rejected the inventory operation or returned an out-of-order result"
                            .to_string(),
                    ),
                };
            }
        })
        .await;
        match result {
            Ok(Ok(result)) => Ok(result),
            failure => {
                let _ = model.step(InventoryEvent::TransportLost);
                self.runtime
                    .set_requirements(vec![Self::inventory_requirement(RequirementState::Unknown)])
                    .await;
                Err(match failure {
                    Ok(Err(error)) => error,
                    Err(_) => "iDRAC inventory remediation timed out; remote result is unknown"
                        .to_string(),
                    Ok(Ok(_)) => unreachable!("successful result returned above"),
                })
            }
        }
    }
}

#[async_trait]
impl BootBackend for IdracWsmanBootBackend {
    async fn capabilities(&self) -> BootCapabilities {
        let cached = self.entries().await;
        Self::capabilities_from(&cached.entries, cached.state)
    }

    async fn selection(&self) -> Result<BootSelection, String> {
        Ok(self.status_snapshot().await?.selection)
    }

    async fn status(&self) -> Result<BootStatus, String> {
        self.status_snapshot().await
    }

    async fn set_selection(&self, selection: BootSelection) -> Result<(), String> {
        let _control = self.control_lock.lock().await;
        let entry_id = selection
            .entry_id
            .as_deref()
            .ok_or_else(|| "iDRAC WS-MAN boot selection must identify an entry".to_string())?;
        if selection.mode != BootMode::Once {
            return Err("iDRAC WS-MAN entries support one-time boot only".to_string());
        }
        self.set_concrete_entry(entry_id).await
    }

    async fn requirements(&self) -> Result<Vec<BackendRequirement>, String> {
        let _control = self.control_lock.lock().await;
        self.refresh_inventory_requirement().await?;
        Ok(self.runtime.requirements().await)
    }

    async fn remediate_requirement(
        &self,
        requirement_id: RequirementId,
        action: RemediationAction,
    ) -> Result<RemediationResult, String> {
        let _control = self.control_lock.lock().await;
        match (requirement_id, action) {
            (
                RequirementId::FirmwareInventoryCollection,
                RemediationAction::EnableInventoryCollection,
            ) => self.enable_inventory_collection().await,
        }
    }
}

impl IdracWsmanBootBackend {
    pub fn new(
        host: String,
        username: String,
        password: String,
        generation: u8,
        runtime: Arc<dyn BootStateStore>,
    ) -> Result<Self, String> {
        if !matches!(generation, 6 | 8) {
            return Err("idrac-wsman requires idrac_generation 6 or 8".to_string());
        }
        let authority = bmc_backend_support::endpoint::authority(&host, 443)?;
        let host = authority
            .strip_suffix(":443")
            .ok_or_else(|| "invalid management endpoint authority".to_string())?
            .to_string();
        Ok(Self {
            host,
            username,
            password,
            generation,
            runtime,
            wsman_sources: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            wsman_reported_selection: Arc::new(tokio::sync::RwLock::new(None)),
            wsman_one_time_active_state: Arc::new(tokio::sync::RwLock::new(None)),
            management_connection: Arc::new(tokio::sync::Mutex::new(None)),
            control_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub fn initial_requirement() -> BackendRequirement {
        Self::inventory_requirement(RequirementState::Unknown)
    }

    pub async fn discover(&self) {
        let _control = self.control_lock.lock().await;
        self.refresh_entries().await;
    }
}

/// Parse wire data in the shell; pagination policy belongs to the pure model.
fn next_enumeration_context(
    state: &mut bmc_idrac_model::wsman::Enumeration,
    response: &str,
    label: &str,
) -> Result<Option<String>, String> {
    use bmc_idrac_model::wsman::{EnumerationEffect, EnumerationEvent};
    let transition = std::mem::take(state).step(EnumerationEvent::Page {
        end: has_xml_element(response, "EndOfSequence"),
        context: xml_text(response, "EnumerationContext"),
    });
    *state = transition.state;
    match transition.effect {
        EnumerationEffect::Pull(context) => Ok(Some(context)),
        EnumerationEffect::Complete => Ok(None),
        EnumerationEffect::Reject(fault) => {
            Err(format!("iDRAC WS-MAN {label} {}", fault.message()))
        }
        EnumerationEffect::IgnoreTerminal => {
            Err("iDRAC WS-MAN enumeration already ended".to_string())
        }
    }
}

fn wsman_enumerate_request(host: &str, resource_uri: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsen="http://schemas.xmlsoap.org/ws/2004/09/enumeration" xmlns:wsman="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd"><s:Header><wsa:To s:mustUnderstand="true">https://{host}:443/wsman</wsa:To><wsman:ResourceURI s:mustUnderstand="true">{resource_uri}</wsman:ResourceURI><wsa:MessageID s:mustUnderstand="true">uuid:{}</wsa:MessageID><wsa:ReplyTo><wsa:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</wsa:Address></wsa:ReplyTo><wsa:Action s:mustUnderstand="true">http://schemas.xmlsoap.org/ws/2004/09/enumeration/Enumerate</wsa:Action></s:Header><s:Body><wsen:Enumerate><wsman:OptimizeEnumeration/><wsman:MaxElements>100</wsman:MaxElements></wsen:Enumerate></s:Body></s:Envelope>"#,
        uuid::Uuid::new_v4()
    )
}

fn wsman_pull_request(host: &str, resource_uri: &str, context: &str) -> String {
    let context = quick_xml::escape::escape(context);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsen="http://schemas.xmlsoap.org/ws/2004/09/enumeration" xmlns:wsman="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd"><s:Header><wsa:To s:mustUnderstand="true">https://{host}:443/wsman</wsa:To><wsman:ResourceURI s:mustUnderstand="true">{resource_uri}</wsman:ResourceURI><wsa:MessageID s:mustUnderstand="true">uuid:{}</wsa:MessageID><wsa:ReplyTo><wsa:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</wsa:Address></wsa:ReplyTo><wsa:Action s:mustUnderstand="true">http://schemas.xmlsoap.org/ws/2004/09/enumeration/Pull</wsa:Action></s:Header><s:Body><wsen:Pull><wsen:EnumerationContext>{context}</wsen:EnumerationContext><wsen:MaxElements>100</wsen:MaxElements></wsen:Pull></s:Body></s:Envelope>"#,
        uuid::Uuid::new_v4()
    )
}

fn wsman_change_one_time_boot_request(host: &str, source_instance_id: &str) -> String {
    let source_instance_id = quick_xml::escape::escape(source_instance_id);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsman="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" xmlns:n1="{BOOT_CONFIG_URI}"><s:Header><wsa:To s:mustUnderstand="true">https://{host}:443/wsman</wsa:To><wsman:ResourceURI s:mustUnderstand="true">{BOOT_CONFIG_URI}</wsman:ResourceURI><wsa:MessageID s:mustUnderstand="true">uuid:{}</wsa:MessageID><wsa:ReplyTo><wsa:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</wsa:Address></wsa:ReplyTo><wsa:Action s:mustUnderstand="true">{BOOT_CONFIG_URI}/ChangeBootOrderByInstanceID</wsa:Action><wsman:SelectorSet><wsman:Selector Name="InstanceID">OneTime</wsman:Selector></wsman:SelectorSet></s:Header><s:Body><n1:ChangeBootOrderByInstanceID_INPUT><n1:source>{source_instance_id}</n1:source></n1:ChangeBootOrderByInstanceID_INPUT></s:Body></s:Envelope>"#,
        uuid::Uuid::new_v4()
    )
}

struct LcRemediationRequests {
    set: String,
    commit: Option<String>,
}

fn wsman_lc_remediation_requests(host: &str, generation: u8) -> LcRemediationRequests {
    if generation == 6 {
        LcRemediationRequests {
            set: wsman_lc_set_attribute_request(host),
            commit: Some(wsman_lc_create_config_job_request_legacy(host)),
        }
    } else {
        LcRemediationRequests {
            set: wsman_lc_set_attributes_request(host),
            commit: Some(wsman_lc_create_config_job_request(host)),
        }
    }
}

fn wsman_lc_create_config_job_request_legacy(host: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsman="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" xmlns:n1="{LC_SERVICE_URI}"><s:Header><wsa:To s:mustUnderstand="true">https://{host}:443/wsman</wsa:To><wsman:ResourceURI s:mustUnderstand="true">{LC_SERVICE_URI}</wsman:ResourceURI><wsa:MessageID s:mustUnderstand="true">uuid:{}</wsa:MessageID><wsa:ReplyTo><wsa:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</wsa:Address></wsa:ReplyTo><wsa:Action s:mustUnderstand="true">{LC_SERVICE_URI}/CreateConfigJob</wsa:Action><wsman:SelectorSet><wsman:Selector Name="CreationClassName">DCIM_LCService</wsman:Selector><wsman:Selector Name="Name">DCIM:LCService</wsman:Selector><wsman:Selector Name="SystemCreationClassName">DCIM_ComputerSystem</wsman:Selector><wsman:Selector Name="SystemName">DCIM:ComputerSystem</wsman:Selector></wsman:SelectorSet></s:Header><s:Body><n1:CreateConfigJob_INPUT/></s:Body></s:Envelope>"#,
        uuid::Uuid::new_v4()
    )
}

fn wsman_lc_set_attribute_request(host: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsman="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" xmlns:n1="{LC_SERVICE_URI}"><s:Header><wsa:To s:mustUnderstand="true">https://{host}:443/wsman</wsa:To><wsman:ResourceURI s:mustUnderstand="true">{LC_SERVICE_URI}</wsman:ResourceURI><wsa:MessageID s:mustUnderstand="true">uuid:{}</wsa:MessageID><wsa:ReplyTo><wsa:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</wsa:Address></wsa:ReplyTo><wsa:Action s:mustUnderstand="true">{LC_SERVICE_URI}/SetAttribute</wsa:Action><wsman:SelectorSet><wsman:Selector Name="CreationClassName">DCIM_LCService</wsman:Selector><wsman:Selector Name="Name">DCIM:LCService</wsman:Selector><wsman:Selector Name="SystemCreationClassName">DCIM_ComputerSystem</wsman:Selector><wsman:Selector Name="SystemName">DCIM:ComputerSystem</wsman:Selector></wsman:SelectorSet></s:Header><s:Body><n1:SetAttribute_INPUT><n1:AttributeName>Collect System Inventory on Restart</n1:AttributeName><n1:AttributeValue>Enabled</n1:AttributeValue></n1:SetAttribute_INPUT></s:Body></s:Envelope>"#,
        uuid::Uuid::new_v4()
    )
}

fn wsman_lc_set_attributes_request(host: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsman="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" xmlns:n1="{LC_SERVICE_URI}"><s:Header><wsa:To s:mustUnderstand="true">https://{host}:443/wsman</wsa:To><wsman:ResourceURI s:mustUnderstand="true">{LC_SERVICE_URI}</wsman:ResourceURI><wsa:MessageID s:mustUnderstand="true">uuid:{}</wsa:MessageID><wsa:ReplyTo><wsa:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</wsa:Address></wsa:ReplyTo><wsa:Action s:mustUnderstand="true">{LC_SERVICE_URI}/SetAttributes</wsa:Action><wsman:SelectorSet><wsman:Selector Name="CreationClassName">DCIM_LCService</wsman:Selector><wsman:Selector Name="Name">DCIM:LCService</wsman:Selector><wsman:Selector Name="SystemCreationClassName">DCIM_ComputerSystem</wsman:Selector><wsman:Selector Name="SystemName">DCIM:ComputerSystem</wsman:Selector></wsman:SelectorSet></s:Header><s:Body><n1:SetAttributes_INPUT><n1:Target/><n1:AttributeValue>Enabled</n1:AttributeValue><n1:AttributeName>Collect System Inventory on Restart</n1:AttributeName></n1:SetAttributes_INPUT></s:Body></s:Envelope>"#,
        uuid::Uuid::new_v4()
    )
}

fn wsman_lc_create_config_job_request(host: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsman="http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd" xmlns:n1="{LC_SERVICE_URI}"><s:Header><wsa:To s:mustUnderstand="true">https://{host}:443/wsman</wsa:To><wsman:ResourceURI s:mustUnderstand="true">{LC_SERVICE_URI}</wsman:ResourceURI><wsa:MessageID s:mustUnderstand="true">uuid:{}</wsa:MessageID><wsa:ReplyTo><wsa:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</wsa:Address></wsa:ReplyTo><wsa:Action s:mustUnderstand="true">{LC_SERVICE_URI}/CreateConfigJob</wsa:Action><wsman:SelectorSet><wsman:Selector Name="CreationClassName">DCIM_LCService</wsman:Selector><wsman:Selector Name="Name">DCIM:LCService</wsman:Selector><wsman:Selector Name="SystemCreationClassName">DCIM_ComputerSystem</wsman:Selector><wsman:Selector Name="SystemName">DCIM:ComputerSystem</wsman:Selector></wsman:SelectorSet></s:Header><s:Body><n1:CreateConfigJob_INPUT><n1:Target/><n1:ScheduledStartTime>TIME_NOW</n1:ScheduledStartTime></n1:CreateConfigJob_INPUT></s:Body></s:Envelope>"#,
        uuid::Uuid::new_v4()
    )
}

fn require_wsman_return_value(xml: &str, expected: &str, operation: &str) -> Result<(), String> {
    let value = xml_text(xml, "ReturnValue").ok_or_else(|| {
        let detail = ["Message", "Text", "Value", "FaultDetail"]
            .into_iter()
            .flat_map(|name| {
                xml_text_values(xml, name)
                    .into_iter()
                    .map(move |value| format!("{name}={value}"))
            })
            .collect::<Vec<_>>();
        let elements = xml_element_names(xml, 16);
        format!(
            "iDRAC {operation} response has no ReturnValue (bytes={}, detail=[{}], elements=[{}])",
            xml.len(),
            detail.join(", "),
            elements.join(", ")
        )
    })?;
    if value == expected {
        return Ok(());
    }
    let message = xml_text(xml, "Message").unwrap_or_else(|| "no BMC message".to_string());
    Err(format!(
        "iDRAC {operation} returned {value}, expected {expected}: {message}"
    ))
}

fn xml_text_values(xml: &str, wanted: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut matched = false;
    let mut values = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                matched = event.local_name().as_ref() == wanted.as_bytes();
            }
            Ok(Event::Text(text)) if matched => {
                if let Ok(value) = text.decode() {
                    values.push(value.into_owned());
                }
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == wanted.as_bytes() => {
                matched = false;
            }
            Ok(Event::Eof) | Err(_) => return values,
            _ => {}
        }
    }
}

fn xml_element_names(xml: &str, limit: usize) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    let mut names = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event) | Event::Empty(event)) => {
                let name = String::from_utf8_lossy(event.local_name().as_ref()).to_string();
                if !names.contains(&name) {
                    names.push(name);
                    if names.len() == limit {
                        return names;
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => return names,
            _ => {}
        }
    }
}

fn xml_text(xml: &str, wanted: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut matched = false;
    loop {
        match reader.read_event().ok()? {
            Event::Start(event) => matched = event.local_name().as_ref() == wanted.as_bytes(),
            Event::Text(text) if matched => {
                return text.decode().ok().map(|value| value.into_owned())
            }
            Event::End(_) => matched = false,
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn has_xml_element(xml: &str, wanted: &str) -> bool {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == wanted.as_bytes() =>
            {
                return true
            }
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
    }
}

#[cfg(test)]
fn parse_wsman_boot_entries(xml: &str) -> Result<Vec<DiscoveredEntry>, String> {
    let entries = parse_wsman_boot_entries_page(xml)?;
    if entries.is_empty() {
        return Err("iDRAC WS-MAN returned no boot entries".to_string());
    }
    Ok(entries)
}

fn parse_wsman_boot_entries_page(xml: &str) -> Result<Vec<DiscoveredEntry>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut in_entry = false;
    let mut field = String::new();
    let mut values: HashMap<String, String> = HashMap::new();
    let mut entries = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = String::from_utf8_lossy(event.local_name().as_ref()).to_string();
                if name == "DCIM_BootSourceSetting" {
                    in_entry = true;
                    values.clear();
                } else if in_entry {
                    field = name;
                }
            }
            Ok(Event::Text(text)) if in_entry && !field.is_empty() => {
                values.insert(
                    field.clone(),
                    text.decode()
                        .map_err(|error| format!("invalid WS-MAN text: {error}"))?
                        .into_owned(),
                );
            }
            Ok(Event::End(event)) => {
                let name = String::from_utf8_lossy(event.local_name().as_ref()).to_string();
                if name == "DCIM_BootSourceSetting" {
                    let id = values.get("InstanceID").cloned();
                    let label = values
                        .get("BIOSBootString")
                        .or_else(|| values.get("ElementName"))
                        .cloned();
                    if let (Some(id), Some(label)) = (id, label) {
                        entries.push(DiscoveredEntry {
                            public: BootEntry {
                                id: wsman_public_entry_id(&id),
                                label: wsman_boot_label(&label),
                                group: Some("Specific boot entries".to_string()),
                                badges: wsman_boot_badges(&id),
                                enabled: values
                                    .get("PendingEnabledStatus")
                                    .or_else(|| values.get("CurrentEnabledStatus"))
                                    .is_none_or(|value| value == "1"),
                                selectable: false,
                                supported_modes: vec![BootMode::Once],
                            },
                            one_time_target_id: id
                                .strip_prefix("OneTime:")
                                .map(wsman_public_entry_id),
                            source_instance_id: Some(id),
                        });
                    }
                    in_entry = false;
                    values.clear();
                }
                field.clear();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("invalid iDRAC WS-MAN XML: {error}")),
        }
    }
    Ok(entries)
}

fn wsman_public_entry_id(source_instance_id: &str) -> String {
    format!("wsman:{:x}", Sha256::digest(source_instance_id.as_bytes()))
}

fn take_wsman_reported_selection(entries: &mut Vec<DiscoveredEntry>) -> Option<String> {
    let mut targets = entries
        .iter()
        .filter_map(|entry| entry.one_time_target_id.clone())
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    entries.retain(|entry| entry.one_time_target_id.is_none());
    (targets.len() == 1).then(|| targets.remove(0))
}

fn parse_wsman_boot_configs_page(xml: &str) -> Result<Vec<WsmanBootConfig>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut in_config = false;
    let mut field = String::new();
    let mut values: HashMap<String, String> = HashMap::new();
    let mut configs = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = String::from_utf8_lossy(event.local_name().as_ref()).to_string();
                if name == "DCIM_BootConfigSetting" {
                    in_config = true;
                    values.clear();
                } else if in_config {
                    field = name;
                }
            }
            Ok(Event::Text(text)) if in_config && !field.is_empty() => {
                values.insert(
                    field.clone(),
                    text.decode()
                        .map_err(|error| format!("invalid WS-MAN text: {error}"))?
                        .into_owned(),
                );
            }
            Ok(Event::End(event)) => {
                if event.local_name().as_ref() == b"DCIM_BootConfigSetting" {
                    if let Some(instance_id) = values.get("InstanceID").cloned() {
                        let parse_state = |field: &str| {
                            values.get(field).and_then(|value| value.parse::<u8>().ok())
                        };
                        configs.push(WsmanBootConfig {
                            instance_id,
                            is_current: parse_state("IsCurrent"),
                            is_next: parse_state("IsNext"),
                        });
                    }
                    in_config = false;
                    values.clear();
                }
                field.clear();
            }
            Ok(Event::Eof) if in_config => {
                return Err("invalid iDRAC WS-MAN XML: unterminated boot configuration".to_string())
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("invalid iDRAC WS-MAN XML: {error}")),
        }
    }
    Ok(configs)
}

fn parse_wsman_one_time_bios_state_page(xml: &str) -> Result<WsmanOneTimeBiosState, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut in_entry = false;
    let mut field = String::new();
    let mut attribute_name = None;
    let mut current_value = None;
    let mut pending_value = None;
    let mut state = WsmanOneTimeBiosState::default();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = String::from_utf8_lossy(event.local_name().as_ref()).to_string();
                if name == "DCIM_BIOSEnumeration" {
                    in_entry = true;
                    attribute_name = None;
                    current_value = None;
                    pending_value = None;
                } else if in_entry {
                    field = name;
                }
            }
            Ok(Event::Text(text)) if in_entry => {
                let value = text
                    .decode()
                    .map_err(|error| format!("invalid WS-MAN text: {error}"))?
                    .into_owned();
                match field.as_str() {
                    "AttributeName" => attribute_name = Some(value),
                    "CurrentValue" => current_value = Some(value),
                    "PendingValue" => pending_value = Some(value),
                    _ => {}
                }
            }
            Ok(Event::End(event)) => {
                if event.local_name().as_ref() == b"DCIM_BIOSEnumeration" {
                    let value = pending_value.take().or_else(|| current_value.take());
                    match attribute_name.as_deref() {
                        Some("OneTimeBootMode") => state.mode = value,
                        Some("OneTimeBootSeqDev") => state.boot_device = value,
                        Some("OneTimeHddSeqDev") => state.hdd_device = value,
                        Some("OneTimeUefiBootSeqDev") => state.uefi_device = value,
                        _ => {}
                    }
                    in_entry = false;
                }
                field.clear();
            }
            Ok(Event::Eof) if in_entry => {
                return Err("invalid iDRAC WS-MAN XML: unterminated BIOS enumeration".to_string())
            }
            Ok(Event::Eof) => return Ok(state),
            Ok(_) => {}
            Err(error) => return Err(format!("invalid iDRAC WS-MAN XML: {error}")),
        }
    }
}

fn parse_wsman_csior_status(xml: &str) -> Option<CsiorStatus> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut in_entry = false;
    let mut field = String::new();
    let mut attribute_name = None;
    let mut current_value = None;
    let mut pending_value = None;
    let mut lifecycle_licensed = None;
    let mut csior_current = None;
    let mut csior_pending = None;
    let mut found_csior = false;
    loop {
        match reader.read_event().ok()? {
            Event::Start(event) => {
                let name = String::from_utf8_lossy(event.local_name().as_ref()).to_string();
                if name == "DCIM_LCEnumeration" {
                    in_entry = true;
                    attribute_name = None;
                    current_value = None;
                    pending_value = None;
                } else if in_entry {
                    field = name;
                }
            }
            Event::Text(text) if in_entry => {
                let value = text.decode().ok()?.into_owned();
                match field.as_str() {
                    "AttributeName" => attribute_name = Some(value),
                    "CurrentValue" => current_value = Some(value),
                    "PendingValue" => pending_value = Some(value),
                    _ => {}
                }
            }
            Event::End(event) => {
                if event.local_name().as_ref() == b"DCIM_LCEnumeration" {
                    let enabled = |value: Option<&str>| match value {
                        Some("Enabled") => Some(true),
                        Some("Disabled") => Some(false),
                        _ => None,
                    };
                    match attribute_name.as_deref() {
                        Some("Licensed") => {
                            lifecycle_licensed = match current_value.as_deref() {
                                Some("Yes") => Some(true),
                                Some("No") => Some(false),
                                _ => None,
                            };
                        }
                        Some("Collect System Inventory on Restart") => {
                            found_csior = true;
                            csior_current = enabled(current_value.as_deref());
                            csior_pending = enabled(pending_value.as_deref());
                        }
                        _ => {}
                    }
                    in_entry = false;
                }
                field.clear();
            }
            Event::Eof => {
                return found_csior.then_some(CsiorStatus {
                    current_enabled: csior_current,
                    pending_enabled: csior_pending,
                    lifecycle_licensed,
                })
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestBootState {
        discovery: tokio::sync::RwLock<BootDiscoverySnapshot>,
        requirements: tokio::sync::RwLock<Vec<BackendRequirement>>,
    }

    #[async_trait]
    impl bmc_feature_api::boot::BootDiscoveryStore for TestBootState {
        async fn discovery(&self) -> BootDiscoverySnapshot {
            self.discovery.read().await.clone()
        }

        async fn set_discovery(&self, discovery: BootDiscoverySnapshot) {
            *self.discovery.write().await = discovery;
        }
    }

    #[async_trait]
    impl BootStateStore for TestBootState {
        async fn requirements(&self) -> Vec<BackendRequirement> {
            self.requirements.read().await.clone()
        }

        async fn set_requirements(&self, requirements: Vec<BackendRequirement>) {
            *self.requirements.write().await = requirements;
        }

        async fn set_selected_entry_id(&self, selected_entry_id: Option<String>) {
            self.discovery.write().await.selected_entry_id = selected_entry_id;
        }

        async fn clear_selected_entry_id_if(&self, expected_entry_id: &str) {
            let mut discovery = self.discovery.write().await;
            if discovery.selected_entry_id.as_deref() == Some(expected_entry_id) {
                discovery.selected_entry_id = None;
            }
        }
    }

    fn discovering_backend() -> IdracWsmanBootBackend {
        let runtime = Arc::new(TestBootState {
            discovery: tokio::sync::RwLock::new(BootDiscoverySnapshot {
                entries: Vec::new(),
                state: BootEntryDiscoveryState::Discovering,
                selected_entry_id: None,
            }),
            requirements: tokio::sync::RwLock::new(vec![
                IdracWsmanBootBackend::inventory_requirement(RequirementState::Unknown),
            ]),
        });
        IdracWsmanBootBackend {
            host: "unreachable.invalid".to_string(),
            username: "fixture".to_string(),
            password: "fixture".to_string(),
            generation: 8,
            runtime,
            wsman_sources: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            wsman_reported_selection: Arc::new(tokio::sync::RwLock::new(None)),
            wsman_one_time_active_state: Arc::new(tokio::sync::RwLock::new(None)),
            management_connection: Arc::new(tokio::sync::Mutex::new(None)),
            control_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    #[test]
    fn management_host_is_validated_and_ipv6_soap_authority_is_bracketed() {
        let runtime = discovering_backend().runtime;
        for input in ["::1", "[::1]"] {
            let backend = IdracWsmanBootBackend::new(
                input.into(),
                String::new(),
                String::new(),
                8,
                runtime.clone(),
            )
            .unwrap();
            assert_eq!(backend.host, "[::1]");
            assert!(wsman_enumerate_request(&backend.host, BOOT_SOURCE_URI)
                .contains("https://[::1]:443/wsman"));
        }
        for input in ["user@host", "host/path", "host<xml", "host:8443"] {
            assert!(IdracWsmanBootBackend::new(
                input.into(),
                String::new(),
                String::new(),
                8,
                runtime.clone(),
            )
            .is_err());
        }
    }

    async fn cached_http_peer(backend: &IdracWsmanBootBackend) -> tokio::io::DuplexStream {
        let (client, peer) = tokio::io::duplex(4096);
        let (sender, connection) = http1::handshake(TokioIo::new(client)).await.unwrap();
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        *backend.management_connection.lock().await = Some(LegacyHttpConnection { sender, task });
        peer
    }

    async fn read_http_headers(peer: &mut tokio::io::DuplexStream) {
        use tokio::io::AsyncReadExt;
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8];
            peer.read_exact(&mut byte).await.unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 4096);
        }
    }

    async fn read_http_request(peer: &mut tokio::io::DuplexStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8];
            peer.read_exact(&mut byte).await.unwrap();
            headers.push(byte[0]);
            assert!(headers.len() <= 16384);
        }
        let headers = String::from_utf8(headers).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; length];
        peer.read_exact(&mut body).await.unwrap();
        String::from_utf8(body).unwrap()
    }

    #[tokio::test]
    async fn boot_selection_rechecks_activation_after_entry_readback_before_publishing_success() {
        use tokio::io::AsyncWriteExt;
        for consumed_during_readback in [false, true] {
            let backend = discovering_backend();
            let source = "UEFI:SATA.Embedded.1-1:opaque";
            let source_xml = format!(
                r#"<s:Envelope xmlns:s="urn:test"><s:Body><DCIM_BootSourceSetting><BIOSBootString>Disk</BIOSBootString><InstanceID>{source}</InstanceID><PendingEnabledStatus>1</PendingEnabledStatus></DCIM_BootSourceSetting><DCIM_BootSourceSetting><BIOSBootString>Disk</BIOSBootString><InstanceID>OneTime:{source}</InstanceID><PendingEnabledStatus>1</PendingEnabledStatus></DCIM_BootSourceSetting><EndOfSequence/></s:Body></s:Envelope>"#
            );
            let active_xml = "<Envelope><DCIM_BootConfigSetting><InstanceID>OneTime</InstanceID><IsNext>1</IsNext></DCIM_BootConfigSetting><EndOfSequence/></Envelope>";
            let inactive_xml = "<Envelope><DCIM_BootConfigSetting><InstanceID>OneTime</InstanceID><IsNext>2</IsNext></DCIM_BootConfigSetting><EndOfSequence/></Envelope>";
            let mut entries = parse_wsman_boot_entries(&source_xml).unwrap();
            let expected = take_wsman_reported_selection(&mut entries).unwrap();
            apply_wsman_one_time_capability(&mut entries, true);
            backend.remember_wsman_sources(&entries).await;
            backend
                .runtime
                .set_discovery(BootDiscoverySnapshot {
                    entries: entries.into_iter().map(|entry| entry.public).collect(),
                    state: BootEntryDiscoveryState::Available,
                    selected_entry_id: None,
                })
                .await;
            let mut peer = cached_http_peer(&backend).await;
            let device = async {
                let mut responses = vec![
                    (200, "<ReturnValue>0</ReturnValue>"),
                    (200, active_xml),
                    (200, source_xml.as_str()),
                    (
                        200,
                        if consumed_during_readback {
                            inactive_xml
                        } else {
                            active_xml
                        },
                    ),
                ];
                if consumed_during_readback {
                    responses.push((503, "unavailable"));
                }
                for (index, (status, body)) in responses.into_iter().enumerate() {
                    let request = read_http_request(&mut peer).await;
                    if index == 0 {
                        assert!(request.contains("ChangeBootOrderByInstanceID"));
                    }
                    if index == 3 {
                        assert!(
                            request.contains(BOOT_CONFIG_URI),
                            "the operation must recheck activation"
                        );
                        assert!(backend.entries().await.selected_entry_id.is_none());
                    }
                    let response = format!(
                        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    peer.write_all(response.as_bytes()).await.unwrap();
                }
            };
            let scenario = async { tokio::join!(backend.set_wsman_entry(&expected), device) };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(5), scenario)
                .await
                .unwrap();
            if consumed_during_readback {
                assert!(result.is_err());
                assert!(backend.entries().await.selected_entry_id.is_none());
            } else {
                result.unwrap();
                assert_eq!(
                    backend.entries().await.selected_entry_id.as_deref(),
                    Some(expected.as_str())
                );
            }
        }
    }

    #[tokio::test]
    async fn cancellation_during_a_partial_http_body_discards_cached_connection() {
        use tokio::io::AsyncWriteExt;
        let backend = discovering_backend();
        let mut peer = cached_http_peer(&backend).await;
        let requester = backend.clone();
        let request = tokio::spawn(async move {
            requester
                .legacy_request(Method::POST, "/wsman", &[], Vec::new())
                .await
                .map(|response| response.body)
        });
        tokio::time::timeout(Duration::from_secs(1), read_http_headers(&mut peer))
            .await
            .unwrap();
        peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc")
            .await
            .unwrap();
        // The cache was taken before sending, so cancellation cannot restore
        // a sender whose response still contains seven unread bytes.
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert!(backend.management_connection.lock().await.is_none());
    }

    #[tokio::test]
    async fn http_connection_close_token_discards_even_a_successful_response() {
        use tokio::io::AsyncWriteExt;
        let backend = discovering_backend();
        let mut peer = cached_http_peer(&backend).await;
        let device = async {
            read_http_headers(&mut peer).await;
            peer.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive, close\r\n\r\nok",
            )
            .await
            .unwrap();
        };
        let (result, ()) = tokio::join!(
            backend.legacy_request(Method::POST, "/wsman", &[], Vec::new()),
            device
        );
        assert_eq!(result.unwrap().body.as_ref(), b"ok");
        assert!(backend.management_connection.lock().await.is_none());
    }

    #[tokio::test]
    async fn capabilities_do_not_wait_for_management_discovery() {
        let capabilities = tokio::time::timeout(
            Duration::from_millis(20),
            discovering_backend().capabilities(),
        )
        .await
        .expect("cached capabilities must not perform network I/O");
        assert_eq!(
            capabilities.entry_discovery,
            BootEntryDiscoveryState::Discovering
        );
        assert!(capabilities.entries.is_empty());
    }

    #[test]
    fn parses_disabled_csior_without_inferring_inventory_entries() {
        let xml = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:n1="http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_LCEnumeration"><s:Body><n1:DCIM_LCEnumeration><n1:AttributeName>Unrelated</n1:AttributeName><n1:CurrentValue>Enabled</n1:CurrentValue></n1:DCIM_LCEnumeration><n1:DCIM_LCEnumeration><n1:AttributeName>Collect System Inventory on Restart</n1:AttributeName><n1:CurrentValue>Disabled</n1:CurrentValue></n1:DCIM_LCEnumeration></s:Body></s:Envelope>"#;
        assert_eq!(
            parse_wsman_csior_status(xml),
            Some(CsiorStatus {
                current_enabled: Some(false),
                pending_enabled: None,
                lifecycle_licensed: None,
            })
        );
    }

    #[test]
    fn parses_pending_csior_as_distinct_from_current_state() {
        let xml = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:n1="http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_LCEnumeration"><s:Body><n1:DCIM_LCEnumeration><n1:AttributeName>Collect System Inventory on Restart</n1:AttributeName><n1:CurrentValue>Disabled</n1:CurrentValue><n1:PendingValue>Enabled</n1:PendingValue></n1:DCIM_LCEnumeration></s:Body></s:Envelope>"#;
        assert_eq!(
            parse_wsman_csior_status(xml),
            Some(CsiorStatus {
                current_enabled: Some(false),
                pending_enabled: Some(true),
                lifecycle_licensed: None,
            })
        );
    }

    #[test]
    fn parses_unlicensed_lifecycle_controller_with_csior() {
        let xml = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:n1="http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_LCEnumeration"><s:Body><n1:DCIM_LCEnumeration><n1:AttributeName>Licensed</n1:AttributeName><n1:CurrentValue>No</n1:CurrentValue></n1:DCIM_LCEnumeration><n1:DCIM_LCEnumeration><n1:AttributeName>Collect System Inventory on Restart</n1:AttributeName><n1:CurrentValue>Disabled</n1:CurrentValue></n1:DCIM_LCEnumeration></s:Body></s:Envelope>"#;
        let status = parse_wsman_csior_status(xml).expect("CSIOR status");
        assert_eq!(status.lifecycle_licensed, Some(false));
        assert_eq!(
            inventory_requirement_state(Some(status)),
            RequirementState::Unavailable
        );
    }

    #[test]
    fn only_action_required_inventory_advertises_remediation() {
        let actionable =
            IdracWsmanBootBackend::inventory_requirement(RequirementState::ActionRequired);
        assert_eq!(
            actionable.remediation.as_ref().map(|item| item.action),
            Some(RemediationAction::EnableInventoryCollection)
        );
        assert_eq!(
            actionable
                .remediation
                .as_ref()
                .map(|item| item.restart_requirement),
            Some(RestartRequirement::NextHostPost)
        );

        for state in [
            RequirementState::Satisfied,
            RequirementState::Pending,
            RequirementState::Unavailable,
            RequirementState::Unknown,
        ] {
            assert!(IdracWsmanBootBackend::inventory_requirement(state)
                .remediation
                .is_none());
        }
    }

    #[test]
    fn inventory_requirement_state_preserves_current_and_pending_semantics() {
        assert_eq!(
            inventory_requirement_state(Some(CsiorStatus {
                current_enabled: Some(true),
                pending_enabled: None,
                lifecycle_licensed: Some(false),
            })),
            RequirementState::Satisfied
        );
        assert_eq!(
            inventory_requirement_state(Some(CsiorStatus {
                current_enabled: Some(false),
                pending_enabled: Some(true),
                lifecycle_licensed: None,
            })),
            RequirementState::Pending
        );
        assert_eq!(
            inventory_requirement_state(Some(CsiorStatus {
                current_enabled: Some(false),
                pending_enabled: None,
                lifecycle_licensed: None,
            })),
            RequirementState::ActionRequired
        );
        assert_eq!(inventory_requirement_state(None), RequirementState::Unknown);
    }

    #[test]
    fn lc_remediation_requests_follow_the_generation_specific_contract() {
        let idrac6 = wsman_lc_remediation_requests("bmc.example", 6);
        assert!(idrac6.set.contains("DCIM_LCService/SetAttribute"));
        assert!(idrac6.set.contains("<n1:SetAttribute_INPUT>"));
        assert!(idrac6.set.contains("Collect System Inventory on Restart"));
        assert!(idrac6
            .set
            .contains("<n1:AttributeValue>Enabled</n1:AttributeValue>"));
        assert!(!idrac6.set.contains("<n1:Target"));
        let commit = idrac6.commit.expect("iDRAC6 configuration job");
        assert!(commit.contains("DCIM_LCService/CreateConfigJob"));
        assert!(commit.contains("<n1:CreateConfigJob_INPUT/>"));
        assert!(!commit.contains("ScheduledStartTime"));
        assert!(!commit.contains("RebootIfRequired"));

        let idrac8 = wsman_lc_remediation_requests("bmc.example", 8);
        assert!(idrac8.set.contains("DCIM_LCService/SetAttributes"));
        assert!(idrac8.set.contains("<n1:SetAttributes_INPUT>"));
        let commit = idrac8.commit.expect("iDRAC8 configuration job");
        assert!(commit.contains("DCIM_LCService/CreateConfigJob"));
        assert!(commit.contains("<n1:ScheduledStartTime>TIME_NOW</n1:ScheduledStartTime>"));
        assert!(!commit.contains("RebootJobType"));
    }

    #[test]
    fn remediation_result_requires_the_documented_return_value() {
        assert!(require_wsman_return_value(
            "<x><ReturnValue>4096</ReturnValue></x>",
            "4096",
            "test"
        )
        .is_ok());
        assert!(require_wsman_return_value(
            "<x><ReturnValue>2</ReturnValue><Message>rejected</Message></x>",
            "4096",
            "test"
        )
        .unwrap_err()
        .contains("rejected"));
        let fault = require_wsman_return_value(
            "<s:Envelope xmlns:s=\"urn:test\"><s:Body><s:Fault><s:Code><s:Value>s:Receiver</s:Value></s:Code><s:Reason><s:Text>Configuration job queue is busy</s:Text></s:Reason></s:Fault></s:Body></s:Envelope>",
            "0",
            "test",
        )
        .unwrap_err();
        assert!(fault.contains("Text=Configuration job queue is busy"));
        assert!(fault.contains("Value=s:Receiver"));
        assert!(fault.contains("elements=[Envelope, Body, Fault, Code, Value, Reason, Text]"));
    }

    #[test]
    fn parses_idrac6_and_idrac8_wsman_boot_entries_without_guessing_ids() {
        let xml = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:n1="http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_BootSourceSetting"><s:Body><n1:DCIM_BootSourceSetting><n1:BIOSBootString>Embedded NIC UefiBootSeq</n1:BIOSBootString><n1:InstanceID>UEFI:NIC.Embedded.1-1:opaque</n1:InstanceID><n1:PendingEnabledStatus>1</n1:PendingEnabledStatus></n1:DCIM_BootSourceSetting><n1:DCIM_BootSourceSetting><n1:BIOSBootString>Hard drive BootSeq</n1:BIOSBootString><n1:BootSourceType>IPL</n1:BootSourceType><n1:InstanceID>IPL:BIOS.Setup.1-1#HardDisk.List.1-1#opaque</n1:InstanceID><n1:PendingEnabledStatus>0</n1:PendingEnabledStatus></n1:DCIM_BootSourceSetting></s:Body></s:Envelope>"#;
        let mut entries = parse_wsman_boot_entries(xml).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].public.supported_modes, vec![BootMode::Once]);
        assert_eq!(
            entries[0].public.id,
            wsman_public_entry_id("UEFI:NIC.Embedded.1-1:opaque")
        );
        assert_eq!(
            entries[0].source_instance_id.as_deref(),
            Some("UEFI:NIC.Embedded.1-1:opaque")
        );
        assert!(!serde_json::to_string(&entries[0].public)
            .unwrap()
            .contains("UEFI:NIC.Embedded.1-1:opaque"));
        assert!(!entries[0].public.selectable);
        assert_eq!(entries[1].public.supported_modes, vec![BootMode::Once]);
        assert!(!entries[1].public.enabled);
        apply_wsman_one_time_capability(&mut entries, true);
        assert!(entries[0].public.selectable);
        assert!(!entries[1].public.selectable);
        apply_wsman_one_time_capability(&mut entries, false);
        assert!(entries.iter().all(|entry| !entry.public.selectable));
    }

    #[test]
    fn one_time_capability_uses_enabled_dell_source_families_not_uefi_policy() {
        let xml = format!(
            r#"<s:Envelope xmlns:s="urn:test" xmlns:n1="{BOOT_SOURCE_URI}"><s:Body><n1:DCIM_BootSourceSetting><n1:BIOSBootString>Legacy boot sequence</n1:BIOSBootString><n1:BootSourceType>IPL</n1:BootSourceType><n1:InstanceID>IPL:BIOS.Setup.1-1#BootSeq#HardDisk.List.1-1#opaque</n1:InstanceID><n1:PendingEnabledStatus>1</n1:PendingEnabledStatus></n1:DCIM_BootSourceSetting><n1:DCIM_BootSourceSetting><n1:BIOSBootString>Legacy disk sequence</n1:BIOSBootString><n1:BootSourceType>BCV</n1:BootSourceType><n1:InstanceID>BCV:BIOS.Setup.1-1#HddSeq#Disk.SATA.1#opaque</n1:InstanceID><n1:PendingEnabledStatus>1</n1:PendingEnabledStatus></n1:DCIM_BootSourceSetting><n1:DCIM_BootSourceSetting><n1:BIOSBootString>UEFI sequence</n1:BIOSBootString><n1:BootSourceType>UEFI</n1:BootSourceType><n1:InstanceID>UEFI:BIOS.Setup.1-1#UefiBootSeq#Disk.SATA.1#opaque</n1:InstanceID><n1:PendingEnabledStatus>1</n1:PendingEnabledStatus></n1:DCIM_BootSourceSetting></s:Body></s:Envelope>"#
        );
        let mut entries = parse_wsman_boot_entries(&xml).unwrap();
        apply_wsman_one_time_capability(&mut entries, true);
        assert!(entries.iter().all(|entry| entry.public.selectable));
    }

    #[test]
    fn wsman_labels_hide_protocol_sequence_suffixes() {
        let xml = format!(
            r#"<s:Envelope xmlns:s="urn:test" xmlns:n1="{BOOT_SOURCE_URI}"><s:Body><n1:DCIM_BootSourceSetting><n1:BIOSBootString>Hard drive C: BootSeq</n1:BIOSBootString><n1:BootSourceType>IPL</n1:BootSourceType><n1:InstanceID>IPL:HardDisk.List.1-1</n1:InstanceID></n1:DCIM_BootSourceSetting><n1:DCIM_BootSourceSetting><n1:BIOSBootString>Embedded SATA 1: Example SSD HddSeq</n1:BIOSBootString><n1:BootSourceType>BCV</n1:BootSourceType><n1:InstanceID>BCV:Disk.SATA.1</n1:InstanceID></n1:DCIM_BootSourceSetting><n1:DCIM_BootSourceSetting><n1:BIOSBootString>EFI Fixed Disk Boot Device 1 UefiBootSeq</n1:BIOSBootString><n1:BootSourceType>UEFI</n1:BootSourceType><n1:InstanceID>UEFI:Disk.SATA.1</n1:InstanceID></n1:DCIM_BootSourceSetting></s:Body></s:Envelope>"#
        );
        let entries = parse_wsman_boot_entries(&xml).unwrap();
        assert_eq!(entries[0].public.label, "Hard drive C:");
        assert_eq!(entries[1].public.label, "Embedded SATA 1: Example SSD");
        assert_eq!(entries[2].public.label, "EFI Fixed Disk Boot Device 1");
        assert!(entries
            .iter()
            .all(|entry| entry.public.group.as_deref() == Some("Specific boot entries")));
        assert_eq!(entries[0].public.badges, vec!["Legacy"]);
        assert_eq!(entries[1].public.badges, vec!["Legacy"]);
        assert_eq!(entries[2].public.badges, vec!["UEFI"]);
    }

    #[test]
    fn one_time_boot_source_alias_is_readback_not_a_duplicate_public_entry() {
        let source = "UEFI:SATA.Embedded.1-1:opaque";
        let xml = format!(
            r#"<s:Envelope xmlns:s="urn:test" xmlns:n1="{BOOT_SOURCE_URI}"><s:Body><n1:DCIM_BootSourceSetting><n1:BIOSBootString>EFI Fixed Disk Boot Device 1</n1:BIOSBootString><n1:InstanceID>{source}</n1:InstanceID><n1:PendingEnabledStatus>1</n1:PendingEnabledStatus></n1:DCIM_BootSourceSetting><n1:DCIM_BootSourceSetting><n1:BIOSBootString>EFI Fixed Disk Boot Device 1</n1:BIOSBootString><n1:InstanceID>OneTime:{source}</n1:InstanceID><n1:PendingEnabledStatus>1</n1:PendingEnabledStatus></n1:DCIM_BootSourceSetting></s:Body></s:Envelope>"#
        );
        let mut entries = parse_wsman_boot_entries(&xml).unwrap();
        let expected = wsman_public_entry_id(source);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[1].one_time_target_id.as_deref(),
            Some(expected.as_str())
        );
        let selected = take_wsman_reported_selection(&mut entries);
        assert_eq!(selected.as_deref(), Some(expected.as_str()));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].public.supported_modes, vec![BootMode::Once]);
    }

    #[test]
    fn parses_one_time_boot_configuration_without_inferring_missing_capability() {
        let xml = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:n1="http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_BootConfigSetting"><s:Body><n1:DCIM_BootConfigSetting><n1:InstanceID>UEFI</n1:InstanceID><n1:IsCurrent>1</n1:IsCurrent><n1:IsNext>2</n1:IsNext></n1:DCIM_BootConfigSetting><n1:DCIM_BootConfigSetting><n1:InstanceID>OneTime</n1:InstanceID><n1:IsCurrent>2</n1:IsCurrent><n1:IsNext>3</n1:IsNext></n1:DCIM_BootConfigSetting></s:Body></s:Envelope>"#;
        let configs = parse_wsman_boot_configs_page(xml).unwrap();
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].instance_id, "UEFI");
        assert_eq!(configs[0].is_current, Some(1));
        assert!(configs[1].is_one_time());
        assert!(configs[1].is_one_time_active());
        assert!(wsman_supports_one_time(Some(&configs), false));
        assert!(wsman_supports_one_time(None, true));
        assert!(!wsman_supports_one_time(None, false));

        let without_one_time = r#"<n1:DCIM_BootConfigSetting xmlns:n1="urn:test"><n1:InstanceID>UEFI</n1:InstanceID><n1:IsNext>2</n1:IsNext></n1:DCIM_BootConfigSetting>"#;
        assert!(!parse_wsman_boot_configs_page(without_one_time)
            .unwrap()
            .iter()
            .any(WsmanBootConfig::is_one_time));
        let configs_without_one_time = parse_wsman_boot_configs_page(without_one_time).unwrap();
        assert!(!wsman_supports_one_time(
            Some(&configs_without_one_time),
            true
        ));
        assert!(parse_wsman_boot_configs_page("<DCIM_BootConfigSetting>").is_err());
    }

    #[test]
    fn bios_one_time_device_maps_each_dell_sequence_to_exact_source_component() {
        let bios_xml = r#"<s:Envelope xmlns:s="urn:test" xmlns:n1="http://schemas.dell.com/wbem/wscim/1/cim-schema/2/DCIM_BIOSEnumeration"><s:Body><n1:DCIM_BIOSEnumeration><n1:AttributeName>OneTimeBootMode</n1:AttributeName><n1:CurrentValue>OneTimeUefiBootSeq</n1:CurrentValue></n1:DCIM_BIOSEnumeration><n1:DCIM_BIOSEnumeration><n1:AttributeName>OneTimeBootSeqDev</n1:AttributeName><n1:CurrentValue>HardDisk.List.1-1</n1:CurrentValue></n1:DCIM_BIOSEnumeration><n1:DCIM_BIOSEnumeration><n1:AttributeName>OneTimeHddSeqDev</n1:AttributeName><n1:CurrentValue>Disk.SATAEmbedded.J-1</n1:CurrentValue></n1:DCIM_BIOSEnumeration><n1:DCIM_BIOSEnumeration><n1:AttributeName>OneTimeUefiBootSeqDev</n1:AttributeName><n1:CurrentValue>Optical.SATAEmbedded.E-1</n1:CurrentValue></n1:DCIM_BIOSEnumeration></s:Body></s:Envelope>"#;
        let state = parse_wsman_one_time_bios_state_page(bios_xml).unwrap();
        assert_eq!(state.mode.as_deref(), Some("OneTimeUefiBootSeq"));
        assert_eq!(
            state.uefi_device.as_deref(),
            Some("Optical.SATAEmbedded.E-1")
        );
        assert_eq!(state.boot_device.as_deref(), Some("HardDisk.List.1-1"));
        assert_eq!(state.hdd_device.as_deref(), Some("Disk.SATAEmbedded.J-1"));

        let sources_xml = format!(
            r#"<s:Envelope xmlns:s="urn:test" xmlns:n1="{BOOT_SOURCE_URI}"><s:Body><n1:DCIM_BootSourceSetting><n1:BIOSBootString>Virtual optical</n1:BIOSBootString><n1:BootSourceType>UEFI</n1:BootSourceType><n1:InstanceID>UEFI:BIOS.Setup.1-1#UefiBootSeq#Optical.SATAEmbedded.E-1#opaque</n1:InstanceID></n1:DCIM_BootSourceSetting><n1:DCIM_BootSourceSetting><n1:BIOSBootString>Network</n1:BIOSBootString><n1:BootSourceType>UEFI</n1:BootSourceType><n1:InstanceID>UEFI:BIOS.Setup.1-1#UefiBootSeq#NIC.Embedded.1-1-1#opaque</n1:InstanceID></n1:DCIM_BootSourceSetting></s:Body></s:Envelope>"#
        );
        let entries = parse_wsman_boot_entries(&sources_xml).unwrap();
        assert_eq!(
            wsman_bios_selected_entry_id(&state, &entries),
            Some(entries[0].public.id.clone())
        );

        let disabled = WsmanOneTimeBiosState {
            mode: Some("Disabled".to_string()),
            boot_device: state.boot_device,
            hdd_device: state.hdd_device,
            uefi_device: state.uefi_device,
        };
        assert!(wsman_bios_selected_entry_id(&disabled, &entries).is_none());
        assert!(parse_wsman_one_time_bios_state_page("<DCIM_BIOSEnumeration>").is_err());
    }

    #[test]
    fn bios_one_time_readback_matches_the_reported_dell_source_family() {
        let sources_xml = format!(
            r#"<s:Envelope xmlns:s="urn:test" xmlns:n1="{BOOT_SOURCE_URI}"><s:Body><n1:DCIM_BootSourceSetting><n1:BIOSBootString>Legacy boot sequence</n1:BIOSBootString><n1:BootSourceType>IPL</n1:BootSourceType><n1:InstanceID>IPL:BIOS.Setup.1-1#BootSeq#HardDisk.List.1-1#opaque</n1:InstanceID></n1:DCIM_BootSourceSetting><n1:DCIM_BootSourceSetting><n1:BIOSBootString>Legacy disk sequence</n1:BIOSBootString><n1:BootSourceType>BCV</n1:BootSourceType><n1:InstanceID>BCV:BIOS.Setup.1-1#HddSeq#Disk.SATAEmbedded.J-1#opaque</n1:InstanceID></n1:DCIM_BootSourceSetting><n1:DCIM_BootSourceSetting><n1:BIOSBootString>UEFI sequence with the same device component</n1:BIOSBootString><n1:BootSourceType>UEFI</n1:BootSourceType><n1:InstanceID>UEFI:BIOS.Setup.1-1#UefiBootSeq#Disk.SATAEmbedded.J-1#opaque</n1:InstanceID></n1:DCIM_BootSourceSetting></s:Body></s:Envelope>"#
        );
        let entries = parse_wsman_boot_entries(&sources_xml).unwrap();

        let ipl = WsmanOneTimeBiosState {
            mode: Some("OneTimeBootSeq".to_string()),
            boot_device: Some("HardDisk.List.1-1".to_string()),
            ..Default::default()
        };
        assert_eq!(
            wsman_bios_selected_entry_id(&ipl, &entries),
            Some(entries[0].public.id.clone())
        );

        let bcv = WsmanOneTimeBiosState {
            mode: Some("OneTimeHddSeq".to_string()),
            hdd_device: Some("Disk.SATAEmbedded.J-1".to_string()),
            ..Default::default()
        };
        assert_eq!(
            wsman_bios_selected_entry_id(&bcv, &entries),
            Some(entries[1].public.id.clone())
        );
    }

    #[test]
    fn one_time_boot_request_uses_confirmed_dell_method_and_escapes_source() {
        let request =
            wsman_change_one_time_boot_request("bmc.example", "UEFI:Device.1-1:opaque<&>");
        assert!(request.contains(BOOT_CONFIG_URI));
        assert!(request.contains(&format!("{BOOT_CONFIG_URI}/ChangeBootOrderByInstanceID")));
        assert!(request.contains("<wsman:Selector Name=\"InstanceID\">OneTime</wsman:Selector>"));
        assert!(request.contains("<n1:ChangeBootOrderByInstanceID_INPUT>"));
        assert!(request.contains("<n1:source>UEFI:Device.1-1:opaque&lt;&amp;&gt;</n1:source>"));
        assert!(!request.contains("opaque<&>"));
    }

    #[test]
    fn wsman_request_uses_dell_boot_source_resource_and_optimized_enumeration() {
        let request = wsman_enumerate_request("bmc.example", BOOT_SOURCE_URI);
        assert!(request.contains(BOOT_SOURCE_URI));
        assert!(request.contains("OptimizeEnumeration"));
        assert!(request.contains("<wsman:MaxElements>100</wsman:MaxElements>"));
        assert!(request.contains("https://bmc.example:443/wsman"));
        assert!(!request.contains("OperationTimeout"));
    }
}
