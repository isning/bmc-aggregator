use crate::bmc::media_digest::calculate_digests;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

#[cfg(test)]
use super::handler::DEFAULT_MAX_IMAGE_BYTES;
use super::handler::{backend_media_store, max_image_bytes};
use super::{
    MsdMediaDigest, MsdMediaKind, MsdMediaUpload, MsdStorageLocation, MsdStorageOperation,
};
use crate::bmc::feature::fault::{
    fault_response, record_fault_response, FeatureFault, FeatureFaultDomain,
};
use crate::bmc::msd_storage::MsdStorageProvider;
use crate::web::state::AppState;

const UPLOAD_OFFSET_HEADER: &str = "upload-offset";
const ACTIVE_SESSION_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const TERMINAL_SESSION_TTL_MS: u64 = 24 * 60 * 60 * 1000;
type UploadLock = tokio::sync::Mutex<()>;
static UPLOAD_LOCKS: LazyLock<Mutex<HashMap<String, Weak<UploadLock>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn upload_lock(device_id: &str, upload_id: &str) -> Arc<UploadLock> {
    let key = format!("{device_id}/{upload_id}");
    let mut locks = UPLOAD_LOCKS.lock().expect("upload lock registry poisoned");
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(UploadLock::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsdTransferState {
    Uploading,
    Finalizing,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Publication {
    #[default]
    Pending,
    Dispatching,
    Published,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MsdTransferStatus {
    pub id: String,
    pub device_id: String,
    pub filename: String,
    pub location: MsdStorageLocation,
    pub media_type: Option<MsdMediaKind>,
    pub length: u64,
    pub offset: u64,
    pub state: MsdTransferState,
    pub image_id: Option<String>,
    #[serde(default)]
    pub publication: Publication,
    pub digests: Vec<MsdMediaDigest>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Deserialize)]
pub struct CreateUploadRequest {
    pub filename: String,
    pub location: MsdStorageLocation,
    #[serde(default)]
    pub media_type: Option<MsdMediaKind>,
    pub length: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn valid_upload_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok()
}

async fn paths(
    state: &AppState,
    device_id: &str,
    id: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let base = state.msd_storage.staging_dir(device_id).await;
    (
        base.join(format!("{id}.part")),
        base.join(format!("{id}.json")),
    )
}

async fn save_status(path: &std::path::Path, status: &MsdTransferStatus) -> Result<(), String> {
    let bytes = serde_json::to_vec(status).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut file = tokio::fs::File::create(&temporary)
        .await
        .map_err(|error| error.to_string())?;
    file.write_all(&bytes)
        .await
        .map_err(|error| error.to_string())?;
    file.sync_all().await.map_err(|error| error.to_string())?;
    tokio::fs::rename(&temporary, path)
        .await
        .map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "upload checkpoint has no parent".to_owned())?;
    tokio::fs::File::open(parent)
        .await
        .map_err(|error| error.to_string())?
        .sync_all()
        .await
        .map_err(|error| error.to_string())
}

async fn load_status(
    state: &AppState,
    device_id: &str,
    id: &str,
) -> Result<MsdTransferStatus, Response> {
    if !valid_upload_id(id) {
        return Err(not_found_fault());
    }
    let (_, status_path) = paths(state, device_id, id).await;
    let bytes = tokio::fs::read(status_path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            not_found_fault()
        } else {
            transfer_fault(
                StatusCode::INTERNAL_SERVER_ERROR,
                "upload_state_read_failed",
                "The upload checkpoint could not be read; retry before starting a new upload",
                true,
                "resume_upload",
            )
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        transfer_fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upload_state_invalid",
            "The upload state could not be read",
            false,
            "resume_upload",
        )
    })
}

