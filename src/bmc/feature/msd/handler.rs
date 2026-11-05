use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use tokio::io::AsyncWriteExt;
use tracing::warn;

use crate::bmc::feature::connection::{FeatureConnection, FeatureGuard};
use crate::bmc::feature::fault::{
    acquisition_fault_response, fault_response, record_fault_response, unavailable_fault_response,
    FeatureFault, FeatureFaultDomain,
};
use crate::bmc::feature::msd::{
    feature_runtime as msd_feature_runtime, MsdAttachment, MsdBackend, MsdCapabilities,
    MsdDigestAlgorithm, MsdFeatureRuntime, MsdMedia, MsdMediaDigest, MsdMediaKind,
    MsdMediaReference, MsdMediaSource, MsdMediaStore, MsdMediaUpload, MsdMode, MsdOccupancy,
    MsdOccupancyState, MsdOwnedMount, MsdStatusSnapshot, MsdStorageLocation, MsdStorageOperation,
    MsdStoredImage,
};
use crate::bmc::feature::{acquire_connected_feature, feature_connect_status, retry_feature};
use crate::bmc::feature::{build_feature_instances_status, build_feature_status};
use crate::bmc::msd_storage::MsdStorageProvider;
use crate::web::state::{AppState, MsdDownloadStatus, MsdDownloadTask};

/// Local media is streamed to disk, so this is a resource policy rather than
/// a process-memory bound. Remote downloads retain their smaller independent
/// limit until that transport is streaming too.
pub(super) const DEFAULT_MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_ACTIVE_REMOTE_DOWNLOADS: usize = 4;
const MAX_DOWNLOAD_STATUS_RECORDS: usize = 64;
const DOWNLOAD_CHECKPOINT_INTERVAL_BYTES: u64 = 8 * 1024 * 1024;
const DOWNLOAD_CHECKPOINT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const DOWNLOAD_HEADERS_ENVELOPE: &str = "x-bmc-download-headers";
const MAX_DOWNLOAD_HEADERS: usize = 32;
const MAX_DOWNLOAD_HEADER_BYTES: usize = 8 * 1024;

fn download_request_headers(
    configured: &std::collections::HashMap<String, String>,
    configured_origins: &[String],
    download_url: &reqwest::Url,
    incoming: &HeaderMap,
) -> Result<HeaderMap, &'static str> {
    let mut values = std::collections::HashMap::new();
    if !configured.is_empty() {
        let origin = download_url.origin().ascii_serialization();
        let allowed = configured_origins.iter().any(|candidate| {
            reqwest::Url::parse(candidate)
                .ok()
                .is_some_and(|url| url.origin().ascii_serialization() == origin)
        });
        if !allowed {
            return Err("download_header_origin_not_allowed");
        }
        values.extend(configured.clone());
    }
    if let Some(envelope) = incoming.get(DOWNLOAD_HEADERS_ENVELOPE) {
        let envelope = envelope.to_str().map_err(|_| "invalid_download_headers")?;
        if envelope.len() > MAX_DOWNLOAD_HEADER_BYTES {
            return Err("download_headers_too_large");
        }
        let supplied: std::collections::HashMap<String, String> =
            serde_json::from_str(envelope).map_err(|_| "invalid_download_headers")?;
        values.extend(supplied);
    }
    if values.len() > MAX_DOWNLOAD_HEADERS
        || values
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>()
            > MAX_DOWNLOAD_HEADER_BYTES
    {
        return Err("download_headers_too_large");
    }
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        let name = HeaderName::try_from(name).map_err(|_| "invalid_download_headers")?;
        if matches!(
            name.as_str(),
            "host" | "content-length" | "transfer-encoding" | "connection" | "range" | "if-range"
        ) {
            return Err("restricted_download_header");
        }
        let value = HeaderValue::try_from(value).map_err(|_| "invalid_download_headers")?;
        headers.insert(name, value);
    }
    Ok(headers)
}

#[derive(Clone, Serialize, Deserialize)]
struct RemoteDownloadCheckpoint {
    id: String,
    device_id: String,
    url: String,
    filename: String,
    offset: u64,
    validator: Option<String>,
    #[serde(default)]
    total_bytes: Option<u64>,
    maximum_bytes: u64,
}

async fn remote_download_paths(
    state: &AppState,
    device_id: &str,
    id: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let staging = state.msd_storage.staging_dir(device_id).await;
    (
        staging.join(format!("download-{id}.part")),
        staging.join(format!("download-{id}.json")),
    )
}

async fn save_download_checkpoint(
    path: &std::path::Path,
    checkpoint: &RemoteDownloadCheckpoint,
) -> Result<(), String> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    tokio::fs::write(
        &temporary,
        serde_json::to_vec(checkpoint).map_err(|error| error.to_string())?,
    )
    .await
    .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|error| error.to_string())?;
    }
    tokio::fs::rename(temporary, path)
        .await
        .map_err(|error| error.to_string())
}

enum DownloadCheckpointError {
    Missing,
    ReadFailed,
    Invalid,
}

impl DownloadCheckpointError {
    fn into_response(self, operation: &'static str) -> Response {
        let (status, code, message, retryable) = match self {
            Self::Missing => (
                StatusCode::NOT_FOUND,
                "download_not_found",
                "The requested media download was not found",
                false,
            ),
            Self::ReadFailed => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "download_checkpoint_read_failed",
                "The download checkpoint could not be read; retry before starting a new download",
                true,
            ),
            Self::Invalid => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "download_checkpoint_invalid",
                "The download checkpoint is invalid; its recovery record has been retained",
                false,
            ),
        };
        static_msd_fault(status, code, message, retryable, operation)
    }
}

async fn load_download_checkpoint(
    state: &AppState,
    device_id: &str,
    id: &str,
) -> Result<RemoteDownloadCheckpoint, DownloadCheckpointError> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(DownloadCheckpointError::Missing);
    }
    let (_, path) = remote_download_paths(state, device_id, id).await;
    let bytes = tokio::fs::read(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            DownloadCheckpointError::Missing
        } else {
            DownloadCheckpointError::ReadFailed
        }
    })?;
    let checkpoint: RemoteDownloadCheckpoint =
        serde_json::from_slice(&bytes).map_err(|_| DownloadCheckpointError::Invalid)?;
    (checkpoint.device_id == device_id && checkpoint.id == id)
        .then_some(checkpoint)
        .ok_or(DownloadCheckpointError::Missing)
}

async fn cleanup_expired_download_checkpoints(state: &AppState, device_id: &str) {
    let directory = state.msd_storage.staging_dir(device_id).await;
    let Ok(mut entries) = tokio::fs::read_dir(&directory).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(id) = name
            .strip_prefix("download-")
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        let expired = entry
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > DOWNLOAD_CHECKPOINT_TTL);
        if expired {
            let (part, checkpoint) = remote_download_paths(state, device_id, id).await;
            let _ = tokio::fs::remove_file(part).await;
            let _ = tokio::fs::remove_file(checkpoint).await;
        }
    }
}

fn checked_transfer_size(current: u64, chunk: usize, maximum: u64) -> Option<u64> {
    current
        .checked_add(chunk as u64)
        .filter(|size| *size <= maximum)
}

fn strong_etag(value: &str) -> Option<&str> {
    let opaque = value.strip_prefix('"')?.strip_suffix('"')?;
    opaque
        .bytes()
        .all(|byte| byte == b'!' || (b'#'..=b'~').contains(&byte))
        .then_some(value)
}

struct DownloadResponsePlan {
    restart: bool,
    validator: Option<String>,
    total_bytes: Option<u64>,
    response_end: Option<u64>,
}

/// Decide whether this response can extend the staged representation. HTTP
/// success alone does not prove that a partial response is a complete image.
fn download_response_plan(
    status: StatusCode,
    headers: &HeaderMap,
    offset: u64,
    validator: Option<&str>,
    previous_total: Option<u64>,
    content_length: Option<u64>,
) -> Result<DownloadResponsePlan, &'static str> {
    let etag = headers
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok());
    if status == StatusCode::OK {
        if headers.contains_key(header::CONTENT_RANGE) {
            return Err("full download response contains a content range");
        }
        return Ok(DownloadResponsePlan {
            restart: offset > 0,
            validator: etag.and_then(strong_etag).map(str::to_owned),
            total_bytes: content_length,
            response_end: content_length,
        });
    }
    let validator = validator
        .and_then(strong_etag)
        .filter(|_| status == StatusCode::PARTIAL_CONTENT && offset > 0)
        .ok_or("download response does not identify a complete representation")?;
    // A 206 response to If-Range can omit ETag. An explicitly contradictory
    // validator must never authorize combining bytes from different versions.
    if headers.contains_key(header::ETAG) && etag != Some(validator) {
        return Err("download representation changed during resume");
    }
    let range = headers
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("bytes "))
        .ok_or("download range is missing or invalid")?;
    let (interval, total) = range.split_once('/').ok_or("invalid download range")?;
    let (start, end) = interval.split_once('-').ok_or("invalid download range")?;
    let decimal = |value: &str| {
        (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse::<u64>().ok())
            .flatten()
    };
    let (start, end, total) = (decimal(start), decimal(end), decimal(total));
    let (Some(start), Some(end), Some(total)) = (start, end, total) else {
        return Err("invalid download range");
    };
    if start != offset
        || start > end
        || end >= total
        || previous_total.is_some_and(|previous| previous != total)
        || content_length.is_some_and(|length| length != end - start + 1)
    {
        return Err("download range contradicts the staged representation");
    }
    Ok(DownloadResponsePlan {
        restart: false,
        validator: Some(validator.to_owned()),
        total_bytes: Some(total),
        response_end: Some(end + 1),
    })
}

fn upload_too_large_fault() -> FeatureFault {
    FeatureFault::operation(
        FeatureFaultDomain::Storage,
        "upload_too_large",
        "The media image exceeds the upload size limit",
        false,
        "upload_image",
    )
}

pub(super) async fn max_image_bytes(state: &AppState, device_id: &str) -> u64 {
    state
        .config
        .read()
        .await
        .devices
        .iter()
        .find(|device| device.id == device_id)
        .and_then(|device| device.feature_u64_param("msd", "max_image_bytes"))
        .filter(|limit| *limit > 0)
        .unwrap_or(DEFAULT_MAX_IMAGE_BYTES)
}

fn is_private_download_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || octets[0] >= 224
        }
        IpAddr::V6(ip) => {
            ip.to_ipv4_mapped()
                .is_some_and(|ipv4| is_private_download_address(IpAddr::V4(ipv4)))
                || ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
        }
    }
}

#[derive(Clone, Debug)]
struct ValidatedDownloadTarget {
    url: reqwest::Url,
    resolved_host: Option<String>,
    resolved_addresses: Vec<SocketAddr>,
}

impl ValidatedDownloadTarget {
    fn client(&self, connect_timeout: Duration) -> Result<reqwest::Client, String> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(connect_timeout);
        if let Some(host) = &self.resolved_host {
            builder = builder.resolve_to_addrs(host, &self.resolved_addresses);
        }
        builder
            .build()
            .map_err(|error| format!("download client: {error}"))
    }
}

async fn validate_download_url(
    raw: &str,
    allow_private: bool,
) -> Result<ValidatedDownloadTarget, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "invalid download URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("download URL must use http or https".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("download URL must not contain credentials".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "download URL has no host".to_string())?;
    if !allow_private
        && (host.eq_ignore_ascii_case("localhost")
            || host.to_ascii_lowercase().ends_with(".localhost"))
    {
        return Err("private download host is not allowed".to_string());
    }
    if let Ok(address) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        if !allow_private && is_private_download_address(address) {
            return Err("private download host is not allowed".to_string());
        }
        let port = url.port_or_known_default().unwrap_or(80);
        return Ok(ValidatedDownloadTarget {
            url,
            resolved_host: None,
            resolved_addresses: vec![SocketAddr::new(address, port)],
        });
    }

    let port = url.port_or_known_default().unwrap_or(80);
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "download host could not be resolved".to_string())?
        .collect();
    if addresses.is_empty()
        || (!allow_private
            && addresses
                .iter()
                .any(|address| is_private_download_address(address.ip())))
    {
        return Err("private download host is not allowed".to_string());
    }
    let resolved_host = host.to_string();
    Ok(ValidatedDownloadTarget {
        url,
        resolved_host: Some(resolved_host),
        resolved_addresses: addresses,
    })
}

/// Redfish VirtualMedia makes the BMC retrieve this URL, not the aggregator.
/// Require an explicit HTTPS URL without embedded credentials; the URL is
/// still operator-controlled and never fetched by this process.
fn validate_remote_media_url(
    raw: &str,
    media_type: Option<MsdMediaKind>,
) -> Result<MsdMedia, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "invalid remote media URL".to_string())?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err("remote media URL must be an absolute HTTPS URL".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("remote media URL must not contain credentials".to_string());
    }
    Ok(MsdMedia {
        source: MsdMediaSource::RemoteUrl { url: url.into() },
        media_type,
    })
}

