use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use tracing::{debug, info, warn, error};

use crate::bmc::msd_manager::MsdManager;
use crate::bmc::msd_storage::{FsMsdStorage, MsdStorageProvider};
use crate::config::MsdDeviceConfig;
use crate::web::handlers::AppState;

pub async fn msd_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let runtime = state.msd_runtime_for(&bmc_id).await;
    MsdManager::status(state.orchestrator.registry(), &runtime, &bmc_id).await.into()
}

#[derive(Deserialize)]
pub struct MsdConnectParams {
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub image_id: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub backend: Option<String>,
}

pub async fn msd_connect(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<MsdConnectParams>,
) -> Response {
    let backend = payload.backend.clone().unwrap_or_else(|| "mahogany".to_string());
    let mode = payload.mode.clone().unwrap_or_else(|| "image".to_string());
    let image_id = payload.image_id.clone().or(payload.image.clone());

    match MsdManager::connect(
        state.orchestrator.registry(), &state.msd_storage,
        &bmc_id, &backend, &mode, image_id.clone(),
    ).await {
        Ok(_needs_keepalive) => {
            state.update_msd_runtime(&bmc_id, |runtime| {
                runtime.backend = backend;
                runtime.mode = mode;
                runtime.current_image = image_id;
                runtime.connected = true;
            }).await;
            (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
        }
        Err(e) => {
            warn!("msd_connect error: {}", e);
            (StatusCode::SERVICE_UNAVAILABLE, e).into_response()
        }
    }
}

pub async fn msd_disconnect(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let runtime = state.msd_runtime_for(&bmc_id).await;
    let backend = runtime.backend.clone();

    match MsdManager::disconnect(state.orchestrator.registry(), &state.msd_storage, &bmc_id, &backend).await {
        Ok(()) => {
            state.update_msd_runtime(&bmc_id, |r| {
                r.connected = false;
                r.mode = "drive".to_string();
                r.current_image = None;
            }).await;
            (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
        }
        Err(e) => {
            warn!("msd_disconnect error: {}", e);
            (StatusCode::SERVICE_UNAVAILABLE, e).into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct MsdSetImageParams {
    pub image: Option<String>,
    pub image_size: Option<u64>,
}

pub async fn msd_set_image(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<MsdSetImageParams>,
) -> Response {
    let selected = payload.image.clone();
    let size = payload.image_size.unwrap_or(0);

    match MsdManager::set_image(state.orchestrator.registry(), &bmc_id, selected.clone(), size).await {
        Ok(()) => {
            state.update_msd_runtime(&bmc_id, |r| {
                r.current_image = selected;
            }).await;
            (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
        }
        Err(e) => {
            (StatusCode::SERVICE_UNAVAILABLE, e).into_response()
        }
    }
}

pub async fn msd_images_list(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let _ = state.msd_storage.ensure_dirs(&bmc_id).await;
    let images = state.msd_storage.list_images(&bmc_id).await;
    Json(serde_json::Value::Array(images))
}

#[derive(Deserialize)]
pub struct MsdImageUploadParams {
    #[serde(default)]
    pub filename: Option<String>,
}

pub async fn msd_images_upload(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Query(params): Query<MsdImageUploadParams>,
    mut multipart: Multipart,
) -> Response {
    let _ = state.msd_storage.ensure_dirs(&bmc_id).await;
    let mut uploaded_name: Option<String> = None;
    let mut uploaded_size = 0u64;

    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() != Some("file") {
            continue;
        }
        let field_name = field
            .file_name()
            .map(|s| crate::bmc::msd_storage::FsMsdStorage::safe_filename(&s))
            .or_else(|| params.filename.clone())
            .unwrap_or_else(|| format!("upload-{}.img", uuid::Uuid::new_v4()));
        let bytes = match field.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": format!("invalid multipart payload: {}", error)})),
                )
                    .into_response();
            }
        };
        match state.msd_storage.upload_image(&bmc_id, &field_name, &bytes).await {
            Ok(size) => {
                uploaded_size = size;
                uploaded_name = Some(field_name);
            }
            Err(e) => {
                return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e}))).into_response();
            }
        }
        break;
    }

    let filename = uploaded_name
        .or(params.filename.clone())
        .unwrap_or_else(|| format!("upload-{}.img", uuid::Uuid::new_v4()));

    if uploaded_size == 0 {
        let _ = state.msd_storage.upload_image(&bmc_id, &filename, &[]).await;
    }

    (StatusCode::OK, Json(serde_json::json!({"id": filename, "name": filename, "size": uploaded_size}))).into_response()
}