fn transfer_fault(
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

fn not_found_fault() -> Response {
    transfer_fault(
        StatusCode::NOT_FOUND,
        "upload_not_found",
        "The upload session was not found",
        false,
        "resume_upload",
    )
}

fn upload_write_fault() -> Response {
    transfer_fault(
        StatusCode::INSUFFICIENT_STORAGE,
        "upload_write_failed",
        "The media image could not be written",
        true,
        "resume_upload",
    )
}

async fn cleanup_expired_uploads(state: &AppState, device_id: &str) {
    let directory = state.msd_storage.staging_dir(device_id).await;
    let Ok(mut entries) = tokio::fs::read_dir(&directory).await else {
        return;
    };
    let now = now_ms();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".json") || name.starts_with("download-") {
            continue;
        }
        let Some(id) = name.strip_suffix(".json").filter(|id| valid_upload_id(id)) else {
            continue;
        };
        let lock = upload_lock(device_id, id);
        let Ok(_guard) = lock.try_lock() else {
            continue;
        };
        let Ok(bytes) = tokio::fs::read(entry.path()).await else {
            continue;
        };
        let Ok(status) = serde_json::from_slice::<MsdTransferStatus>(&bytes) else {
            continue;
        };
        if status.publication == Publication::Dispatching
            && status.location != MsdStorageLocation::Aggregator
        {
            continue;
        }
        let terminal = matches!(
            status.state,
            MsdTransferState::Completed | MsdTransferState::Cancelled | MsdTransferState::Failed
        );
        let ttl = if terminal {
            TERMINAL_SESSION_TTL_MS
        } else {
            ACTIVE_SESSION_TTL_MS
        };
        if now.saturating_sub(status.updated_at_ms) <= ttl {
            continue;
        }
        let (part, sidecar) = paths(state, device_id, &status.id).await;
        let _ = tokio::fs::remove_file(part).await;
        let _ = tokio::fs::remove_file(sidecar).await;
    }
}

pub async fn create_upload(
    Path(device_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateUploadRequest>,
) -> Response {
    if state.registry.get_dev(&device_id).await.is_none() {
        return not_found_fault();
    }
    let filename = crate::bmc::msd_storage::FsMsdStorage::safe_filename(&request.filename);
    if !crate::bmc::msd_storage::FsMsdStorage::is_image_name(&filename) {
        return transfer_fault(
            StatusCode::BAD_REQUEST,
            "invalid_image_name",
            "A public image name must not use an internal storage name",
            false,
            "create_upload",
        );
    }
    cleanup_expired_uploads(&state, &device_id).await;
    let maximum = max_image_bytes(&state, &device_id).await;
    if request.length == 0 {
        return transfer_fault(
            StatusCode::BAD_REQUEST,
            "invalid_upload_length",
            "The upload length must be greater than zero",
            false,
            "create_upload",
        );
    }
    if request.length > maximum {
        return transfer_fault(
            StatusCode::PAYLOAD_TOO_LARGE,
            "upload_too_large",
            "The media image exceeds the upload size limit",
            false,
            "create_upload",
        );
    }
    match request.location {
        MsdStorageLocation::Aggregator => {}
        MsdStorageLocation::Bmc => {
            if let Err(response) = backend_media_store(
                &state,
                &device_id,
                request.location,
                MsdStorageOperation::Upload,
                "create_upload",
            )
            .await
            {
                return response;
            }
        }
        MsdStorageLocation::Remote => {
            return transfer_fault(
                StatusCode::BAD_REQUEST,
                "storage_location_unsupported",
                "Images cannot be uploaded into remote URL storage",
                false,
                "create_upload",
            );
        }
    }
    if state.msd_storage.ensure_dirs(&device_id).await.is_err() {
        return transfer_fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upload_staging_failed",
            "The media image upload could not be staged",
            true,
            "create_upload",
        );
    }
    let id = uuid::Uuid::new_v4().to_string();
    let (part_path, status_path) = paths(&state, &device_id, &id).await;
    if tokio::fs::create_dir_all(state.msd_storage.staging_dir(&device_id).await)
        .await
        .is_err()
    {
        return transfer_fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upload_staging_failed",
            "The media image upload could not be staged",
            true,
            "create_upload",
        );
    }
    if tokio::fs::File::create(&part_path).await.is_err() {
        return transfer_fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upload_staging_failed",
            "The media image upload could not be staged",
            true,
            "create_upload",
        );
    }
    let timestamp = now_ms();
    let status = MsdTransferStatus {
        id,
        device_id,
        filename,
        location: request.location,
        media_type: request.media_type,
        length: request.length,
        offset: 0,
        state: MsdTransferState::Uploading,
        image_id: None,
        publication: Publication::Pending,
        digests: Vec::new(),
        created_at_ms: timestamp,
        updated_at_ms: timestamp,
    };
    if save_status(&status_path, &status).await.is_err() {
        let _ = tokio::fs::remove_file(part_path).await;
        return transfer_fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upload_state_write_failed",
            "The upload state could not be stored",
            true,
            "create_upload",
        );
    }
    (StatusCode::CREATED, Json(status)).into_response()
}