fn public_remote_media_url(raw: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(raw).map_err(|_| "invalid remote media URL".to_string())?;
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

#[derive(Deserialize)]
pub struct MsdConnect {
    slot_id: String,
    mode: MsdMode,
    #[serde(default)]
    source: Option<MsdMediaReference>,
    #[serde(default)]
    media_type: Option<MsdMediaKind>,
    #[serde(default)]
    mutation_id: Option<String>,
}

#[derive(Deserialize)]
pub struct MsdSetImage {
    slot_id: String,
    source: MsdMediaReference,
    #[serde(default)]
    media_type: Option<MsdMediaKind>,
    #[serde(default)]
    mutation_id: Option<String>,
}

async fn resolve_media_source(
    state: &Arc<AppState>,
    bmc_id: &str,
    source: MsdMediaReference,
    media_type: Option<MsdMediaKind>,
) -> Result<(MsdMediaReference, MsdMedia), (StatusCode, &'static str, &'static str)> {
    match source {
        MsdMediaReference::Aggregator { image_id } => {
            match resolve_managed_image(state, bmc_id, &image_id).await {
                Ok((id, path, size, file)) => {
                    let reference = MsdMediaReference::Aggregator { image_id: id };
                    Ok((
                        reference,
                        MsdMedia {
                            source: MsdMediaSource::AggregatorImage { path, size, file },
                            media_type,
                        },
                    ))
                }
                Err((status, _)) => Err((
                    status,
                    "image_unavailable",
                    "The selected aggregator media image is unavailable",
                )),
            }
        }
        MsdMediaReference::Bmc { image_id } => {
            let image_id = image_id.trim().to_string();
            if image_id.is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "image_required",
                    "The selected BMC media image is invalid",
                ));
            }
            Ok((
                MsdMediaReference::Bmc {
                    image_id: image_id.clone(),
                },
                MsdMedia {
                    source: MsdMediaSource::BmcImage { id: image_id },
                    media_type,
                },
            ))
        }
        MsdMediaReference::Remote { url } => {
            let media = validate_remote_media_url(&url, media_type).map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    "invalid_remote_media_url",
                    "The remote media URL is invalid",
                )
            })?;
            let MsdMediaSource::RemoteUrl { url } = &media.source else {
                unreachable!()
            };
            let public_url = public_remote_media_url(url).map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    "invalid_remote_media_url",
                    "The remote media URL is invalid",
                )
            })?;
            Ok((MsdMediaReference::Remote { url: public_url }, media))
        }
    }
}

fn normalize_media_type(
    capabilities: &MsdCapabilities,
    slot_id: &str,
    source: &MsdMediaReference,
    requested: Option<MsdMediaKind>,
) -> Result<Option<MsdMediaKind>, (&'static str, &'static str)> {
    let Some(storage) = capabilities.storage(source.location()) else {
        return Err((
            "storage_location_unsupported",
            "The MSD backend does not support the selected storage location",
        ));
    };
    if !storage.supports(MsdStorageOperation::Select) {
        return Err((
            "storage_selection_unsupported",
            "The MSD backend cannot select media from this storage location",
        ));
    }
    let Some(slot) = capabilities.slots.iter().find(|slot| slot.id == slot_id) else {
        return Err(("slot_unsupported", "The selected MSD slot is unavailable"));
    };
    let supported: Vec<_> = storage
        .media_types
        .iter()
        .copied()
        .filter(|media_type| slot.media_types.contains(media_type))
        .collect();
    match (supported.as_slice(), requested) {
        ([], None) => Ok(None),
        ([], Some(_)) => Err((
            "media_type_unsupported",
            "This storage location uses an implicit media type",
        )),
        ([only], None) => Ok(Some(*only)),
        (supported, Some(media_type)) if supported.contains(&media_type) => Ok(Some(media_type)),
        ([_, ..], None) => Err((
            "media_type_required",
            "This storage location requires an explicit media type",
        )),
        (_, Some(_)) => Err((
            "media_type_unsupported",
            "The selected media type is not supported by this slot and storage location",
        )),
    }
}

#[derive(Deserialize)]
pub struct MsdDeleteImage {
    location: MsdStorageLocation,
    image_id: String,
}

#[derive(Deserialize)]
pub struct MsdStorageQuery {
    location: MsdStorageLocation,
    #[serde(default)]
    media_type: Option<MsdMediaKind>,
}

#[derive(Deserialize)]
pub struct MsdDriveMkdir {
    path: String,
}

#[derive(Deserialize)]
pub struct MsdDriveInit {
    size_mb: u64,
    #[serde(default)]
    mutation_id: Option<String>,
}

fn json_ok(msg: &str) -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "message": msg}))
}

fn storage_status(error: &std::io::Error) -> StatusCode {
    match error.kind() {
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::NotFound => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn storage_fault(error: &std::io::Error, operation: &'static str) -> FeatureFault {
    let status = storage_status(error);
    let (code, message, retryable) = if status == StatusCode::BAD_REQUEST {
        (
            "storage_request_invalid",
            "The requested storage operation is invalid",
            false,
        )
    } else {
        (
            "storage_operation_failed",
            "The media storage operation failed",
            true,
        )
    };
    FeatureFault::operation(
        FeatureFaultDomain::Storage,
        code,
        message,
        retryable,
        operation,
    )
}

async fn get_backend(
    fc: &Arc<crate::bmc::feature::connection::FeatureConnection>,
) -> Option<Arc<dyn MsdBackend>> {
    fc.backend_runtime::<Arc<dyn MsdBackend>>().await
}

fn static_msd_fault(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
    operation: &'static str,
) -> Response {
    fault_response(
        status,
        FeatureFault::operation(
            FeatureFaultDomain::Storage,
            code,
            message,
            retryable,
            operation,
        ),
    )
}

async fn msd_fault_response(
    state: &Arc<AppState>,
    bmc_id: &str,
    status: StatusCode,
    fault: FeatureFault,
) -> Response {
    if let Some(dev) = state.registry.get_dev(bmc_id).await {
        if let Some(fc) = dev.features.get("msd") {
            fc.faults.record(fault.clone()).await;
        }
    }
    fault_response(status, fault)
}

async fn backend_unavailable(
    fc: &crate::bmc::feature::connection::FeatureConnection,
    operation: &'static str,
) -> Response {
    record_fault_response(
        fc,
        StatusCode::SERVICE_UNAVAILABLE,
        FeatureFault::operation(
            FeatureFaultDomain::Lifecycle,
            "backend_runtime_unavailable",
            "The MSD backend runtime is not available",
            true,
            operation,
        ),
    )
    .await
}

pub(super) struct MsdStorageGuard {
    _feature: FeatureGuard,
    _backend: tokio::sync::OwnedMutexGuard<()>,
}

pub(super) async fn backend_media_store(
    state: &Arc<AppState>,
    bmc_id: &str,
    location: MsdStorageLocation,
    required_operation: MsdStorageOperation,
    operation: &'static str,
) -> Result<
    (
        MsdStorageGuard,
        Arc<FeatureConnection>,
        Arc<dyn MsdMediaStore>,
    ),
    Response,
> {
    let Some(dev) = state.registry.get_dev(bmc_id).await else {
        return Err(unavailable_fault_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "msd",
        ));
    };
    let Some(fc) = dev.features.get("msd").cloned() else {
        return Err(unavailable_fault_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "msd",
        ));
    };
    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => guard,
        Err(error) => {
            return Err(
                acquisition_fault_response(&fc, feature_connect_status(&error), &error).await,
            )
        }
    };
    let generation = fc.backend_generation();
    let backend_guard = fc
        .backend_owned_effect_guard(generation)
        .await
        .map_err(|_| unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "msd"))?;
    let Some(backend) = get_backend(&fc).await else {
        return Err(backend_unavailable(&fc, operation).await);
    };
    let supported = backend
        .capabilities()
        .await
        .storage(location)
        .is_some_and(|storage| storage.supports(required_operation));
    if !supported {
        return Err(static_msd_fault(
            StatusCode::BAD_REQUEST,
            "storage_operation_unsupported",
            "The MSD backend does not support this storage operation",
            false,
            operation,
        ));
    }
    let Some(store) = backend.media_store(location) else {
        return Err(record_fault_response(
            &fc,
            StatusCode::INTERNAL_SERVER_ERROR,
            FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "backend_storage_contract_invalid",
                "The MSD backend advertised a storage operation without an implementation",
                false,
                operation,
            ),
        )
        .await);
    };
    Ok((
        MsdStorageGuard {
            _feature: guard,
            _backend: backend_guard,
        },
        fc,
        store,
    ))
}

async fn resolve_managed_image(
    state: &Arc<AppState>,
    bmc_id: &str,
    image_id: &str,
) -> Result<(String, String, u64, Arc<std::fs::File>), (StatusCode, String)> {
    let image_id = image_id.trim_start_matches('/');
    if image_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "missing image id".to_string()));
    }
    state
        .msd_storage
        .ensure_dirs(bmc_id)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    match state.msd_storage.image_file(bmc_id, image_id).await {
        Ok(Some((path, size, file))) => Ok((
            image_id.to_string(),
            path.to_string_lossy().to_string(),
            size,
            file,
        )),
        Ok(None) => Err((StatusCode::BAD_REQUEST, "image not found".to_string())),
        Err(error) => Err((StatusCode::BAD_REQUEST, error)),
    }
}

#[derive(Default, Deserialize)]
pub struct MsdStatusQuery {
    #[serde(default)]
    discover: bool,
}

fn msd_occupancy(attachments: &[MsdAttachment], fault: Option<&FeatureFault>) -> MsdOccupancy {
    if !attachments.is_empty() {
        return MsdOccupancy {
            state: MsdOccupancyState::Managed,
            details_available: true,
            active_sessions: None,
        };
    }
    if fault.is_some_and(|fault| fault.code == "session_in_use") {
        return MsdOccupancy {
            state: MsdOccupancyState::External,
            details_available: false,
            active_sessions: None,
        };
    }
    MsdOccupancy {
        state: MsdOccupancyState::Idle,
        details_available: true,
        active_sessions: Some(0),
    }
}

async fn msd_discovery_timeout(fc: &FeatureConnection) -> Duration {
    if fc
        .faults
        .active()
        .await
        .is_some_and(|fault| fault.code == "session_in_use")
    {
        // A busy AVMP peer rejects login immediately. Keep subsequent probes
        // short so status requests do not queue behind the normal discovery
        // budget while another client owns the session.
        Duration::from_secs(3)
    } else {
        Duration::from_secs(15)
    }
}

fn merge_attachment_snapshot(
    runtime: &BTreeMap<String, MsdAttachment>,
    backend: Option<Vec<MsdAttachment>>,
) -> Vec<MsdAttachment> {
    let Some(backend) = backend else {
        return runtime.values().cloned().collect();
    };
    let mut merged: BTreeMap<_, _> = backend
        .into_iter()
        .map(|attachment| (attachment.slot_id.clone(), attachment))
        .collect();
    for (slot_id, owned) in runtime {
        if let Some(authoritative) = merged.get_mut(slot_id) {
            if authoritative.media.is_none()
                && (authoritative.media_type.is_none()
                    || owned.media_type.is_none()
                    || authoritative.media_type == owned.media_type)
            {
                authoritative.media = owned.media.clone();
            }
        }
    }
    merged.into_values().collect()
}

async fn reconcile_attachment_snapshot(
    runtime: &Arc<MsdFeatureRuntime>,
    backend: Option<Vec<MsdAttachment>>,
) -> Vec<MsdAttachment> {
    let mut owned_mounts = runtime.owned_mounts.lock().await;
    if let Some(authoritative) = backend.as_ref() {
        let by_slot: BTreeMap<_, _> = authoritative
            .iter()
            .map(|attachment| (attachment.slot_id.as_str(), attachment))
            .collect();
        owned_mounts.retain(|slot_id, owned| {
            by_slot.get(slot_id.as_str()).is_some_and(|actual| {
                actual.mode == owned.attachment.mode
                    && (actual.media_type.is_none()
                        || owned.attachment.media_type.is_none()
                        || actual.media_type == owned.attachment.media_type)
            })
        });
    }
    let owned = owned_mounts
        .iter()
        .map(|(slot_id, mount)| (slot_id.clone(), mount.attachment.clone()))
        .collect();
    merge_attachment_snapshot(&owned, backend)
}