pub async fn msd_images_delete(
    Path((bmc_id, image_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    match state.msd_storage.delete_image(&bmc_id, &image_id).await {
        Ok(()) => Json(serde_json::json!({"status": "ok"})),
        Err(_) => Json(serde_json::json!({"status": "error", "message": "invalid image id"})),
    }
}

#[derive(Deserialize)]
pub struct MsdImageDownloadParams {
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
}

pub async fn msd_images_download(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<MsdImageDownloadParams>,
) -> Response {
    let _ = state.msd_storage.ensure_dirs(&bmc_id).await;
    let url = match payload.url {
        Some(url) if !url.trim().is_empty() => url,
        _ => {
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "url is required"}))).into_response();
        }
    };

    let inferred = payload
        .filename
        .or_else(|| url.rsplit('/').next().map(|value| value.to_string()))
        .unwrap_or_else(|| "download.iso".to_string());

    let download_id = uuid::Uuid::new_v4().to_string();

    info!("[{}] download request: url={}, filename={}, download_id={}", bmc_id, url, inferred, download_id);

    let cfg = state.config.read().await;
    let default_headers = &cfg.server.download_headers;

    let mut headers = HeaderMap::new();
    for (key, value) in default_headers {
        if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(key.as_bytes()), HeaderValue::from_str(value)) {
            headers.insert(name, val);
        }
    }

    if let Some(override_headers) = &payload.headers {
        for (key, value) in override_headers {
            if let Ok(name) = HeaderName::from_bytes(key.as_bytes()) {
                if value.is_empty() {
                    headers.remove(&name);
                } else if let Ok(val) = HeaderValue::from_str(value) {
                    headers.insert(name, val);
                }
            }
        }
    }

    drop(cfg);

    let client = state.http_client.clone();
    let event_tx = state.event_tx.clone();
    let storage = state.msd_storage.clone();
    let bmc_id2 = bmc_id.clone();
    let url2 = url.clone();

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    state.active_downloads.write().await.insert(download_id.clone(), cancel_tx);

    let download_id2 = download_id.clone();
    let inferred2 = inferred.clone();
    tokio::spawn(async move {
        let result = stream_download(
            &client, &url2, headers, &storage, &bmc_id2, &inferred2,
            &download_id2, &event_tx, cancel_rx,
        ).await;
        state.active_downloads.write().await.remove(&download_id2);
        if let Err(e) = result {
            error!("[{}] download failed: {} (download_id={})", bmc_id2, e, download_id2);
            let total_bytes: Option<u64> = None;
            let progress_pct: Option<f64> = None;
            let event = serde_json::json!({
                "event": "msd.download_progress",
                "data": {
                    "download_id": download_id2,
                    "filename": inferred2,
                    "bytes_downloaded": 0u64,
                    "total_bytes": total_bytes,
                    "progress_pct": progress_pct,
                    "status": format!("failed: {}", e),
                }
            });
            let _ = event_tx.send(event.to_string());
        }
    });

    let init_total: Option<u64> = None;
    let init_pct: Option<f64> = None;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "download_id": download_id,
            "filename": inferred,
            "bytes_downloaded": 0u64,
            "total_bytes": init_total,
            "progress_pct": init_pct,
            "download_speed_bps": 0u64,
            "status": "downloading",
        })),
    ).into_response()
}

fn calc_speed(downloaded: u64, start: std::time::Instant) -> u64 {
    let elapsed = start.elapsed().as_secs_f64();
    if elapsed > 0.0 {
        (downloaded as f64 / elapsed) as u64
    } else {
        0
    }
}