pub async fn upload_status(
    Path((device_id, upload_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let lock = upload_lock(&device_id, &upload_id);
    let _guard = lock.lock().await;
    match load_status(&state, &device_id, &upload_id).await {
        Ok(status) => Json(status).into_response(),
        Err(response) => response,
    }
}

pub async fn complete_upload(
    Path((device_id, upload_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let lock = upload_lock(&device_id, &upload_id);
    let guard = lock.lock_owned().await;
    let status = match load_status(&state, &device_id, &upload_id).await {
        Ok(status) => status,
        Err(response) => return response,
    };
    if status.state == MsdTransferState::Completed {
        return Json(status).into_response();
    }
    if status.state != MsdTransferState::Finalizing {
        return transfer_fault(
            StatusCode::CONFLICT,
            "upload_not_complete",
            "The upload is not ready for publication",
            false,
            "finalize_upload",
        );
    }
    let (part, checkpoint) = paths(&state, &device_id, &upload_id).await;
    finalize_owned(state, part, checkpoint, status, guard).await
}

async fn finalize_owned(
    state: Arc<AppState>,
    part: std::path::PathBuf,
    checkpoint: std::path::PathBuf,
    mut status: MsdTransferStatus,
    guard: tokio::sync::OwnedMutexGuard<()>,
) -> Response {
    crate::web::state::mutation_response(tokio::spawn(async move {
        let _guard = guard;
        match finalize_upload(&state, &part, &checkpoint, &mut status).await {
            Ok(()) => Json(status).into_response(),
            Err(response) => response,
        }
    }))
    .await
}

pub async fn append_upload(
    Path((device_id, upload_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let lock = upload_lock(&device_id, &upload_id);
    let guard = lock.lock_owned().await;
    let mut status = match load_status(&state, &device_id, &upload_id).await {
        Ok(status) => status,
        Err(response) => return response,
    };
    if status.state != MsdTransferState::Uploading {
        return transfer_fault(
            StatusCode::CONFLICT,
            "upload_not_writable",
            "The upload session is not writable",
            false,
            "resume_upload",
        );
    }
    let supplied_offset = headers
        .get(UPLOAD_OFFSET_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if supplied_offset != Some(status.offset) {
        return transfer_fault(
            StatusCode::CONFLICT,
            "upload_offset_mismatch",
            "The upload offset does not match the stored state",
            true,
            "resume_upload",
        );
    }
    let maximum = max_image_bytes(&state, &device_id).await;
    let (part_path, status_path) = paths(&state, &device_id, &upload_id).await;
    let mut file = match tokio::fs::OpenOptions::new()
        .write(true)
        .append(true)
        .open(&part_path)
        .await
    {
        Ok(file) => file,
        Err(_) => return upload_write_fault(),
    };
    let actual = match file.metadata().await {
        Ok(metadata) => metadata.len(),
        Err(_) => return upload_write_fault(),
    };
    if actual < status.offset {
        return transfer_fault(
            StatusCode::CONFLICT,
            "upload_data_lost",
            "The staged image is shorter than its checkpoint; restart the upload",
            false,
            "resume_upload",
        );
    }
    if actual > status.offset && file.set_len(status.offset).await.is_err() {
        return upload_write_fault();
    }
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => {
                return transfer_fault(
                    StatusCode::BAD_REQUEST,
                    "invalid_upload",
                    "The media image upload could not be read",
                    true,
                    "resume_upload",
                )
            }
        };
        let next = match status.offset.checked_add(chunk.len() as u64) {
            Some(next) if next <= status.length && next <= maximum => next,
            _ => {
                return transfer_fault(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "upload_too_large",
                    "The media image exceeds the upload size limit",
                    false,
                    "resume_upload",
                )
            }
        };
        if file.write_all(&chunk).await.is_err() {
            return upload_write_fault();
        }
        status.offset = next;
    }
    if file.sync_all().await.is_err() {
        return upload_write_fault();
    }
    status.updated_at_ms = now_ms();
    if status.offset == status.length {
        status.state = MsdTransferState::Finalizing;
    }
    if save_status(&status_path, &status).await.is_err() {
        return transfer_fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upload_state_write_failed",
            "The upload state could not be stored",
            true,
            "resume_upload",
        );
    }
    if status.state == MsdTransferState::Finalizing {
        return finalize_owned(state, part_path, status_path, status, guard).await;
    }
    Json(status).into_response()
}

async fn finalize_upload(
    state: &Arc<AppState>,
    part_path: &std::path::Path,
    checkpoint: &std::path::Path,
    status: &mut MsdTransferStatus,
) -> Result<(), Response> {
    if status.digests.is_empty() {
        status.digests = calculate_digests(part_path).await.map_err(|_| {
            transfer_fault(
                StatusCode::INTERNAL_SERVER_ERROR,
                "checksum_failed",
                "The media image checksums could not be calculated",
                true,
                "finalize_upload",
            )
        })?;
        save_status(checkpoint, status)
            .await
            .map_err(|_| upload_write_fault())?;
    }
    let digests = status.digests.clone();
    let recovering = status.publication == Publication::Dispatching;
    if recovering && status.location == MsdStorageLocation::Bmc {
        return Err(transfer_fault(
            StatusCode::CONFLICT,
            "upload_result_unknown",
            "BMC publication was interrupted; reconcile remote storage before retrying",
            false,
            "finalize_upload",
        ));
    }
    match status.location {
        MsdStorageLocation::Aggregator => {
            if status.publication != Publication::Published {
                status.publication = Publication::Dispatching;
                save_status(checkpoint, status)
                    .await
                    .map_err(|_| upload_write_fault())?;
                let result = if recovering {
                    state
                        .msd_storage
                        .recover_image_file(
                            &status.device_id,
                            &status.filename,
                            part_path,
                            status.length,
                            &digests,
                        )
                        .await
                } else {
                    state
                        .msd_storage
                        .publish_image_file(
                            &status.device_id,
                            &status.filename,
                            part_path,
                            status.length,
                            &digests,
                        )
                        .await
                };
                result.map_err(|error| match error {
                    crate::bmc::msd_storage::ImageDigestError::Changed => transfer_fault(StatusCode::CONFLICT,"upload_image_replaced","The published image was replaced; this upload cannot replace it during recovery",false,"finalize_upload"),
                    crate::bmc::msd_storage::ImageDigestError::Storage(_) => upload_write_fault(),
                })?;
                status.image_id = Some(status.filename.clone());
                status.publication = Publication::Published;
                save_status(checkpoint, status)
                    .await
                    .map_err(|_| upload_write_fault())?;
            }
            state
                .msd_storage
                .store_image_digests(&status.device_id, &status.filename, &digests)
                .await
                .map_err(|error| {
                    if matches!(error, crate::bmc::msd_storage::ImageDigestError::Changed) {
                        return transfer_fault(StatusCode::CONFLICT, "upload_image_replaced", "The published image was replaced; the old upload cannot update its metadata", false, "finalize_upload");
                    }
                    transfer_fault(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "image_metadata_write_failed",
                        "The media image checksum metadata could not be stored",
                        true,
                        "finalize_upload",
                    )
                })?;
        }
        MsdStorageLocation::Bmc => {
            let (_guard, fc, store) = backend_media_store(
                state,
                &status.device_id,
                status.location,
                MsdStorageOperation::Upload,
                "finalize_upload",
            )
            .await?;
            if status.publication != Publication::Published {
                status.publication = Publication::Dispatching;
                save_status(checkpoint, status)
                    .await
                    .map_err(|_| upload_write_fault())?;
                let image = match store
                    .upload(MsdMediaUpload {
                        name: status.filename.clone(),
                        content_type: None,
                        media_type: status.media_type,
                        path: part_path.to_path_buf(),
                        size: status.length,
                        digests: digests.clone(),
                    })
                    .await
                {
                    Ok(image) => image,
                    Err(_) => {
                        return Err(record_fault_response(
                            &fc,
                            StatusCode::BAD_GATEWAY,
                            FeatureFault::operation(
                                FeatureFaultDomain::Storage,
                                "storage_upload_failed",
                                "The BMC upload result is unknown; inspect remote storage before retrying",
                                false,
                                "finalize_upload",
                            ),
                        )
                        .await)
                    }
                };
                status.image_id = Some(image.id);
                status.publication = Publication::Published;
                save_status(checkpoint, status)
                    .await
                    .map_err(|_| upload_write_fault())?;
            }
        }
        MsdStorageLocation::Remote => unreachable!("remote upload rejected at creation"),
    }
    status.digests = digests;
    status.state = MsdTransferState::Completed;
    status.updated_at_ms = now_ms();
    save_status(checkpoint, status)
        .await
        .map_err(|_| upload_write_fault())?;
    let _ = tokio::fs::remove_file(part_path).await;
    Ok(())
}

pub async fn cancel_upload(
    Path((device_id, upload_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let lock = upload_lock(&device_id, &upload_id);
    let _guard = lock.lock().await;
    let mut status = match load_status(&state, &device_id, &upload_id).await {
        Ok(status) => status,
        Err(response) => return response,
    };
    if matches!(
        status.state,
        MsdTransferState::Completed | MsdTransferState::Cancelled
    ) {
        return Json(status).into_response();
    }
    if status.publication != Publication::Pending {
        return transfer_fault(
            StatusCode::CONFLICT,
            "upload_publication_started",
            "Publication has started; inspect or complete this upload",
            false,
            "cancel_upload",
        );
    }
    let (part_path, status_path) = paths(&state, &device_id, &upload_id).await;
    status.state = MsdTransferState::Cancelled;
    status.updated_at_ms = now_ms();
    if save_status(&status_path, &status).await.is_err() {
        return transfer_fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upload_state_write_failed",
            "The upload state could not be stored",
            true,
            "cancel_upload",
        );
    }
    let _ = tokio::fs::remove_file(part_path).await;
    Json(status).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (Arc<AppState>, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let config: crate::config::Config = serde_yaml::from_str("server:\n  listen: 127.0.0.1:12340\n  frontend_dir: frontend/dist\ndevices:\n  - id: device\n    name: Test\n    protocol: none\n    host: 127.0.0.1\n    port: 0\n    auth: {username: test, password: test}\nmsd: {}\n").unwrap();
        let registry = crate::bmc::feature::device_registry::DeviceRegistry::new();
        registry.init(&config.devices, &[]).await;
        let config = Arc::new(tokio::sync::RwLock::new(config));
        let state = Arc::new(AppState {
            registry,
            config: config.clone(),
            config_path: String::new(),
            configuration: crate::bmc::configuration::ConfigurationCoordinator::new(),
            msd_storage: crate::bmc::msd_storage::FsMsdStorage::new(
                directory.path().to_string_lossy().into_owned(),
                config,
            ),
            active_downloads: tokio::sync::RwLock::new(Default::default()),
            webtransport: tokio::sync::RwLock::new(None),
        });
        (state, directory)
    }

    #[tokio::test]
    async fn unreadable_upload_checkpoint_is_not_reported_as_missing() {
        let (state, _directory) = fixture().await;
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            upload_status(Path(("device".into(), id.clone())), State(state.clone()))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        let (_, checkpoint) = paths(&state, "device", &id).await;
        tokio::fs::create_dir_all(&checkpoint).await.unwrap();
        let response = upload_status(Path(("device".into(), id)), State(state)).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let fault: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(fault["fault"]["code"], "upload_state_read_failed");
        assert_eq!(fault["fault"]["retryable"], true);
        assert!(!String::from_utf8_lossy(&body).contains(&checkpoint.to_string_lossy().to_string()));
        assert!(checkpoint.is_dir());
    }

    async fn begin(state: &Arc<AppState>, length: u64) -> String {
        let response = create_upload(
            Path("device".into()),
            State(state.clone()),
            Json(CreateUploadRequest {
                filename: "test.iso".into(),
                location: MsdStorageLocation::Aggregator,
                media_type: None,
                length,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn append(state: &Arc<AppState>, id: &str, offset: u64, bytes: &'static str) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(UPLOAD_OFFSET_HEADER, offset.to_string().parse().unwrap());
        append_upload(
            Path(("device".into(), id.into())),
            State(state.clone()),
            headers,
            Body::from(bytes),
        )
        .await
    }

    #[tokio::test]
    async fn backend_acquisition_failure_does_not_mark_remote_dispatch() {
        let (state, _dir) = fixture().await;
        let id = begin(&state, 4).await;
        let mut status = load_status(&state, "device", &id).await.unwrap();
        status.location = MsdStorageLocation::Bmc;
        status.offset = 4;
        status.state = MsdTransferState::Finalizing;
        let (part, checkpoint) = paths(&state, "device", &id).await;
        tokio::fs::write(part, b"abcd").await.unwrap();
        save_status(&checkpoint, &status).await.unwrap();
        for _ in 0..2 {
            let response =
                complete_upload(Path(("device".into(), id.clone())), State(state.clone())).await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                load_status(&state, "device", &id)
                    .await
                    .unwrap()
                    .publication,
                Publication::Pending
            );
        }
    }

    #[tokio::test]
    async fn short_staging_file_is_rejected_without_padding_or_publication() {
        let (state, dir) = fixture().await;
        let id = begin(&state, 8).await;
        assert_eq!(
            append(&state, &id, 0, "abcd").await.status(),
            StatusCode::OK
        );
        std::fs::write(
            dir.path().join(format!("device/images/.staging/{id}.part")),
            b"a",
        )
        .unwrap();
        assert_eq!(
            append(&state, &id, 4, "efgh").await.status(),
            StatusCode::CONFLICT
        );
        assert!(!dir.path().join("device/images/test.iso").exists());
    }

    #[tokio::test]
    async fn metadata_failure_can_resume_without_republishing_and_get_is_read_only() {
        let (state, dir) = fixture().await;
        let id = begin(&state, 4).await;
        let metadata = dir.path().join("device/images/.metadata");
        std::fs::write(&metadata, b"injected failure").unwrap();
        assert!(!append(&state, &id, 0, "abcd").await.status().is_success());
        assert_eq!(
            std::fs::read(dir.path().join("device/images/test.iso")).unwrap(),
            b"abcd"
        );
        let status = upload_status(Path(("device".into(), id.clone())), State(state.clone())).await;
        assert_eq!(status.status(), StatusCode::OK);
        std::fs::remove_file(metadata).unwrap();
        let response =
            complete_upload(Path(("device".into(), id.clone())), State(state.clone())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["state"], "completed");
        assert_eq!(
            complete_upload(Path(("device".into(), id)), State(state))
                .await
                .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn resuming_old_publication_preserves_newer_same_name_image() {
        use crate::bmc::msd_storage::MsdStorageProvider;
        let (state, dir) = fixture().await;
        let old = begin(&state, 4).await;
        let metadata = dir.path().join("device/images/.metadata");
        std::fs::write(&metadata, b"injected failure").unwrap();
        assert!(!append(&state, &old, 0, "aaaa").await.status().is_success());
        std::fs::remove_file(metadata).unwrap();
        let new = begin(&state, 4).await;
        assert_eq!(
            append(&state, &new, 0, "bbbb").await.status(),
            StatusCode::OK
        );
        let before = state.msd_storage.list_images("device").await;
        assert!(
            !complete_upload(Path(("device".into(), old)), State(state.clone()))
                .await
                .status()
                .is_success()
        );
        assert_eq!(state.msd_storage.list_images("device").await, before);
        assert_eq!(
            std::fs::read(dir.path().join("device/images/test.iso")).unwrap(),
            b"bbbb"
        );
    }

    #[test]
    fn upload_ids_are_strict_uuids() {
        assert!(valid_upload_id(&uuid::Uuid::new_v4().to_string()));
        assert!(!valid_upload_id("../escape"));
    }

    #[test]
    fn default_limit_is_large_iso_capable() {
        const { assert!(DEFAULT_MAX_IMAGE_BYTES >= 64 * 1024 * 1024 * 1024) };
    }

    #[test]
    fn storage_write_failure_is_retryable_507_fault() {
        let response = upload_write_fault();
        assert_eq!(response.status(), StatusCode::INSUFFICIENT_STORAGE);
    }
}