async fn read_msd_status_snapshot(
    fc: &Arc<FeatureConnection>,
    generation: u64,
    runtime: &Arc<MsdFeatureRuntime>,
    backend: &Arc<dyn MsdBackend>,
) -> Result<MsdStatusSnapshot, String> {
    let backend_attachments = backend.attachment_snapshot().await?;
    let capabilities = backend.capabilities().await;
    let backend_status = backend.status().await.unwrap_or(serde_json::Value::Null);
    let _backend = fc.backend_commit_guard(generation).await?;
    let attachments = reconcile_attachment_snapshot(runtime, backend_attachments).await;
    Ok(MsdStatusSnapshot {
        capabilities,
        attachments,
        backend_status,
    })
}

/// Publish an authoritative backend-originated MSD status change.
///
/// Protocol adapters call this when the BMC reports an external mutation, so
/// every subscribed client receives the same revisioned resource update.
pub(crate) async fn publish_backend_status(
    fc: &Arc<FeatureConnection>,
    backend_generation: u64,
    cause: &'static str,
) {
    let Some(backend) = get_backend(fc).await else {
        return;
    };
    let runtime = msd_feature_runtime(fc).await;
    let _ = runtime
        .status
        .update_at(fc, backend_generation, cause, None, || async {
            read_msd_status_snapshot(fc, backend_generation, &runtime, &backend).await
        })
        .await;
}

async fn read_msd_status_after_mutation(
    runtime: &Arc<MsdFeatureRuntime>,
    backend: &Arc<dyn MsdBackend>,
) -> MsdStatusSnapshot {
    let backend_attachments = backend.attachment_snapshot().await.ok().flatten();
    let attachments = reconcile_attachment_snapshot(runtime, backend_attachments).await;
    MsdStatusSnapshot {
        capabilities: backend.capabilities().await,
        attachments,
        backend_status: backend.status().await.unwrap_or(serde_json::Value::Null),
    }
}

async fn target_is_mounted(
    runtime: &Arc<MsdFeatureRuntime>,
    backend: &Arc<dyn MsdBackend>,
    slot_id: &str,
) -> Result<bool, String> {
    let snapshot = backend.attachment_snapshot().await?;
    Ok(reconcile_attachment_snapshot(runtime, snapshot)
        .await
        .iter()
        .any(|attachment| attachment.slot_id == slot_id))
}

fn target_mounted_response(operation: &'static str) -> Response {
    static_msd_fault(
        StatusCode::CONFLICT,
        "target_mounted",
        "Mounted media must be disconnected before mounting another image",
        false,
        operation,
    )
}

async fn target_occupancy_fault(fc: &Arc<FeatureConnection>, operation: &'static str) -> Response {
    record_fault_response(
        fc,
        StatusCode::BAD_GATEWAY,
        FeatureFault::operation(
            FeatureFaultDomain::Operation,
            "attachment_status_failed",
            "The MSD backend could not determine whether the target is already mounted",
            true,
            operation,
        ),
    )
    .await
}

pub async fn msd_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Query(params): Query<MsdStatusQuery>,
) -> Json<serde_json::Value> {
    let (capacity, used, free, _mb) = state.msd_storage.drive_info(&bmc_id).await;

    if let Some(dev) = state.registry.get_dev(&bmc_id).await {
        if let Some(fc) = dev.features.get("msd") {
            let feature_runtime = msd_feature_runtime(fc).await;
            let _target_lifecycle = feature_runtime.target_lifecycle.lock().await;
            // Dynamic backends cannot publish a truthful slot topology until
            // they have authenticated and read the BMC's drive table. Only an
            // explicit UI discovery request acquires the feature; ordinary
            // status polling remains passive and does not pin AVMP sessions.
            let _discovery_guard = if params.discover {
                let timeout = msd_discovery_timeout(fc).await;
                match acquire_connected_feature(fc.clone(), timeout).await {
                    Ok(guard) => {
                        if let Some(backend) = get_backend(fc).await {
                            if let Err(error) = backend.refresh_capabilities(None).await {
                                warn!("MSD capability discovery failed: {error}");
                                fc.faults
                                    .record(FeatureFault::operation(
                                        FeatureFaultDomain::Operation,
                                        "capability_discovery_failed",
                                        "The MSD backend could not discover media capabilities",
                                        true,
                                        "status",
                                    ))
                                    .await;
                            }
                        }
                        Some(guard)
                    }
                    Err(error) => {
                        warn!("MSD capability discovery could not connect: {error}");
                        None
                    }
                }
            } else {
                None
            };
            let backend_generation = fc.backend_generation();
            let status_snapshot = match fc.backend_runtime::<Arc<dyn MsdBackend>>().await {
                Some(backend) => match feature_runtime
                    .status
                    .refresh_at(fc, backend_generation, || async {
                        read_msd_status_snapshot(fc, backend_generation, &feature_runtime, &backend)
                            .await
                    })
                    .await
                {
                    Ok(snapshot) => snapshot,
                    Err(_) => {
                        fc.faults
                            .record(FeatureFault::operation(
                                FeatureFaultDomain::Operation,
                                "status_failed",
                                "The MSD backend could not read its media status",
                                true,
                                "status",
                            ))
                            .await;
                        feature_runtime.status.snapshot().await
                    }
                },
                None => feature_runtime.status.snapshot().await,
            };
            let stale = status_snapshot.value.is_none();
            let status = status_snapshot.value.unwrap_or(MsdStatusSnapshot {
                capabilities: MsdCapabilities::default(),
                attachments: Vec::new(),
                backend_status: serde_json::Value::Null,
            });
            let mut base = build_feature_status(fc).await;
            if let serde_json::Value::Object(ref mut map) = base {
                let fault = fc.faults.active().await;
                map.insert(
                    "capabilities".into(),
                    serde_json::json!(status.capabilities),
                );
                map.insert("backend_status".into(), status.backend_status);
                map.insert("attachments".into(), serde_json::json!(status.attachments));
                map.insert(
                    "occupancy".into(),
                    serde_json::json!(msd_occupancy(&status.attachments, fault.as_ref())),
                );
                map.insert(
                    "resource".into(),
                    serde_json::json!({
                        "epoch": fc.data_epoch,
                        "revision": status_snapshot.revision,
                        "stale": stale,
                    }),
                );
                map.insert(
                    "drive".into(),
                    serde_json::json!({
                        "capacity": capacity,
                        "used": used,
                        "free": free,
                    }),
                );
                let (instances, active_instances) =
                    build_feature_instances_status(&dev, "msd").await;
                map.insert("instances".into(), serde_json::json!(instances));
                map.insert(
                    "active_instances".into(),
                    serde_json::json!(active_instances),
                );
            }
            return Json(base);
        }
    }
    Json(serde_json::json!({
        "available": false,
        "online": false,
        "attachments": Vec::<MsdAttachment>::new(),
        "occupancy": MsdOccupancy::default(),
        "backend_status": serde_json::Value::Null,
        "capabilities": MsdCapabilities::default(),
        "drive": {
            "capacity": capacity,
            "used": used,
            "free": free,
        },
    }))
}

pub async fn msd_connect(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<MsdConnect>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        let mode = params.mode;
        if mode != MsdMode::Image {
            return static_msd_fault(
                StatusCode::BAD_REQUEST,
                "invalid_mode",
                "This backend contract mounts explicit media images",
                false,
                "connect",
            );
        }
        let selected_source = match params.source {
            Some(source) => Some(source),
            None => {
                return static_msd_fault(
                    StatusCode::BAD_REQUEST,
                    "image_required",
                    "Image mode requires an explicit media source",
                    false,
                    "connect",
                )
            }
        };

        let Some(dev) = state.registry.get_dev(&bmc_id).await else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "msd");
        };
        let Some(fc) = dev.features.get("msd").cloned() else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "msd");
        };
        let feature_runtime = msd_feature_runtime(&fc).await;
        let _target_lifecycle = feature_runtime.target_lifecycle.lock().await;
        let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
            Ok(guard) => guard,
            Err(error) => {
                return acquisition_fault_response(&fc, feature_connect_status(&error), &error)
                    .await
            }
        };
        let backend_generation = fc.backend_generation();
        let Ok(_backend_effect) = fc.backend_effect_guard(backend_generation).await else {
            return backend_unavailable(&fc, "mutation").await;
        };
        let Some(backend) = get_backend(&fc).await else {
            return backend_unavailable(&fc, "connect").await;
        };
        match target_is_mounted(&feature_runtime, &backend, &params.slot_id).await {
            Ok(true) => return target_mounted_response("connect"),
            Ok(false) => {}
            Err(_) => return target_occupancy_fault(&fc, "connect").await,
        }
        if let Err(error) = backend.refresh_capabilities(params.media_type).await {
            warn!("MSD capability refresh failed: {error}");
            return record_fault_response(
                &fc,
                StatusCode::BAD_GATEWAY,
                FeatureFault::operation(
                    FeatureFaultDomain::Operation,
                    "capability_refresh_failed",
                    "The MSD backend could not refresh media capabilities",
                    true,
                    "connect",
                ),
            )
            .await;
        }
        let selected_media = if let Some(source) = selected_source {
            let media_type = match normalize_media_type(
                &backend.capabilities().await,
                &params.slot_id,
                &source,
                params.media_type,
            ) {
                Ok(media_type) => media_type,
                Err((code, message)) => {
                    return static_msd_fault(
                        StatusCode::BAD_REQUEST,
                        code,
                        message,
                        false,
                        "connect",
                    )
                }
            };
            match resolve_media_source(&state, &bmc_id, source, media_type).await {
                Ok(media) => Some(media),
                Err((status, code, message)) => {
                    return static_msd_fault(
                        status,
                        code,
                        message,
                        status != StatusCode::BAD_REQUEST,
                        "connect",
                    )
                }
            }
        } else {
            None
        };
        if let Some((reference, image)) = selected_media {
            let media_type = image.media_type;
            if backend.validate_media(&image).await.is_err() {
                return static_msd_fault(
                    StatusCode::BAD_REQUEST,
                    "invalid_media_image",
                    "The selected media image is not valid for this backend target",
                    false,
                    "connect",
                );
            }
            if backend.mount(&params.slot_id, image).await.is_err() {
                return record_fault_response(
                    &fc,
                    StatusCode::BAD_GATEWAY,
                    FeatureFault::operation(
                        FeatureFaultDomain::Operation,
                        "mount_failed",
                        "The MSD backend could not mount the requested image",
                        true,
                        "connect",
                    ),
                )
                .await;
            }
            feature_runtime.owned_mounts.lock().await.insert(
                params.slot_id.clone(),
                MsdOwnedMount {
                    guard,
                    attachment: MsdAttachment {
                        slot_id: params.slot_id.clone(),
                        mode,
                        media: Some(reference),
                        media_type,
                    },
                },
            );
            let revision = feature_runtime
                .status
                .commit_at(
                    &fc,
                    backend_generation,
                    read_msd_status_after_mutation(&feature_runtime, &backend).await,
                    "mutation",
                    params.mutation_id.clone(),
                )
                .await
                .ok()
                .map(|snapshot| snapshot.revision);
            fc.faults.clear_retryable().await;
            return Json(serde_json::json!({
                "ok": true,
                "success": true,
                "message": "connected",
                "mutation_id": params.mutation_id,
                "revision": revision,
            }))
            .into_response();
        }
        json_ok("connected").into_response()
    }))
    .await
}

#[derive(Deserialize)]
pub struct MsdDisconnect {
    slot_id: String,
    #[serde(default)]
    mutation_id: Option<String>,
}

pub async fn msd_disconnect(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<MsdDisconnect>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        let Some(dev) = state.registry.get_dev(&bmc_id).await else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "msd");
        };
        let Some(fc) = dev.features.get("msd").cloned() else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "msd");
        };
        let feature_runtime = msd_feature_runtime(&fc).await;
        let _target_lifecycle = feature_runtime.target_lifecycle.lock().await;
        let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
            Ok(guard) => guard,
            Err(error) => {
                return acquisition_fault_response(&fc, feature_connect_status(&error), &error)
                    .await
            }
        };
        let backend_generation = fc.backend_generation();
        let Ok(_backend_effect) = fc.backend_effect_guard(backend_generation).await else {
            return backend_unavailable(&fc, "mutation").await;
        };
        let Some(backend) = get_backend(&fc).await else {
            return backend_unavailable(&fc, "disconnect").await;
        };
        let result = backend.unmount(&params.slot_id).await;
        match result {
            Ok(()) => {
                feature_runtime
                    .owned_mounts
                    .lock()
                    .await
                    .remove(&params.slot_id);
                let revision = feature_runtime
                    .status
                    .commit_at(
                        &fc,
                        backend_generation,
                        read_msd_status_after_mutation(&feature_runtime, &backend).await,
                        "mutation",
                        params.mutation_id.clone(),
                    )
                    .await
                    .ok()
                    .map(|snapshot| snapshot.revision);
                drop(guard);
                fc.faults.clear_retryable().await;
                Json(serde_json::json!({
                    "ok": true,
                    "success": true,
                    "message": "disconnected",
                    "mutation_id": params.mutation_id,
                    "revision": revision,
                }))
                .into_response()
            }
            Err(_) => {
                drop(guard);
                record_fault_response(
                    &fc,
                    StatusCode::BAD_GATEWAY,
                    FeatureFault::operation(
                        FeatureFaultDomain::Operation,
                        "disconnect_failed",
                        "The MSD backend could not disconnect the current media",
                        true,
                        "disconnect",
                    ),
                )
                .await
            }
        }
    }))
    .await
}