async fn stream_download(
    client: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
    storage: &FsMsdStorage,
    bmc_id: &str,
    filename: &str,
    download_id: &str,
    event_tx: &tokio::sync::broadcast::Sender<String>,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), String> {
    debug!("[{}] stream_download: GET {} (download_id={})", bmc_id, url, download_id);
    let response = client.get(url).headers(headers).send().await
        .map_err(|e| {
            error!("[{}] download HTTP request failed: {} (download_id={})", bmc_id, e, download_id);
            format!("download failed: {}", e)
        })?;

    if !response.status().is_success() {
        warn!("[{}] download upstream returned {} (download_id={})", bmc_id, response.status(), download_id);
        return Err(format!("upstream status: {}", response.status()));
    }

    let total_size = response.content_length();
    let safe_name = FsMsdStorage::safe_filename(filename);
    info!("[{}] download started: url={}, filename={}, size={:?}, download_id={}", bmc_id, url, safe_name, total_size, download_id);
    let images_dir = storage.images_dir(bmc_id);
    let temp_name = format!(".{}.tmp", safe_name);
    let temp_path = images_dir.join(&temp_name);
    let final_path = images_dir.join(&safe_name);

    // Ensure temp file doesn't exist from a previous failed download
    let _ = tokio::fs::remove_file(&temp_path).await;

    let file = std::fs::File::create(&temp_path)
        .map_err(|e| {
            error!("[{}] create temp file failed: {} (download_id={})", bmc_id, e, download_id);
            format!("create temp file failed: {}", e)
        })?;
    let file = std::sync::Arc::new(file);

    // Pre-allocate disk space to avoid fragmentation and metadata churn
    if let Some(size) = total_size {
        let file = file.clone();
        tokio::task::spawn_blocking(move || file.set_len(size)).await
            .map_err(|e| format!("pre-allocate task failed: {}", e))?
            .map_err(|e| {
                error!("[{}] pre-allocate failed: {} (download_id={})", bmc_id, e, download_id);
                format!("pre-allocate failed: {}", e)
            })?;
    }

    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    let start_time = std::time::Instant::now();
    let mut last_progress_time = start_time;
    let mut buf: Vec<u8> = Vec::with_capacity(1024 * 1024);

    while let Some(chunk_result) = stream.next().await {
        if cancel_rx.try_recv().is_ok() {
            warn!("[{}] download cancelled: downloaded={}, download_id={}", bmc_id, downloaded, download_id);
            let _ = std::fs::remove_file(&temp_path);
            let event = serde_json::json!({
                "event": "msd.download_progress",
                "data": {
                    "download_id": download_id,
                    "filename": safe_name,
                    "bytes_downloaded": downloaded,
                    "total_bytes": total_size,
                    "progress_pct": total_size.map(|t| if t > 0 { downloaded as f64 / t as f64 * 100.0 } else { 0.0 }),
                    "download_speed_bps": calc_speed(downloaded, start_time),
                    "status": "cancelled",
                }
            });
            let _ = event_tx.send(event.to_string());
            return Ok(());
        }

        let chunk = chunk_result.map_err(|e| {
            error!("[{}] read chunk failed: {} (download_id={})", bmc_id, e, download_id);
            format!("read chunk failed: {}", e)
        })?;
        downloaded += chunk.len() as u64;
        buf.extend_from_slice(&chunk);

        if buf.len() >= 1024 * 1024 {
            let file = file.clone();
            let data = std::mem::take(&mut buf);
            tokio::task::spawn_blocking(move || {
                let mut f = &*file;
                f.write_all(&data)
            }).await
                .map_err(|e| format!("flush task failed: {}", e))?
                .map_err(|e| {
                    error!("[{}] write chunk failed: {} (download_id={})", bmc_id, e, download_id);
                    format!("write chunk failed: {}", e)
                })?;
        }

        if last_progress_time.elapsed() >= std::time::Duration::from_millis(100) {
            last_progress_time = std::time::Instant::now();
            let progress_pct = total_size.map(|t| {
                if t > 0 { downloaded as f64 / t as f64 * 100.0 } else { 0.0 }
            });

            let event = serde_json::json!({
                "event": "msd.download_progress",
                "data": {
                    "download_id": download_id,
                    "filename": safe_name,
                    "bytes_downloaded": downloaded,
                    "total_bytes": total_size,
                    "progress_pct": progress_pct,
                    "download_speed_bps": calc_speed(downloaded, start_time),
                    "status": "downloading",
                }
            });
            let _ = event_tx.send(event.to_string());
        }
    }

    // Flush remaining buffer
    if !buf.is_empty() {
        let file = file.clone();
        let data = std::mem::take(&mut buf);
        tokio::task::spawn_blocking(move || {
            let mut f = &*file;
            f.write_all(&data)
        }).await
            .map_err(|e| format!("flush task failed: {}", e))?
            .map_err(|e| {
                error!("[{}] write chunk failed: {} (download_id={})", bmc_id, e, download_id);
                format!("write chunk failed: {}", e)
            })?;
    }

    // Sync data to disk on a blocking thread
    let file2 = file.clone();
    tokio::task::spawn_blocking(move || file2.sync_all()).await
        .map_err(|e| format!("sync task failed: {}", e))?
        .map_err(|e| {
            error!("[{}] sync file failed: {} (download_id={})", bmc_id, e, download_id);
            format!("sync file failed: {}", e)
        })?;

    // Close (drop) the file handle — Arc will close when all clones are dropped
    drop(file);

    tokio::fs::rename(&temp_path, &final_path).await
        .map_err(|e| {
            error!("[{}] rename failed: {} (download_id={})", bmc_id, e, download_id);
            format!("rename failed: {}", e)
        })?;

    info!("[{}] download completed: {}, size={}, download_id={}", bmc_id, safe_name, downloaded, download_id);
    let event = serde_json::json!({
        "event": "msd.download_progress",
        "data": {
            "download_id": download_id,
            "filename": safe_name,
            "bytes_downloaded": downloaded,
            "total_bytes": total_size,
            "progress_pct": Some(100.0f64),
            "download_speed_bps": calc_speed(downloaded, start_time),
            "status": "completed",
        }
    });
    let _ = event_tx.send(event.to_string());

    Ok(())
}