#[derive(Deserialize)]
pub struct MsdTargetCreate {
    media_type: MsdMediaKind,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    mutation_id: Option<String>,
}

pub async fn msd_target_create(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<MsdTargetCreate>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        let Some(dev) = state.registry.get_dev(&bmc_id).await else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "msd");
        };
        let Some(fc) = dev.features.get("msd").cloned() else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "msd");
        };
        let feature_runtime = msd_feature_runtime(&fc).await;
        let _target_lifecycle = feature_runtime.target_lifecycle.lock().await;
        let _guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
            Ok(guard) => guard,
            Err(error) => {
                return acquisition_fault_response(&fc, feature_connect_status(&error), &error)
                    .await
            }
        };
        let backend_generation = fc.backend_generation();
        let Ok(_backend_effect) = fc.backend_effect_guard(backend_generation).await else {
            return backend_unavailable(&fc, "mutation").await;
        };
        let Some(backend) = get_backend(&fc).await else {
            return backend_unavailable(&fc, "create_target").await;
        };
        let capabilities = backend.capabilities().await;
        if !capabilities
            .target_management
            .media_types
            .contains(&params.media_type)
        {
            return static_msd_fault(
                StatusCode::BAD_REQUEST,
                "target_type_unsupported",
                "The backend cannot create the requested media target type",
                false,
                "create_target",
            );
        }
        if capabilities
            .target_management
            .limit_for(params.media_type)
            .is_some_and(|limit| {
                capabilities
                    .slots
                    .iter()
                    .filter(|target| {
                        target.dynamic && target.media_types.contains(&params.media_type)
                    })
                    .count()
                    >= usize::from(limit)
            })
        {
            return static_msd_fault(
                StatusCode::CONFLICT,
                "target_capacity_exhausted",
                "All dynamic targets for this media type are already defined",
                false,
                "create_target",
            );
        }
        match backend.create_target(params.media_type, params.label).await {
            Ok(target) => {
                let revision = feature_runtime
                    .status
                    .commit_at(
                        &fc,
                        backend_generation,
                        read_msd_status_after_mutation(&feature_runtime, &backend).await,
                        "mutation",
                        params.mutation_id.clone(),
                    )
                    .await
                    .ok()
                    .map(|snapshot| snapshot.revision);
                fc.faults.clear_retryable().await;
                Json(serde_json::json!({
                    "ok": true,
                    "target": target,
                    "mutation_id": params.mutation_id,
                    "revision": revision,
                }))
                .into_response()
            }
            Err(_) => {
                record_fault_response(
                    &fc,
                    StatusCode::BAD_GATEWAY,
                    FeatureFault::operation(
                        FeatureFaultDomain::Operation,
                        "target_create_failed",
                        "The MSD backend could not create the requested target",
                        true,
                        "create_target",
                    ),
                )
                .await
            }
        }
    }))
    .await
}

#[derive(Deserialize)]
pub struct MsdTargetDelete {
    target_id: String,
    #[serde(default)]
    mutation_id: Option<String>,
}

pub async fn msd_target_delete(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<MsdTargetDelete>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        let Some(dev) = state.registry.get_dev(&bmc_id).await else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "msd");
        };
        let Some(fc) = dev.features.get("msd").cloned() else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "msd");
        };
        let feature_runtime = msd_feature_runtime(&fc).await;
        let _target_lifecycle = feature_runtime.target_lifecycle.lock().await;
        let _guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
            Ok(guard) => guard,
            Err(error) => {
                return acquisition_fault_response(&fc, feature_connect_status(&error), &error)
                    .await
            }
        };
        let backend_generation = fc.backend_generation();
        let Ok(_backend_effect) = fc.backend_effect_guard(backend_generation).await else {
            return backend_unavailable(&fc, "mutation").await;
        };
        let Some(backend) = get_backend(&fc).await else {
            return backend_unavailable(&fc, "delete_target").await;
        };
        match target_is_mounted(&feature_runtime, &backend, &params.target_id).await {
            Ok(true) => {
                return static_msd_fault(
                    StatusCode::CONFLICT,
                    "target_mounted",
                    "Mounted media must be disconnected before deleting its target",
                    false,
                    "delete_target",
                )
            }
            Ok(false) => {}
            Err(_) => return target_occupancy_fault(&fc, "delete_target").await,
        }
        let capabilities = backend.capabilities().await;
        if !capabilities.target_management.deletable
            || !capabilities
                .slots
                .iter()
                .any(|target| target.id == params.target_id && target.dynamic)
        {
            return static_msd_fault(
                StatusCode::BAD_REQUEST,
                "target_delete_unsupported",
                "The requested target cannot be deleted",
                false,
                "delete_target",
            );
        }
        match backend.delete_target(&params.target_id).await {
            Ok(()) => {
                let revision = feature_runtime
                    .status
                    .commit_at(
                        &fc,
                        backend_generation,
                        read_msd_status_after_mutation(&feature_runtime, &backend).await,
                        "mutation",
                        params.mutation_id.clone(),
                    )
                    .await
                    .ok()
                    .map(|snapshot| snapshot.revision);
                fc.faults.clear_retryable().await;
                Json(serde_json::json!({
                    "ok": true,
                    "success": true,
                    "message": "target deleted",
                    "mutation_id": params.mutation_id,
                    "revision": revision,
                }))
                .into_response()
            }
            Err(_) => {
                record_fault_response(
                    &fc,
                    StatusCode::BAD_GATEWAY,
                    FeatureFault::operation(
                        FeatureFaultDomain::Operation,
                        "target_delete_failed",
                        "The MSD backend could not delete the requested target",
                        true,
                        "delete_target",
                    ),
                )
                .await
            }
        }
    }))
    .await
}

pub async fn msd_retry(Path(bmc_id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    retry_feature(&state, &bmc_id, "msd").await
}

pub async fn msd_set_image(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<MsdSetImage>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        let Some(dev) = state.registry.get_dev(&bmc_id).await else {
            return static_msd_fault(
                StatusCode::NOT_FOUND,
                "device_not_found",
                "The requested BMC device was not found",
                false,
                "set_image",
            );
        };
        let Some(fc) = dev.features.get("msd").cloned() else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "msd");
        };
        let feature_runtime = msd_feature_runtime(&fc).await;
        let _target_lifecycle = feature_runtime.target_lifecycle.lock().await;
        let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
            Ok(guard) => guard,
            Err(error) => {
                return acquisition_fault_response(&fc, feature_connect_status(&error), &error)
                    .await
            }
        };
        let backend_generation = fc.backend_generation();
        let Ok(_backend_effect) = fc.backend_effect_guard(backend_generation).await else {
            return backend_unavailable(&fc, "mutation").await;
        };
        let Some(backend) = get_backend(&fc).await else {
            return backend_unavailable(&fc, "set_image").await;
        };
        match target_is_mounted(&feature_runtime, &backend, &params.slot_id).await {
            Ok(true) => return target_mounted_response("set_image"),
            Ok(false) => {}
            Err(_) => return target_occupancy_fault(&fc, "set_image").await,
        }
        if let Err(error) = backend.refresh_capabilities(params.media_type).await {
            warn!("MSD capability refresh failed: {error}");
            return record_fault_response(
                &fc,
                StatusCode::BAD_GATEWAY,
                FeatureFault::operation(
                    FeatureFaultDomain::Operation,
                    "capability_refresh_failed",
                    "The MSD backend could not refresh media capabilities",
                    true,
                    "set_image",
                ),
            )
            .await;
        }
        let media_type = match normalize_media_type(
            &backend.capabilities().await,
            &params.slot_id,
            &params.source,
            params.media_type,
        ) {
            Ok(media_type) => media_type,
            Err((code, message)) => {
                return static_msd_fault(StatusCode::BAD_REQUEST, code, message, false, "set_image")
            }
        };
        let (reference, image) =
            match resolve_media_source(&state, &bmc_id, params.source, media_type).await {
                Ok(media) => media,
                Err((status, code, message)) => {
                    return static_msd_fault(
                        status,
                        code,
                        message,
                        status != StatusCode::BAD_REQUEST,
                        "set_image",
                    )
                }
            };
        if backend.validate_media(&image).await.is_err() {
            return static_msd_fault(
                StatusCode::BAD_REQUEST,
                "invalid_media_image",
                "The selected media image is not valid for this backend target",
                false,
                "set_image",
            );
        }
        let result = backend.mount(&params.slot_id, image).await;
        match result {
            Ok(()) => {
                feature_runtime.owned_mounts.lock().await.insert(
                    params.slot_id.clone(),
                    MsdOwnedMount {
                        guard,
                        attachment: MsdAttachment {
                            slot_id: params.slot_id.clone(),
                            mode: MsdMode::Image,
                            media: Some(reference),
                            media_type,
                        },
                    },
                );
                let revision = feature_runtime
                    .status
                    .commit_at(
                        &fc,
                        backend_generation,
                        read_msd_status_after_mutation(&feature_runtime, &backend).await,
                        "mutation",
                        params.mutation_id.clone(),
                    )
                    .await
                    .ok()
                    .map(|snapshot| snapshot.revision);
                Json(serde_json::json!({
                    "ok": true,
                    "success": true,
                    "message": "image set",
                    "mutation_id": params.mutation_id,
                    "revision": revision,
                }))
                .into_response()
            }
            Err(_) => {
                drop(guard);
                record_fault_response(
                    &fc,
                    StatusCode::BAD_GATEWAY,
                    FeatureFault::operation(
                        FeatureFaultDomain::Operation,
                        "set_image_failed",
                        "The MSD backend could not select the requested image",
                        true,
                        "set_image",
                    ),
                )
                .await
            }
        }
    }))
    .await
}

fn aggregator_catalog(values: Vec<serde_json::Value>) -> Vec<MsdStoredImage> {
    values
        .into_iter()
        .filter_map(|value| {
            let id = value.get("id")?.as_str()?.to_string();
            Some(MsdStoredImage {
                name: value
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                id,
                size: value.get("size").and_then(serde_json::Value::as_u64),
                deletable: true,
                digests: value
                    .get("digests")
                    .cloned()
                    .and_then(|digests| serde_json::from_value(digests).ok())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

async fn update_catalog<F, Fut>(
    fc: &Arc<FeatureConnection>,
    location: MsdStorageLocation,
    cause: &'static str,
    loader: F,
) -> Result<super::super::data::FeatureResourceSnapshot<Vec<MsdStoredImage>>, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Vec<MsdStoredImage>, String>>,
{
    let runtime = msd_feature_runtime(fc).await;
    let resource = runtime.catalog(location).await;
    if cause == "refresh" {
        resource.refresh(fc, loader).await
    } else {
        resource.update(fc, cause, None, loader).await
    }
}

pub async fn msd_images_list(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<MsdStorageQuery>,
) -> Response {
    let (images, revision, epoch) = if params.location == MsdStorageLocation::Aggregator {
        let fc = state
            .registry
            .get_dev(&bmc_id)
            .await
            .and_then(|dev| dev.features.get("msd").cloned());
        if let Some(fc) = fc {
            let snapshot = update_catalog(&fc, params.location, "refresh", || async {
                Ok(aggregator_catalog(
                    state.msd_storage.list_images(&bmc_id).await,
                ))
            })
            .await;
            match snapshot {
                Ok(snapshot) => (
                    snapshot.value.unwrap_or_default(),
                    snapshot.revision,
                    Some(fc.data_epoch.clone()),
                ),
                Err(_) => (
                    aggregator_catalog(state.msd_storage.list_images(&bmc_id).await),
                    0,
                    Some(fc.data_epoch.clone()),
                ),
            }
        } else {
            (
                aggregator_catalog(state.msd_storage.list_images(&bmc_id).await),
                0,
                None,
            )
        }
    } else {
        let (_guard, fc, store) = match backend_media_store(
            &state,
            &bmc_id,
            params.location,
            MsdStorageOperation::List,
            "list_images",
        )
        .await
        {
            Ok(store) => store,
            Err(response) => return response,
        };
        match update_catalog(&fc, params.location, "refresh", || async {
            let images = store.list().await?;
            Ok(images)
        })
        .await
        {
            Ok(snapshot) => {
                return Json(serde_json::json!({
                    "location": params.location,
                    "images": snapshot.value.unwrap_or_default(),
                    "resource": {"epoch": fc.data_epoch, "revision": snapshot.revision, "stale": false},
                }))
                .into_response();
            }
            Err(_) => {
                return record_fault_response(
                    &fc,
                    StatusCode::BAD_GATEWAY,
                    FeatureFault::operation(
                        FeatureFaultDomain::Storage,
                        "storage_list_failed",
                        "The BMC media catalog could not be listed",
                        true,
                        "list_images",
                    ),
                )
                .await
            }
        }
    };
    Json(serde_json::json!({
        "location": params.location,
        "images": images,
        "resource": {"epoch": epoch, "revision": revision, "stale": false},
    }))
    .into_response()
}

#[allow(clippy::never_loop)] // The multipart contract accepts exactly one image field.
pub async fn msd_images_upload(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<MsdStorageQuery>,
    mut multipart: axum::extract::Multipart,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        let max_image_bytes = max_image_bytes(&state, &bmc_id).await;
        cleanup_expired_download_checkpoints(&state, &bmc_id).await;
        let backend_store = if params.location == MsdStorageLocation::Aggregator {
            let _ = state.msd_storage.ensure_dirs(&bmc_id).await;
            None
        } else {
            match backend_media_store(
                &state,
                &bmc_id,
                params.location,
                MsdStorageOperation::Upload,
                "upload_image",
            )
            .await
            {
                Ok(store) => Some(store),
                Err(response) => return response,
            }
        };
        loop {
            let mut field = match multipart.next_field().await {
                Ok(Some(field)) => field,
                Ok(None) => break,
                Err(_) => {
                    return static_msd_fault(
                        StatusCode::BAD_REQUEST,
                        "invalid_upload",
                        "The media image upload could not be read",
                        false,
                        "upload_image",
                    )
                }
            };
            let filename = crate::bmc::msd_storage::FsMsdStorage::safe_filename(
                field.file_name().unwrap_or("image.iso"),
            );
            if !crate::bmc::msd_storage::FsMsdStorage::is_image_name(&filename) {
                return static_msd_fault(
                    StatusCode::BAD_REQUEST,
                    "invalid_image_name",
                    "Invalid published image name",
                    false,
                    "upload_image",
                );
            }
            let content_type = field.content_type().map(ToString::to_string);
            let staged = match state.msd_storage.create_staged_upload(&bmc_id).await {
                Ok(file) => file,
                Err(_) => {
                    return static_msd_fault(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "upload_staging_failed",
                        "The media image upload could not be staged",
                        true,
                        "upload_image",
                    )
                }
            };
            let staged_path = staged.path().to_path_buf();
            let std_file = match staged.reopen() {
                Ok(file) => file,
                Err(_) => {
                    return static_msd_fault(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "upload_staging_failed",
                        "The media image upload could not be staged",
                        true,
                        "upload_image",
                    )
                }
            };
            let mut output = tokio::fs::File::from_std(std_file);
            let mut size = 0u64;
            let mut md5 = Md5::new();
            let mut sha1 = Sha1::new();
            let mut sha256 = Sha256::new();
            let mut sha512 = Sha512::new();
            loop {
                let chunk = match field.chunk().await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(_) => {
                        return static_msd_fault(
                            StatusCode::BAD_REQUEST,
                            "invalid_upload",
                            "The media image upload could not be read",
                            false,
                            "upload_image",
                        )
                    }
                };
                size = match checked_transfer_size(size, chunk.len(), max_image_bytes) {
                    Some(size) => size,
                    _ => {
                        let fault = upload_too_large_fault();
                        return if let Some((_guard, fc, _store)) = &backend_store {
                            record_fault_response(fc, StatusCode::PAYLOAD_TOO_LARGE, fault).await
                        } else {
                            msd_fault_response(
                                &state,
                                &bmc_id,
                                StatusCode::PAYLOAD_TOO_LARGE,
                                fault,
                            )
                            .await
                        };
                    }
                };
                if output.write_all(&chunk).await.is_err() {
                    return static_msd_fault(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "upload_staging_failed",
                        "The media image upload could not be staged",
                        true,
                        "upload_image",
                    );
                }
                md5.update(&chunk);
                sha1.update(&chunk);
                sha256.update(&chunk);
                sha512.update(&chunk);
            }
            if output.flush().await.is_err() {
                return static_msd_fault(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "upload_staging_failed",
                    "The media image upload could not be staged",
                    true,
                    "upload_image",
                );
            }
            drop(output);
            let digests = vec![
                MsdMediaDigest {
                    algorithm: MsdDigestAlgorithm::Md5,
                    value: format!("{:x}", md5.finalize()),
                },
                MsdMediaDigest {
                    algorithm: MsdDigestAlgorithm::Sha1,
                    value: format!("{:x}", sha1.finalize()),
                },
                MsdMediaDigest {
                    algorithm: MsdDigestAlgorithm::Sha256,
                    value: format!("{:x}", sha256.finalize()),
                },
                MsdMediaDigest {
                    algorithm: MsdDigestAlgorithm::Sha512,
                    value: format!("{:x}", sha512.finalize()),
                },
            ];
            if let Some((_guard, fc, store)) = &backend_store {
                match store
                    .upload(MsdMediaUpload {
                        name: filename,
                        content_type,
                        media_type: params.media_type,
                        path: staged_path,
                        size,
                        digests: digests.clone(),
                    })
                    .await
                {
                    Ok(image) => {
                        let _ = update_catalog(fc, params.location, "mutation", || async {
                            store.list().await
                        })
                        .await;
                        return Json(serde_json::json!(image)).into_response();
                    }
                    Err(_) => {
                        return record_fault_response(
                            fc,
                            StatusCode::BAD_GATEWAY,
                            FeatureFault::operation(
                                FeatureFaultDomain::Storage,
                                "storage_upload_failed",
                                "The image could not be uploaded to BMC storage",
                                true,
                                "upload_image",
                            ),
                        )
                        .await
                    }
                }
            } else {
                match state
                    .msd_storage
                    .publish_image_file(&bmc_id, &filename, &staged_path, size, &digests)
                    .await
                {
                    Ok(size) => {
                        let image = MsdStoredImage {
                            id: filename.clone(),
                            name: filename,
                            size: Some(size),
                            deletable: true,
                            digests: digests.clone(),
                        };
                        if let Some(dev) = state.registry.get_dev(&bmc_id).await {
                            if let Some(fc) = dev.features.get("msd") {
                                let _ = update_catalog(fc, params.location, "mutation", || async {
                                    Ok(aggregator_catalog(
                                        state.msd_storage.list_images(&bmc_id).await,
                                    ))
                                })
                                .await;
                            }
                        }
                        return Json(serde_json::json!({
                            "id": image.id,
                            "name": image.name,
                            "size": image.size,
                            "digests": image.digests,
                        }))
                        .into_response();
                    }
                    Err(_) => {
                        return msd_fault_response(
                            &state,
                            &bmc_id,
                            StatusCode::INTERNAL_SERVER_ERROR,
                            FeatureFault::operation(
                                FeatureFaultDomain::Storage,
                                "image_upload_failed",
                                "The media image could not be stored",
                                true,
                                "upload_image",
                            ),
                        )
                        .await
                    }
                }
            }
        }
        static_msd_fault(
            StatusCode::BAD_REQUEST,
            "file_required",
            "A media image file is required",
            false,
            "upload_image",
        )
    }))
    .await
}

pub async fn msd_images_delete(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<MsdDeleteImage>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        let feature_runtime = match state.registry.get_dev(&bmc_id).await {
            Some(dev) => match dev.features.get("msd") {
                Some(fc) => Some(msd_feature_runtime(fc).await),
                None => None,
            },
            None => None,
        };
        // Keep the in-use check and deletion atomic with mount, unmount and target
        // lifecycle operations. Otherwise a mount can start after this check and
        // lose its backing image while the backend is opening it.
        let _target_lifecycle = match &feature_runtime {
            Some(runtime) => Some(runtime.target_lifecycle.lock().await),
            None => None,
        };
        let owned_attachments = match &feature_runtime {
            Some(runtime) => runtime
                .owned_mounts
                .lock()
                .await
                .values()
                .map(|mount| mount.attachment.clone())
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        let deleting_current_media =
            owned_attachments
                .iter()
                .any(|attachment| match &attachment.media {
                    Some(MsdMediaReference::Aggregator { image_id }) => {
                        params.location == MsdStorageLocation::Aggregator
                            && image_id == &params.image_id
                    }
                    Some(MsdMediaReference::Bmc { image_id }) => {
                        params.location == MsdStorageLocation::Bmc && image_id == &params.image_id
                    }
                    _ => false,
                });
        if deleting_current_media {
            return static_msd_fault(
                StatusCode::CONFLICT,
                "media_in_use",
                "Connected media must be disconnected before it can be deleted",
                false,
                "delete_image",
            );
        }
        if params.location != MsdStorageLocation::Aggregator {
            let (_guard, fc, store) = match backend_media_store(
                &state,
                &bmc_id,
                params.location,
                MsdStorageOperation::Delete,
                "delete_image",
            )
            .await
            {
                Ok(store) => store,
                Err(response) => return response,
            };
            return match store.delete(&params.image_id).await {
                Ok(()) => {
                    let _ = update_catalog(&fc, params.location, "mutation", || async {
                        store.list().await
                    })
                    .await;
                    json_ok("deleted").into_response()
                }
                Err(_) => {
                    record_fault_response(
                        &fc,
                        StatusCode::BAD_GATEWAY,
                        FeatureFault::operation(
                            FeatureFaultDomain::Storage,
                            "storage_delete_failed",
                            "The image could not be deleted from BMC storage",
                            true,
                            "delete_image",
                        ),
                    )
                    .await
                }
            };
        }
        match state
            .msd_storage
            .delete_image(&bmc_id, &params.image_id)
            .await
        {
            Ok(()) => {
                if let Some(dev) = state.registry.get_dev(&bmc_id).await {
                    if let Some(fc) = dev.features.get("msd") {
                        let _ = update_catalog(fc, params.location, "mutation", || async {
                            Ok(aggregator_catalog(
                                state.msd_storage.list_images(&bmc_id).await,
                            ))
                        })
                        .await;
                    }
                }
                json_ok("deleted").into_response()
            }
            Err(error) => {
                let status = storage_status(&error);
                msd_fault_response(
                    &state,
                    &bmc_id,
                    status,
                    storage_fault(&error, "delete_image"),
                )
                .await
            }
        }
    }))
    .await
}

pub async fn msd_images_download(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    // Admission reserves a global slot before staging awaits. Its owner must
    // finish staging/launch or release that slot if the HTTP caller disappears.
    crate::web::state::mutation_response(tokio::spawn(async move {
        start_image_download(bmc_id, state, headers, params).await
    }))
    .await
}