#[derive(Deserialize)]
pub struct MsdDownloadCancelParams {
    pub download_id: Option<String>,
}

pub async fn msd_images_download_cancel(
    Path(_bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<MsdDownloadCancelParams>,
) -> Json<serde_json::Value> {
    if let Some(download_id) = &payload.download_id {
        if let Some(cancel_tx) = state.active_downloads.write().await.remove(download_id) {
            let _ = cancel_tx.send(());
            return Json(serde_json::json!({"success": true, "cancelled": true}));
        }
    }
    Json(serde_json::json!({"success": true, "cancelled": false}))
}

pub async fn msd_drive_info(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let _ = state.msd_storage.ensure_dirs(&bmc_id).await;
    let (size, used, free, capacity_mb) = state.msd_storage.drive_info(&bmc_id).await;
    Json(serde_json::json!({"size": size, "used": used, "free": free, "capacity_mb": capacity_mb}))
}

#[derive(Deserialize)]
pub struct MsdDriveInitParams {
    #[serde(default)]
    pub size_mb: Option<u64>,
}

pub async fn msd_drive_init(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<MsdDriveInitParams>,
) -> Json<serde_json::Value> {
    let _ = state.msd_storage.ensure_dirs(&bmc_id).await;
    let size_mb = payload.size_mb.unwrap_or(1024);
    let _ = state.update_config(|cfg| {
        let entry = cfg.msd.entry(bmc_id.clone()).or_insert_with(|| MsdDeviceConfig {
            backend: "mahogany".to_string(),
            drive_size_mb: 1024,
            storage_dir: None,
        });
        entry.drive_size_mb = size_mb;
    }).await;
    Json(serde_json::json!({"status": "ok", "size_mb": size_mb}))
}

pub async fn msd_drive_delete(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let _ = state.msd_storage.ensure_dirs(&bmc_id).await;
    let _ = state.msd_storage.drive_delete_all(&bmc_id).await;
    Json(serde_json::json!({"status": "ok"}))
}

#[derive(Deserialize)]
pub struct MsdDriveListQuery {
    #[serde(default)]
    pub path: Option<String>,
}

pub async fn msd_drive_files_list(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Query(query): Query<MsdDriveListQuery>,
) -> Json<serde_json::Value> {
    let _ = state.msd_storage.ensure_dirs(&bmc_id).await;
    let rel = query.path.unwrap_or_else(|| "/".to_string());
    let items = state.msd_storage.drive_files_list(&bmc_id, &rel).await;
    Json(serde_json::Value::Array(items))
}

pub async fn msd_drive_files_upload(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Query(query): Query<MsdDriveListQuery>,
    mut multipart: Multipart,
) -> Json<serde_json::Value> {
    let _ = state.msd_storage.ensure_dirs(&bmc_id).await;
    let rel = query.path.unwrap_or_else(|| "/".to_string());
    let mut name = format!("upload-{}.bin", uuid::Uuid::new_v4());
    let mut size = 0u64;

    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() != Some("file") {
            continue;
        }
        name = field.file_name().map(|s| crate::bmc::msd_storage::FsMsdStorage::safe_filename(&s)).unwrap_or(name);
        let bytes = match field.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                return Json(serde_json::json!({"status": "error", "message": format!("invalid multipart payload: {}", error)}));
            }
        };
        match state.msd_storage.drive_upload_file(&bmc_id, &rel, &name, &bytes).await {
            Ok(s) => size = s,
            Err(e) => return Json(serde_json::json!({"status": "error", "message": e})),
        }
        break;
    }

    if size == 0 {
        let _ = state.msd_storage.drive_upload_file(&bmc_id, &rel, &name, &[]);
    }

    Json(serde_json::json!({
        "status": "ok",
        "name": name,
        "size": size,
        "path": format!("{}/{}", rel.trim_end_matches('/'), name)
    }))
}

pub async fn msd_drive_files_delete(
    Path((bmc_id, path)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let _ = state.msd_storage.drive_delete_file(&bmc_id, &path).await;
    Json(serde_json::json!({"status": "ok"}))
}

pub async fn msd_drive_mkdir(
    Path((bmc_id, path)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let _ = state.msd_storage.drive_mkdir(&bmc_id, &path).await;
    Json(serde_json::json!({"status": "ok"}))
}