async fn start_image_download(
    bmc_id: String,
    state: Arc<AppState>,
    headers: HeaderMap,
    params: std::collections::HashMap<String, String>,
) -> Response {
    let max_image_bytes = max_image_bytes(&state, &bmc_id).await;
    let resumed = match params.get("resume_id") {
        Some(id) => match load_download_checkpoint(&state, &bmc_id, id).await {
            Ok(checkpoint) => Some(checkpoint),
            Err(error) => return error.into_response("download_image"),
        },
        None => None,
    };
    let url = match (&resumed, params.get("url")) {
        (Some(checkpoint), _) => checkpoint.url.clone(),
        (None, Some(url)) => url.clone(),
        (None, None) => {
            return static_msd_fault(
                StatusCode::BAD_REQUEST,
                "download_url_required",
                "A download URL is required",
                false,
                "download_image",
            )
        }
    };
    if params.get("location").map(String::as_str) != Some("aggregator") {
        return static_msd_fault(
            StatusCode::BAD_REQUEST,
            "storage_location_required",
            "Image downloads require location=aggregator",
            false,
            "download_image",
        );
    }
    let (
        allow_private_downloads,
        configured_headers,
        configured_header_origins,
        connect_timeout,
        idle_timeout,
    ) = {
        let config = state.config.read().await;
        let server = &config.server;
        (
            server.allow_private_downloads,
            server.download_headers.clone(),
            server.download_header_origins.clone(),
            Duration::from_secs(server.download_connect_timeout_seconds.max(1)),
            Duration::from_secs(server.download_idle_timeout_seconds.max(1)),
        )
    };
    let download_target = match validate_download_url(&url, allow_private_downloads).await {
        Ok(target) => target,
        Err(_) => {
            return static_msd_fault(
                StatusCode::BAD_REQUEST,
                "invalid_download_url",
                "The download URL is invalid or not permitted",
                false,
                "download_image",
            )
        }
    };
    let download_headers = match download_request_headers(
        &configured_headers,
        &configured_header_origins,
        &download_target.url,
        &headers,
    ) {
        Ok(headers) => headers,
        Err(code) => {
            return static_msd_fault(
                StatusCode::BAD_REQUEST,
                code,
                "The supplied download request headers are invalid or restricted",
                false,
                "download_image",
            )
        }
    };
    let client = match download_target.client(connect_timeout) {
        Ok(client) => client,
        Err(_) => {
            return static_msd_fault(
                StatusCode::INTERNAL_SERVER_ERROR,
                "download_client_unavailable",
                "The media image download client could not be initialized",
                true,
                "download_image",
            )
        }
    };
    let url = download_target.url.to_string();
    let requested_filename = resumed
        .as_ref()
        .map(|checkpoint| checkpoint.filename.clone())
        .or_else(|| params.get("filename").cloned())
        .unwrap_or_else(|| url.rsplit('/').next().unwrap_or("download.iso").to_string());
    let filename = crate::bmc::msd_storage::FsMsdStorage::safe_filename(&requested_filename);
    if !crate::bmc::msd_storage::FsMsdStorage::is_image_name(&filename) {
        return static_msd_fault(
            StatusCode::BAD_REQUEST,
            "invalid_image_name",
            "Invalid published image name",
            false,
            "download_image",
        );
    }
    if state.msd_storage.ensure_dirs(&bmc_id).await.is_err() {
        return msd_fault_response(
            &state,
            &bmc_id,
            StatusCode::INTERNAL_SERVER_ERROR,
            FeatureFault::operation(
                FeatureFaultDomain::Storage,
                "storage_unavailable",
                "Media storage is unavailable",
                true,
                "download_image",
            ),
        )
        .await;
    }

    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let dl_id = resumed
        .as_ref()
        .map(|checkpoint| checkpoint.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let initial_offset = resumed
        .as_ref()
        .map(|checkpoint| checkpoint.offset)
        .unwrap_or(0);
    {
        let mut downloads = state.active_downloads.write().await;
        if downloads
            .get(&dl_id)
            .is_some_and(|task| !task.status.is_terminal())
        {
            return static_msd_fault(
                StatusCode::CONFLICT,
                "download_already_active",
                "The media image download is already active",
                true,
                "download_image",
            );
        }
        let active = downloads
            .values()
            .filter(|task| !task.status.is_terminal())
            .count();
        if active >= MAX_ACTIVE_REMOTE_DOWNLOADS {
            return static_msd_fault(
                StatusCode::TOO_MANY_REQUESTS,
                "download_limit_reached",
                "Too many media image downloads are active",
                true,
                "download_image",
            );
        }
        if downloads.len() >= MAX_DOWNLOAD_STATUS_RECORDS {
            let terminal_ids: Vec<_> = downloads
                .iter()
                .filter(|(_, task)| task.status.is_terminal())
                .map(|(id, _)| id.clone())
                .collect();
            for id in terminal_ids {
                downloads.remove(&id);
            }
        }
        if downloads.len() >= MAX_DOWNLOAD_STATUS_RECORDS {
            return static_msd_fault(
                StatusCode::TOO_MANY_REQUESTS,
                "download_record_limit_reached",
                "Too many media download records are retained",
                true,
                "download_image",
            );
        }
        downloads.insert(
            dl_id.clone(),
            MsdDownloadTask {
                device_id: bmc_id.clone(),
                cancellation: Some(cancel_tx),
                filename: filename.clone(),
                bytes_downloaded: initial_offset,
                total_bytes: None,
                status: MsdDownloadStatus::Downloading,
                fault: None,
                started_at: std::time::Instant::now(),
            },
        );
    }

    let (staged_path, checkpoint_path) = remote_download_paths(&state, &bmc_id, &dl_id).await;
    if tokio::fs::create_dir_all(state.msd_storage.staging_dir(&bmc_id).await)
        .await
        .is_err()
    {
        state.active_downloads.write().await.remove(&dl_id);
        return static_msd_fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "download_staging_failed",
            "The media image download could not be staged",
            true,
            "download_image",
        );
    }
    let mut checkpoint = resumed.unwrap_or(RemoteDownloadCheckpoint {
        id: dl_id.clone(),
        device_id: bmc_id.clone(),
        url: url.clone(),
        filename: filename.clone(),
        offset: 0,
        validator: None,
        total_bytes: None,
        maximum_bytes: max_image_bytes,
    });
    if checkpoint.offset > max_image_bytes {
        state.active_downloads.write().await.remove(&dl_id);
        return static_msd_fault(
            StatusCode::PAYLOAD_TOO_LARGE,
            "download_too_large",
            "The media image exceeds the download size limit",
            false,
            "download_image",
        );
    }
    checkpoint.maximum_bytes = max_image_bytes;
    if initial_offset == 0
        && (tokio::fs::File::create(&staged_path).await.is_err()
            || save_download_checkpoint(&checkpoint_path, &checkpoint)
                .await
                .is_err())
    {
        state.active_downloads.write().await.remove(&dl_id);
        return static_msd_fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "download_staging_failed",
            "The media image download could not be staged",
            true,
            "download_image",
        );
    }

    let msd_storage = state.msd_storage.clone();
    let bmc_id2 = bmc_id.clone();
    let task_filename = filename.clone();
    let downloads = state.clone();
    let task_download_id = dl_id.clone();

    tokio::spawn(async move {
        let result: Result<(), String> = async {
            let staged_length = tokio::fs::metadata(&staged_path)
                .await
                .map_err(|_| "download_staging_failed".to_string())?
                .len();
            if staged_length < checkpoint.offset {
                return Err("download checkpoint exceeds staged data".to_string());
            }
            if staged_length > checkpoint.offset {
                tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&staged_path)
                    .await
                    .map_err(|_| "download_staging_failed".to_string())?
                    .set_len(checkpoint.offset)
                    .await
                    .map_err(|_| "download_staging_failed".to_string())?;
            }
            let mut md5 = Md5::new();
            let mut sha1 = Sha1::new();
            let mut sha256 = Sha256::new();
            let mut sha512 = Sha512::new();
            if checkpoint.offset > 0
                && checkpoint
                    .validator
                    .as_deref()
                    .and_then(strong_etag)
                    .is_some()
            {
                let mut existing = tokio::fs::File::open(&staged_path)
                    .await
                    .map_err(|_| "download_staging_failed".to_string())?;
                let mut buffer = vec![0u8; 1024 * 1024];
                let mut remaining = checkpoint.offset;
                while remaining > 0 {
                    let requested = remaining.min(buffer.len() as u64) as usize;
                    let count =
                        tokio::io::AsyncReadExt::read(&mut existing, &mut buffer[..requested])
                            .await
                            .map_err(|_| "download_staging_failed".to_string())?;
                    if count == 0 {
                        return Err("download checkpoint data is incomplete".to_string());
                    }
                    md5.update(&buffer[..count]);
                    sha1.update(&buffer[..count]);
                    sha256.update(&buffer[..count]);
                    sha512.update(&buffer[..count]);
                    remaining -= count as u64;
                }
            }
            let mut output = tokio::fs::OpenOptions::new()
                .append(true)
                .open(&staged_path)
                .await
                .map_err(|_| "download_staging_failed".to_string())?;
            let mut bytes_downloaded = checkpoint.offset;
            let mut checkpointed_offset = checkpoint.offset;
            let mut validator = checkpoint.validator.clone();
            let mut completed = false;
            for attempt in 0..=3u32 {
                let mut request = client.get(&url).headers(download_headers.clone());
                if bytes_downloaded > 0 && validator.as_deref().and_then(strong_etag).is_some() {
                    request = request
                        .header(reqwest::header::RANGE, format!("bytes={bytes_downloaded}-"));
                    if let Some(validator) = &validator {
                        request = request.header(reqwest::header::IF_RANGE, validator);
                    }
                }
                let response = tokio::select! {
                    response = tokio::time::timeout(idle_timeout, request.send()) => {
                        match response {
                            Ok(response) => response.map_err(|error| error.to_string()),
                            Err(_) => Err("download_idle_timeout".to_string()),
                        }
                    },
                    _ = &mut cancel_rx => return Err("download cancelled".to_string()),
                };
                let mut response = match response {
                    Ok(response) => response,
                    Err(_) if attempt < 3 => {
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_millis(250u64 << attempt)) => {},
                            _ = &mut cancel_rx => return Err("download cancelled".to_string()),
                        }
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let plan = download_response_plan(
                    response.status(),
                    response.headers(),
                    bytes_downloaded,
                    validator.as_deref(),
                    checkpoint.total_bytes,
                    response.content_length(),
                )?;
                if plan.restart {
                    output
                        .set_len(0)
                        .await
                        .map_err(|_| "download_staging_failed".to_string())?;
                    tokio::io::AsyncSeekExt::rewind(&mut output)
                        .await
                        .map_err(|_| "download_staging_failed".to_string())?;
                    bytes_downloaded = 0;
                    checkpoint.offset = 0;
                    checkpointed_offset = 0;
                    md5 = Md5::new();
                    sha1 = Sha1::new();
                    sha256 = Sha256::new();
                    sha512 = Sha512::new();
                }
                validator = plan.validator;
                checkpoint.validator = validator.clone();
                let total_bytes = plan.total_bytes;
                if total_bytes.is_some_and(|length| length > max_image_bytes) {
                    return Err("download_too_large".to_string());
                }
                checkpoint.total_bytes = total_bytes;
                save_download_checkpoint(&checkpoint_path, &checkpoint).await?;
                if let Some(task) = downloads
                    .active_downloads
                    .write()
                    .await
                    .get_mut(&task_download_id)
                {
                    task.total_bytes = total_bytes;
                }

                let mut interrupted = false;
                loop {
                    let chunk = tokio::select! {
                        chunk = tokio::time::timeout(idle_timeout, response.chunk()) => chunk,
                        _ = &mut cancel_rx => return Err("download cancelled".to_string()),
                    };
                    let chunk = match chunk {
                        Ok(Ok(Some(chunk))) => chunk,
                        Ok(Ok(None)) => break,
                        Ok(Err(_)) | Err(_) if attempt < 3 => {
                            interrupted = true;
                            break;
                        }
                        Ok(Err(error)) => return Err(error.to_string()),
                        Err(_) => return Err("download_idle_timeout".to_string()),
                    };
                    bytes_downloaded =
                        checked_transfer_size(bytes_downloaded, chunk.len(), max_image_bytes)
                            .ok_or_else(|| "download_too_large".to_string())?;
                    if plan.response_end.is_some_and(|end| bytes_downloaded > end) {
                        return Err("download response exceeds its declared range".to_string());
                    }
                    output
                        .write_all(&chunk)
                        .await
                        .map_err(|_| "download_staging_failed".to_string())?;
                    md5.update(&chunk);
                    sha1.update(&chunk);
                    sha256.update(&chunk);
                    sha512.update(&chunk);
                    if let Some(task) = downloads
                        .active_downloads
                        .write()
                        .await
                        .get_mut(&task_download_id)
                    {
                        task.bytes_downloaded = bytes_downloaded;
                    }
                    if bytes_downloaded.saturating_sub(checkpointed_offset)
                        >= DOWNLOAD_CHECKPOINT_INTERVAL_BYTES
                    {
                        output
                            .flush()
                            .await
                            .map_err(|_| "download_staging_failed".to_string())?;
                        checkpoint.offset = bytes_downloaded;
                        save_download_checkpoint(&checkpoint_path, &checkpoint).await?;
                        checkpointed_offset = bytes_downloaded;
                    }
                }
                if !interrupted && total_bytes.is_some_and(|total| bytes_downloaded != total) {
                    if attempt < 3 {
                        interrupted = true;
                    } else {
                        return Err("download ended before the declared length".to_string());
                    }
                }
                if interrupted {
                    output
                        .flush()
                        .await
                        .map_err(|_| "download_staging_failed".to_string())?;
                    checkpoint.offset = bytes_downloaded;
                    save_download_checkpoint(&checkpoint_path, &checkpoint).await?;
                    checkpointed_offset = bytes_downloaded;
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(250u64 << attempt)) => {},
                        _ = &mut cancel_rx => return Err("download cancelled".to_string()),
                    }
                    continue;
                }
                completed = true;
                break;
            }
            if !completed {
                return Err("download retries exhausted".to_string());
            }
            if bytes_downloaded == 0 {
                return Err("downloaded image is empty".to_string());
            }
            output
                .flush()
                .await
                .map_err(|_| "download_staging_failed".to_string())?;
            drop(output);
            checkpoint.offset = bytes_downloaded;
            save_download_checkpoint(&checkpoint_path, &checkpoint).await?;
            let digests = vec![
                MsdMediaDigest {
                    algorithm: MsdDigestAlgorithm::Md5,
                    value: format!("{:x}", md5.finalize()),
                },
                MsdMediaDigest {
                    algorithm: MsdDigestAlgorithm::Sha1,
                    value: format!("{:x}", sha1.finalize()),
                },
                MsdMediaDigest {
                    algorithm: MsdDigestAlgorithm::Sha256,
                    value: format!("{:x}", sha256.finalize()),
                },
                MsdMediaDigest {
                    algorithm: MsdDigestAlgorithm::Sha512,
                    value: format!("{:x}", sha512.finalize()),
                },
            ];
            msd_storage
                .publish_image_file(
                    &bmc_id2,
                    &task_filename,
                    &staged_path,
                    bytes_downloaded,
                    &digests,
                )
                .await
                .map_err(|error| error.to_string())?;
            let _ = tokio::fs::remove_file(&checkpoint_path).await;
            Ok(())
        }
        .await;
        let status = match &result {
            Ok(()) => MsdDownloadStatus::Completed,
            Err(error) if error == "download cancelled" => MsdDownloadStatus::Cancelled,
            Err(_) => MsdDownloadStatus::Failed,
        };
        if result.is_ok() {
            if let Some(dev) = downloads.registry.get_dev(&bmc_id2).await {
                if let Some(fc) = dev.features.get("msd") {
                    let _ =
                        update_catalog(fc, MsdStorageLocation::Aggregator, "mutation", || async {
                            Ok(aggregator_catalog(msd_storage.list_images(&bmc_id2).await))
                        })
                        .await;
                }
            }
        }
        if status == MsdDownloadStatus::Cancelled {
            let _ = tokio::fs::remove_file(&staged_path).await;
            let _ = tokio::fs::remove_file(&checkpoint_path).await;
        }
        if result.is_err() {
            // The source URL can contain signed query parameters, so do not
            // include transport/storage detail in a public process log.
            warn!("MSD image download {task_download_id} failed");
        }
        let failure_fault = result.as_ref().err().and_then(|error| {
            (error != "download cancelled").then(|| {
                if error == "download_too_large" {
                    FeatureFault::child(
                        FeatureFaultDomain::Storage,
                        "download_too_large",
                        "The media image exceeds the download size limit",
                        false,
                        "download",
                        task_download_id.clone(),
                    )
                } else {
                    FeatureFault::child(
                        FeatureFaultDomain::Storage,
                        "download_failed",
                        "The media image download failed",
                        true,
                        "download",
                        task_download_id.clone(),
                    )
                }
            })
        });
        if let Some(fault) = failure_fault.clone() {
            if let Some(dev) = downloads.registry.get_dev(&bmc_id2).await {
                if let Some(fc) = dev.features.get("msd") {
                    fc.faults.record(fault).await;
                }
            }
        }
        if let Some(task) = downloads
            .active_downloads
            .write()
            .await
            .get_mut(&task_download_id)
        {
            task.cancellation = None;
            task.status = status;
            task.fault = failure_fault;
        }
    });

    Json(serde_json::json!({
        "ok": true,
        "download_id": dl_id,
        "filename": filename,
        "status": MsdDownloadStatus::Downloading.as_str(),
    }))
    .into_response()
}

pub async fn msd_images_download_cancel(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let dl_id = params
        .get("download_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // Keep admission excluded through paused-file cleanup. A consumed sender
    // means cancellation was requested, not that the worker released its files.
    let mut downloads = state.active_downloads.write().await;
    let task = downloads
        .get_mut(dl_id)
        .filter(|task| task.device_id == bmc_id);
    let mut cancelled = false;
    if let Some(task) = task.filter(|task| !task.status.is_terminal()) {
        if let Some(tx) = task.cancellation.take() {
            cancelled = tx.send(()).is_ok();
            if cancelled {
                task.status = MsdDownloadStatus::Cancelling;
            }
        }
    } else if load_download_checkpoint(&state, &bmc_id, dl_id)
        .await
        .is_ok()
    {
        let (part, checkpoint) = remote_download_paths(&state, &bmc_id, dl_id).await;
        cancelled = true;
        for path in [part, checkpoint] {
            if let Err(error) = tokio::fs::remove_file(path).await {
                if error.kind() != std::io::ErrorKind::NotFound {
                    cancelled = false;
                    break;
                }
            }
        }
    }
    Json(serde_json::json!({"ok": cancelled}))
}

pub async fn msd_images_download_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let download_id = match params.get("download_id").filter(|id| !id.is_empty()) {
        Some(download_id) => download_id,
        None => {
            return static_msd_fault(
                StatusCode::BAD_REQUEST,
                "download_id_required",
                "A download identifier is required",
                false,
                "download_status",
            )
        }
    };
    {
        let downloads = state.active_downloads.read().await;
        if let Some(task) = downloads
            .get(download_id)
            .filter(|task| task.device_id == bmc_id)
        {
            let elapsed_seconds = task.started_at.elapsed().as_secs_f64();
            let download_speed_bps = if elapsed_seconds > 0.0 {
                (task.bytes_downloaded as f64 / elapsed_seconds) as u64
            } else {
                0
            };
            return Json(serde_json::json!({
                "ok": true,
                "download_id": download_id,
                "filename": task.filename,
                "bytes_downloaded": task.bytes_downloaded,
                "total_bytes": task.total_bytes,
                "download_speed_bps": download_speed_bps,
                "fault": task.fault,
                "status": task.status.as_str(),
            }))
            .into_response();
        }
    }
    match load_download_checkpoint(&state, &bmc_id, download_id).await {
        Ok(checkpoint) => Json(serde_json::json!({
            "ok": true,
            "download_id": download_id,
            "filename": checkpoint.filename,
            "bytes_downloaded": checkpoint.offset,
            "total_bytes": checkpoint.total_bytes,
            "download_speed_bps": 0,
            "fault": serde_json::Value::Null,
            "status": "paused",
        }))
        .into_response(),
        Err(error) => error.into_response("download_status"),
    }
}

pub async fn msd_drive_info(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let (capacity, used, free, _mb) = state.msd_storage.drive_info(&bmc_id).await;
    let base = state.msd_storage.storage_base(&bmc_id).await;
    Json(serde_json::json!({
        "capacity": capacity,
        "used": used,
        "free": free,
        "storage_base": base,
        "supported": true
    }))
}

pub async fn msd_drive_init(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<MsdDriveInit>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        if params.size_mb == 0 {
            return static_msd_fault(
                StatusCode::BAD_REQUEST,
                "invalid_drive_size",
                "The virtual drive size must be greater than zero",
                false,
                "initialize_drive",
            );
        }

        let _transaction = state.configuration.lock().await;
        let mut candidate = state.config.read().await.clone();
        if !candidate.devices.iter().any(|device| device.id == bmc_id) {
            return static_msd_fault(
                StatusCode::NOT_FOUND,
                "device_not_found",
                "The requested BMC device was not found",
                false,
                "initialize_drive",
            );
        }
        let previous_size = candidate
            .msd
            .get(&bmc_id)
            .map(|config| config.drive_size_mb);
        candidate
            .msd
            .entry(bmc_id.clone())
            .or_default()
            .drive_size_mb = params.size_mb;
        let changed = previous_size != Some(params.size_mb);
        let mutation_id = params
            .mutation_id
            .unwrap_or_else(|| format!("msd-drive-init-{}", uuid::Uuid::new_v4()));
        let affected_features = if changed {
            let scope =
                crate::bmc::configuration::ConfigurationScope::Device { id: bmc_id.clone() };
            let report = match state
                .commit_config_scoped(candidate, std::slice::from_ref(&scope))
                .await
            {
                Ok(report) => report,
                Err(_) => {
                    return msd_fault_response(
                        &state,
                        &bmc_id,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        FeatureFault::operation(
                            FeatureFaultDomain::Storage,
                            "configuration_save_failed",
                            "The virtual drive configuration could not be saved",
                            true,
                            "initialize_drive",
                        ),
                    )
                    .await;
                }
            };
            let affected_features = report.features_for(&bmc_id);
            state.configuration.publish(
                scope,
                mutation_id.clone(),
                vec!["msd.drive_size_mb".to_string()],
                affected_features.clone(),
            );
            affected_features
        } else {
            Vec::new()
        };
        drop(_transaction);

        if state.msd_storage.ensure_dirs(&bmc_id).await.is_err() {
            return msd_fault_response(
                &state,
                &bmc_id,
                StatusCode::INTERNAL_SERVER_ERROR,
                FeatureFault::operation(
                    FeatureFaultDomain::Storage,
                    "storage_unavailable",
                    "Media storage is unavailable",
                    true,
                    "initialize_drive",
                ),
            )
            .await;
        }
        let size_mb = state
            .msd_storage
            .drive_init_size(&bmc_id, params.size_mb)
            .await;
        Json(serde_json::json!({
            "ok": true,
            "success": true,
            "size_mb": size_mb,
            "mutation_id": mutation_id,
            "affected_features": affected_features,
            "revision": state.configuration.current_revision(),
        }))
        .into_response()
    }))
    .await
}

pub async fn msd_drive_delete(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state.msd_storage.drive_delete_all(&bmc_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(error) => {
            let status = storage_status(&error);
            msd_fault_response(
                &state,
                &bmc_id,
                status,
                storage_fault(&error, "delete_drive"),
            )
            .await
        }
    }
}

pub async fn msd_drive_files_list(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let path = params
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    if path.is_empty() {
        return Json(serde_json::json!({"items": [], "path": "/"}));
    }
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{}", path)
    };
    let items = state.msd_storage.drive_files_list(&bmc_id, &path).await;
    Json(serde_json::json!({"items": items, "path": path}))
}

pub async fn msd_drive_files_upload(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    mut multipart: axum::extract::Multipart,
) -> Response {
    use tokio::io::AsyncWriteExt;
    static UPLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let Ok(Ok(_permit)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), UPLOADS.acquire()).await
    else {
        return static_msd_fault(
            StatusCode::TOO_MANY_REQUESTS,
            "upload_limit",
            "Too many drive uploads",
            true,
            "upload_drive_file",
        );
    };
    let malformed = || {
        static_msd_fault(
            StatusCode::BAD_REQUEST,
            "invalid_upload",
            "The multipart upload is incomplete or invalid",
            false,
            "upload_drive_file",
        )
    };
    if state.msd_storage.ensure_dirs(&bmc_id).await.is_err() {
        return malformed();
    }
    let (capacity, _, _, _) = state.msd_storage.drive_info(&bmc_id).await;
    let mut rel_path = "/".to_owned();
    let mut upload = None;
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => return malformed(),
        };
        let name = field.name().unwrap_or("").to_owned();
        if let Some(filename) = field.file_name().map(str::to_owned) {
            if upload.is_some() {
                return malformed();
            }
            let staged = match state.msd_storage.create_staged_upload(&bmc_id).await {
                Ok(file) => file,
                Err(_) => return malformed(),
            };
            let mut output = match tokio::fs::OpenOptions::new()
                .write(true)
                .open(staged.path())
                .await
            {
                Ok(file) => file,
                Err(_) => return malformed(),
            };
            let mut size = 0u64;
            loop {
                let chunk = match field.chunk().await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(_) => return malformed(),
                };
                size = match size
                    .checked_add(chunk.len() as u64)
                    .filter(|n| *n <= capacity)
                {
                    Some(size) => size,
                    None => {
                        return static_msd_fault(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "drive_file_too_large",
                            "The drive upload exceeds capacity",
                            false,
                            "upload_drive_file",
                        )
                    }
                };
                if output.write_all(&chunk).await.is_err() {
                    return malformed();
                }
            }
            if output.sync_all().await.is_err() {
                return malformed();
            }
            if name != "file" {
                rel_path = name;
            }
            upload = Some((filename, staged, size));
        } else {
            let mut data = Vec::new();
            loop {
                let chunk = match field.chunk().await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(_) => return malformed(),
                };
                if data.len().saturating_add(chunk.len()) > 8192 {
                    return static_msd_fault(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "drive_metadata_too_large",
                        "Drive metadata exceeds 8 KiB",
                        false,
                        "upload_drive_file",
                    );
                }
                data.extend_from_slice(&chunk);
            }
            if name == "path" {
                rel_path = match String::from_utf8(data) {
                    Ok(path) => path,
                    Err(_) => return malformed(),
                };
            }
        }
    }
    let Some((filename, staged, size)) = upload else {
        return malformed();
    };
    match state
        .msd_storage
        .drive_upload_file_from_path(&bmc_id, &rel_path, &filename, staged.path(), size)
        .await
    {
        Ok(size) => Json(serde_json::json!({"ok": true, "size": size})).into_response(),
        Err(_) => static_msd_fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "drive_file_upload_failed",
            "The drive file could not be stored",
            false,
            "upload_drive_file",
        ),
    }
}

pub async fn msd_drive_files_download(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let path = params.get("path").map(String::as_str).unwrap_or("");
    if path.is_empty() {
        return static_msd_fault(
            StatusCode::BAD_REQUEST,
            "path_required",
            "A drive file path is required",
            false,
            "download_drive_file",
        );
    }

    match state.msd_storage.drive_read_file(&bmc_id, path).await {
        Ok(Some((filename, data))) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", filename.replace('"', "_")),
                ),
            ],
            data,
        )
            .into_response(),
        Ok(None) => static_msd_fault(
            StatusCode::NOT_FOUND,
            "file_not_found",
            "The requested drive file was not found",
            false,
            "download_drive_file",
        ),
        Err(_) => {
            msd_fault_response(
                &state,
                &bmc_id,
                StatusCode::BAD_REQUEST,
                FeatureFault::operation(
                    FeatureFaultDomain::Storage,
                    "drive_file_unavailable",
                    "The requested drive file is unavailable",
                    false,
                    "download_drive_file",
                ),
            )
            .await
        }
    }
}

pub async fn msd_drive_files_delete(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<serde_json::Value>,
) -> Response {
    let file_path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if file_path.is_empty() {
        return static_msd_fault(
            StatusCode::BAD_REQUEST,
            "path_required",
            "A drive file path is required",
            false,
            "delete_drive_file",
        );
    }
    match state
        .msd_storage
        .drive_delete_file(&bmc_id, file_path)
        .await
    {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(error) => {
            let status = storage_status(&error);
            msd_fault_response(
                &state,
                &bmc_id,
                status,
                storage_fault(&error, "delete_drive_file"),
            )
            .await
        }
    }
}

pub async fn msd_drive_mkdir(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<MsdDriveMkdir>,
) -> Response {
    let path = if params.path.starts_with('/') {
        params.path.clone()
    } else {
        format!("/{}", params.path)
    };
    match state.msd_storage.drive_mkdir(&bmc_id, &path).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(error) => {
            let status = storage_status(&error);
            msd_fault_response(
                &state,
                &bmc_id,
                status,
                storage_fault(&error, "create_drive_directory"),
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::IpAddr;

    use super::{
        checked_transfer_size, download_request_headers, header, is_private_download_address,
        merge_attachment_snapshot, msd_occupancy, normalize_media_type, public_remote_media_url,
        reconcile_attachment_snapshot, upload_too_large_fault, validate_download_url,
        validate_remote_media_url, FeatureFault, FeatureFaultDomain, HeaderMap, HeaderValue,
        MsdAttachment, MsdCapabilities, MsdMedia, MsdMediaKind, MsdMediaReference, MsdMediaSource,
        MsdMode, MsdOccupancyState, MsdOwnedMount, DOWNLOAD_HEADERS_ENVELOPE,
    };
    use crate::bmc::feature::config::{BackendParams, FeatureConfig, FeatureParams};
    use crate::bmc::feature::connection::{FeatureConnection, FeatureGuard};
    use crate::bmc::feature::msd::{MsdSlotCapabilities, MsdStorageCapabilities};

    #[test]
    fn occupancy_does_not_invent_an_attachment_for_an_external_session() {
        let fault = FeatureFault::feature(
            FeatureFaultDomain::Lifecycle,
            "session_in_use",
            "busy",
            true,
        );
        let occupancy = msd_occupancy(&[], Some(&fault));
        assert_eq!(occupancy.state, MsdOccupancyState::External);
        assert!(!occupancy.details_available);
        assert_eq!(occupancy.active_sessions, None);
    }

    #[test]
    fn known_attachments_take_precedence_over_a_stale_busy_fault() {
        let fault = FeatureFault::feature(
            FeatureFaultDomain::Lifecycle,
            "session_in_use",
            "busy",
            true,
        );
        let occupancy = msd_occupancy(&[optical_attachment()], Some(&fault));
        assert_eq!(occupancy.state, MsdOccupancyState::Managed);
        assert!(occupancy.details_available);
    }

    #[test]
    fn media_type_selection_is_validated_per_storage_location() {
        let capabilities = MsdCapabilities {
            virtual_drive: false,
            slots: vec![MsdSlotCapabilities::new(
                "optical",
                vec![MsdMediaKind::Optical, MsdMediaKind::Floppy],
            )],
            storage_locations: vec![
                MsdStorageCapabilities::aggregator(vec![
                    MsdMediaKind::Optical,
                    MsdMediaKind::Floppy,
                ]),
                MsdStorageCapabilities::remote_url(Vec::new()),
            ],
            unavailable_storage_locations: Vec::new(),
            target_management: Default::default(),
        };
        let local = MsdMediaReference::Aggregator {
            image_id: "boot.iso".to_string(),
        };
        let remote = MsdMediaReference::Remote {
            url: "https://images.example.test/boot.iso".to_string(),
        };

        assert_eq!(
            normalize_media_type(&capabilities, "optical", &local, None),
            Err((
                "media_type_required",
                "This storage location requires an explicit media type",
            ))
        );
        assert_eq!(
            normalize_media_type(&capabilities, "optical", &local, Some(MsdMediaKind::Floppy)),
            Ok(Some(MsdMediaKind::Floppy))
        );
        assert_eq!(
            normalize_media_type(&capabilities, "optical", &remote, None),
            Ok(None)
        );
        assert!(normalize_media_type(
            &capabilities,
            "optical",
            &remote,
            Some(MsdMediaKind::Optical)
        )
        .is_err());
    }

    #[test]
    fn remote_virtual_media_requires_https_without_credentials() {
        assert!(matches!(
            validate_remote_media_url("https://images.example.test/install.iso", None),
            Ok(MsdMedia {
                source: MsdMediaSource::RemoteUrl { .. },
                ..
            })
        ));
        assert!(validate_remote_media_url("http://images.example.test/install.iso", None).is_err());
        assert!(validate_remote_media_url(
            "https://user:pass@images.example.test/install.iso",
            None,
        )
        .is_err());
    }

    #[test]
    fn public_remote_media_reference_drops_signed_url_secrets() {
        assert_eq!(
            public_remote_media_url(
                "https://images.example.test/install.iso?token=secret#fragment"
            )
            .unwrap(),
            "https://images.example.test/install.iso"
        );
    }

    #[test]
    fn remote_download_address_filter_rejects_non_public_ranges() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "224.0.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "ff02::1",
        ] {
            assert!(is_private_download_address(
                address.parse::<IpAddr>().unwrap()
            ));
        }
        assert!(!is_private_download_address(
            "8.8.8.8".parse::<IpAddr>().unwrap()
        ));
    }

    #[tokio::test]
    async fn mapped_download_addresses_follow_the_embedded_ipv4_policy() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "224.0.0.1",
        ] {
            let ipv4: std::net::Ipv4Addr = address.parse().unwrap();
            assert!(
                is_private_download_address(IpAddr::V6(ipv4.to_ipv6_mapped())),
                "mapped {address} bypassed the address policy"
            );
        }
        let public: std::net::Ipv4Addr = "8.8.8.8".parse().unwrap();
        assert!(!is_private_download_address(IpAddr::V6(
            public.to_ipv6_mapped()
        )));
        for host in [
            "[::1]",
            "[::ffff:127.0.0.1]",
            "[::ffff:10.0.0.1]",
            "[::ffff:169.254.169.254]",
        ] {
            let url = format!("http://{host}/image.iso");
            assert!(validate_download_url(&url, false).await.is_err());
            assert!(validate_download_url(&url, true).await.is_ok());
        }
        assert!(
            validate_download_url("https://[2606:4700:4700::1111]/image.iso", false)
                .await
                .is_ok()
        );
    }

    #[test]
    fn transfer_size_limit_accepts_boundary_and_rejects_first_excess_byte() {
        assert_eq!(checked_transfer_size(7, 1, 8), Some(8));
        assert_eq!(checked_transfer_size(8, 1, 8), None);
        assert_eq!(checked_transfer_size(u64::MAX, 1, u64::MAX), None);
    }

    #[test]
    fn download_headers_merge_config_and_request_user_agent() {
        let configured = std::collections::HashMap::from([(
            "Accept".to_string(),
            "application/octet-stream".to_string(),
        )]);
        let mut incoming = HeaderMap::new();
        incoming.insert(
            DOWNLOAD_HEADERS_ENVELOPE,
            HeaderValue::from_static("{\"User-Agent\":\"BmcAggregator/1.0\"}"),
        );
        let url = reqwest::Url::parse("https://images.example.test/install.iso").unwrap();
        let headers = download_request_headers(
            &configured,
            &["https://images.example.test".to_string()],
            &url,
            &incoming,
        )
        .unwrap();
        assert_eq!(headers[header::ACCEPT], "application/octet-stream");
        assert_eq!(headers[header::USER_AGENT], "BmcAggregator/1.0");
    }

    #[test]
    fn download_headers_reject_transport_owned_fields() {
        let mut incoming = HeaderMap::new();
        incoming.insert(
            DOWNLOAD_HEADERS_ENVELOPE,
            HeaderValue::from_static("{\"Range\":\"bytes=0-\"}"),
        );
        assert!(matches!(
            download_request_headers(
                &std::collections::HashMap::new(),
                &[],
                &reqwest::Url::parse("https://images.example.test/install.iso").unwrap(),
                &incoming,
            ),
            Err("restricted_download_header")
        ));
    }

    #[test]
    fn configured_download_headers_are_origin_scoped() {
        let configured = std::collections::HashMap::from([(
            "Authorization".to_string(),
            "Bearer secret".to_string(),
        )]);
        let url = reqwest::Url::parse("https://attacker.example.test/install.iso").unwrap();
        assert_eq!(
            download_request_headers(&configured, &[], &url, &HeaderMap::new()),
            Err("download_header_origin_not_allowed")
        );
    }

    #[test]
    fn upload_limit_uses_the_unified_non_retryable_storage_fault() {
        let fault = upload_too_large_fault();
        assert_eq!(fault.domain, FeatureFaultDomain::Storage);
        assert_eq!(fault.code, "upload_too_large");
        assert!(!fault.retryable);
        assert!(matches!(
            fault.scope,
            crate::bmc::feature::fault::FeatureFaultScope::Operation {
                operation: "upload_image"
            }
        ));
    }

    fn test_feature_connection() -> std::sync::Arc<FeatureConnection> {
        FeatureConnection::new(
            "test-bmc".to_string(),
            "msd".to_string(),
            "test".to_string(),
            FeatureConfig {
                backend: "test".to_string(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        )
    }

    fn optical_attachment() -> MsdAttachment {
        MsdAttachment {
            slot_id: "optical".to_string(),
            mode: MsdMode::Image,
            media: Some(MsdMediaReference::Aggregator {
                image_id: "installer.iso".to_string(),
            }),
            media_type: Some(MsdMediaKind::Optical),
        }
    }

    #[tokio::test]
    async fn authoritative_backend_snapshot_drops_stale_owned_mount_and_guard() {
        let fc = test_feature_connection();
        let runtime = crate::bmc::feature::msd::feature_runtime(&fc).await;
        runtime.owned_mounts.lock().await.insert(
            "optical".to_string(),
            MsdOwnedMount {
                guard: FeatureGuard::new(fc.clone()),
                attachment: optical_attachment(),
            },
        );
        assert_eq!(fc.ref_count(), 1);

        assert!(reconcile_attachment_snapshot(&runtime, Some(Vec::new()))
            .await
            .is_empty());
        assert!(runtime.owned_mounts.lock().await.is_empty());
        assert_eq!(fc.ref_count(), 0);
    }

    #[tokio::test]
    async fn non_introspectable_backend_keeps_a_live_owned_mount() {
        let fc = test_feature_connection();
        let runtime = crate::bmc::feature::msd::feature_runtime(&fc).await;
        runtime.owned_mounts.lock().await.insert(
            "optical".to_string(),
            MsdOwnedMount {
                guard: FeatureGuard::new(fc.clone()),
                attachment: optical_attachment(),
            },
        );

        assert_eq!(
            reconcile_attachment_snapshot(&runtime, None).await,
            vec![optical_attachment()]
        );
        assert_eq!(fc.ref_count(), 1);
    }

    #[test]
    fn authoritative_merge_drops_unowned_runtime_entries() {
        let runtime = BTreeMap::from([(
            "optical".to_string(),
            MsdAttachment {
                slot_id: "optical".to_string(),
                mode: MsdMode::Image,
                media: None,
                media_type: Some(MsdMediaKind::Optical),
            },
        )]);
        assert!(merge_attachment_snapshot(&runtime, Some(Vec::new())).is_empty());
    }

    #[test]
    fn runtime_reference_enriches_matching_backend_attachment() {
        let runtime_attachment = MsdAttachment {
            slot_id: "optical".to_string(),
            mode: MsdMode::Image,
            media: Some(MsdMediaReference::Aggregator {
                image_id: "installer.iso".to_string(),
            }),
            media_type: Some(MsdMediaKind::Optical),
        };
        let mut runtime = BTreeMap::new();
        runtime.insert("optical".to_string(), runtime_attachment.clone());
        let merged = merge_attachment_snapshot(
            &runtime,
            Some(vec![MsdAttachment {
                media: None,
                ..runtime_attachment.clone()
            }]),
        );
        assert_eq!(merged, vec![runtime_attachment]);
    }
}
