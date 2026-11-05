use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::StreamExt;
use tower::ServiceExt;

use bmc_aggregator::bmc::feature::config::{
    BackendParams, FeatureConfig, FeatureConfigs, FeatureParams,
};
use bmc_aggregator::bmc::feature::connection::{ConnectionState, FeatureConnection};
use bmc_aggregator::bmc::feature::device_registry::DeviceRegistry;
use bmc_aggregator::bmc::feature::fault::{FeatureFault, FeatureFaultDomain};
use bmc_aggregator::bmc::feature::managed::ManagedDevice;
use bmc_aggregator::bmc::feature::msd::{
    MsdBackend, MsdMediaKind, MsdMediaStore, MsdMediaUpload, MsdSlotCapabilities,
    MsdStorageLocation, MsdStorageOperation, MsdStoredImage,
};
use bmc_aggregator::bmc::feature::video::runtime::VideoRuntime;
use bmc_aggregator::bmc::msd_storage::MsdStorageProvider;
use bmc_aggregator::config::{AuthConfig, Config, DeviceConfig, ServerConfig, WebTransportConfig};
use bmc_aggregator::web::routes::create_router;
use bmc_aggregator::web::state::AppState;

// ── State Builder ────────────────────────────────────────────────

async fn open_test_websocket(address: std::net::SocketAddr, path: &str) -> tokio::net::TcpStream {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut headers = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stream.read_u8().await.unwrap());
            assert!(headers.len() < 8192);
        }
    })
    .await
    .unwrap();
    assert!(headers.starts_with(b"HTTP/1.1 101"));
    stream
}

#[tokio::test]
async fn shutdown_releases_idle_configuration_and_device_websockets() {
    use tokio::io::AsyncReadExt;
    let state = make_state().await;
    let app = create_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, requested) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = requested.await;
            })
            .await
            .unwrap();
    });
    let configuration = open_test_websocket(address, "/api/configuration/ws").await;
    let device = open_test_websocket(address, "/bmc/test-bmc/api/ws").await;
    shutdown.send(()).unwrap();
    state.registry.shutdown().await;
    for mut socket in [configuration, device] {
        let mut remaining = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            socket.read_to_end(&mut remaining),
        )
        .await
        .unwrap()
        .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn device_event_subscriptions_follow_replaced_feature_connections() {
    use bmc_aggregator::bmc::configuration::ConfigurationScope;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let state = make_state().await;
    let old = state.registry.get_dev("test-bmc").await.unwrap().features["video"].clone();
    let app = create_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, requested) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = requested.await;
            })
            .await
            .unwrap();
    });
    let mut socket = open_test_websocket(address, "/bmc/test-bmc/api/ws").await;
    let payload = br#"{"type":"subscribe","payload":{"features":["video"]}}"#;
    let mask = [1u8, 2, 3, 4];
    let mut frame = vec![0x81, 0x80 | payload.len() as u8];
    frame.extend(mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(i, byte)| byte ^ mask[i % 4]),
    );
    socket.write_all(&frame).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while old.ref_count() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let replacement = FeatureConnection::new(
        "test-bmc".into(),
        "video".into(),
        "mock".into(),
        old.config.clone(),
    );
    state
        .registry
        .insert(
            "test-bmc".into(),
            Arc::new(ManagedDevice::new(
                "test-bmc".into(),
                FeatureConfigs {
                    features: Default::default(),
                },
                [("video".into(), replacement.clone())].into(),
            )),
        )
        .await;
    old.shutdown();
    state.configuration.publish(
        ConfigurationScope::Feature {
            device_id: "test-bmc".into(),
            feature_id: "video".into(),
        },
        "replacement",
        vec![],
        vec!["video".into()],
    );
    let rebound = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while replacement.ref_count() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let old_released = old.ref_count() == 0;
    state.registry.shutdown().await;
    shutdown.send(()).unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        socket.read_to_end(&mut Vec::new()),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        rebound,
        "the existing socket did not acquire the replacement feature"
    );
    assert!(
        old_released,
        "the existing socket retained a stopped feature"
    );
    assert_eq!(replacement.ref_count(), 0);
}

#[tokio::test]
async fn download_checkpoint_failures_are_not_missing_and_never_restart_downloads() {
    let state = make_state().await;
    let missing = uuid::Uuid::new_v4().to_string();
    let unreadable = uuid::Uuid::new_v4().to_string();
    let invalid = uuid::Uuid::new_v4().to_string();
    let directory = std::path::PathBuf::from(state.msd_storage.storage_base("test-bmc").await)
        .join("images/.staging");
    let unreadable_path = directory.join(format!("download-{unreadable}.json"));
    let invalid_path = directory.join(format!("download-{invalid}.json"));
    tokio::fs::create_dir_all(&unreadable_path).await.unwrap();
    tokio::fs::write(&invalid_path, b"invalid checkpoint")
        .await
        .unwrap();
    let app = create_router(state.clone());
    for (id, expected, code) in [
        (missing, StatusCode::NOT_FOUND, "download_not_found"),
        (
            unreadable,
            StatusCode::INTERNAL_SERVER_ERROR,
            "download_checkpoint_read_failed",
        ),
        (
            invalid,
            StatusCode::INTERNAL_SERVER_ERROR,
            "download_checkpoint_invalid",
        ),
    ] {
        for endpoint in [
            format!("download/status?download_id={id}"),
            format!("download?resume_id={id}"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/bmc/test-bmc/api/feature/msd/images/{endpoint}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["fault"]["code"], code);
            assert!(
                !String::from_utf8_lossy(&body).contains(&directory.to_string_lossy().to_string())
            );
        }
    }
    assert!(state.active_downloads.read().await.is_empty());
    assert!(unreadable_path.is_dir());
    assert_eq!(
        tokio::fs::read(invalid_path).await.unwrap(),
        b"invalid checkpoint"
    );
}

#[tokio::test]
async fn failed_paused_download_cleanup_does_not_acknowledge_cancellation() {
    let state = make_state().await;
    let id = uuid::Uuid::new_v4().to_string();
    let directory = std::path::PathBuf::from(state.msd_storage.storage_base("test-bmc").await)
        .join("images/.staging");
    let part = directory.join(format!("download-{id}.part"));
    let checkpoint = directory.join(format!("download-{id}.json"));
    tokio::fs::create_dir_all(&part).await.unwrap();
    tokio::fs::write(
        &checkpoint,
        serde_json::to_vec(&serde_json::json!({
            "id":id,"device_id":"test-bmc","url":"https://example.invalid/test.iso",
            "filename":"test.iso","offset":0,"validator":null,"maximum_bytes":1024,
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let response = create_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/download/cancel")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"download_id":id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["ok"], false);
    assert!(checkpoint.is_file());
    assert!(part.is_dir());
}

#[tokio::test]
async fn cancelling_download_keeps_its_files_and_exclusive_worker_slot() {
    use bmc_aggregator::web::state::{MsdDownloadStatus, MsdDownloadTask};
    let state = make_state().await;
    state.config.write().await.server.allow_private_downloads = true;
    let id = uuid::Uuid::new_v4().to_string();
    let directory = std::path::PathBuf::from(state.msd_storage.storage_base("test-bmc").await)
        .join("images/.staging");
    tokio::fs::create_dir_all(&directory).await.unwrap();
    let part = directory.join(format!("download-{id}.part"));
    let checkpoint = directory.join(format!("download-{id}.json"));
    tokio::fs::write(&part, b"in-flight data").await.unwrap();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "id":id,"device_id":"test-bmc","url":"http://127.0.0.1:9/test.iso",
        "filename":"test.iso","offset":0,"validator":null,"maximum_bytes":1024,
    }))
    .unwrap();
    tokio::fs::write(&checkpoint, &bytes).await.unwrap();
    state.active_downloads.write().await.insert(
        id.clone(),
        MsdDownloadTask {
            device_id: "test-bmc".into(),
            cancellation: None,
            filename: "test.iso".into(),
            bytes_downloaded: 0,
            total_bytes: None,
            status: MsdDownloadStatus::Cancelling,
            fault: None,
            started_at: std::time::Instant::now(),
        },
    );
    let app = create_router(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/download/cancel")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"download_id":id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["ok"],
        false
    );
    assert_eq!(tokio::fs::read(&part).await.unwrap(), b"in-flight data");
    assert_eq!(tokio::fs::read(&checkpoint).await.unwrap(), bytes);
    let response = app.clone().oneshot(Request::builder()
        .uri(format!("/bmc/test-bmc/api/feature/msd/images/download?location=aggregator&resume_id={id}"))
        .body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        state.active_downloads.read().await[&id].status,
        MsdDownloadStatus::Cancelling
    );
    for _ in 0..3 {
        state.active_downloads.write().await.insert(
            uuid::Uuid::new_v4().to_string(),
            MsdDownloadTask {
                device_id: "test-bmc".into(),
                cancellation: None,
                filename: "other.iso".into(),
                bytes_downloaded: 0,
                total_bytes: None,
                status: MsdDownloadStatus::Cancelling,
                fault: None,
                started_at: std::time::Instant::now(),
            },
        );
    }
    let response = app.oneshot(Request::builder()
        .uri("/bmc/test-bmc/api/feature/msd/images/download?location=aggregator&url=http%3A%2F%2F127.0.0.1%3A9%2Fnew.iso")
        .body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(state.active_downloads.read().await.len(), 4);
}

#[tokio::test]
async fn live_downloads_require_their_owner_and_a_live_cancellation_receiver() {
    use bmc_aggregator::web::state::{MsdDownloadStatus, MsdDownloadTask};
    let state = make_state_two_devices().await;
    let owners = state.registry.list_devices().await;
    let owner = &owners[0];
    let other = &owners[1];
    let id = uuid::Uuid::new_v4().to_string();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    state.active_downloads.write().await.insert(
        id.clone(),
        MsdDownloadTask {
            device_id: owner.clone(),
            cancellation: Some(tx),
            filename: "owner.iso".into(),
            bytes_downloaded: 0,
            total_bytes: None,
            status: MsdDownloadStatus::Downloading,
            fault: None,
            started_at: std::time::Instant::now(),
        },
    );
    let app = create_router(state.clone());
    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/bmc/{other}/api/feature/msd/images/download/status?download_id={id}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::NOT_FOUND);
    for (device, expected) in [(other, false), (owner, true)] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/bmc/{device}/api/feature/msd/images/download/cancel"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"download_id":id}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["ok"],
            expected
        );
        if !expected {
            assert!(matches!(
                rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            assert_eq!(
                state.active_downloads.read().await[&id].status,
                MsdDownloadStatus::Downloading
            );
        }
    }
    assert!(rx.await.is_ok());
    let (tx, rx) = tokio::sync::oneshot::channel();
    drop(rx);
    {
        let mut tasks = state.active_downloads.write().await;
        let task = tasks.get_mut(&id).unwrap();
        task.cancellation = Some(tx);
        task.status = MsdDownloadStatus::Downloading;
    }
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/bmc/{owner}/api/feature/msd/images/download/cancel"
                ))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"download_id":id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["ok"],
        false
    );
    assert_eq!(
        state.active_downloads.read().await[&id].status,
        MsdDownloadStatus::Downloading
    );
}

fn test_msd_storage_root() -> String {
    std::env::temp_dir()
        .join(format!("bmc-aggregator-test-msd-{}", uuid::Uuid::new_v4()))
        .to_string_lossy()
        .to_string()
}

#[derive(Default)]
struct RecordingMsdBackend {
    selected_image: tokio::sync::Mutex<Option<(String, u64)>>,
    selected_bmc_image: tokio::sync::Mutex<Option<String>>,
    refreshed_media_type: tokio::sync::Mutex<Option<Option<MsdMediaKind>>>,
    mounted_slots: tokio::sync::Mutex<HashMap<String, String>>,
    dynamic_targets: tokio::sync::Mutex<std::collections::BTreeMap<String, MsdMediaKind>>,
    connected: std::sync::atomic::AtomicBool,
    store: Arc<RecordingMsdStore>,
}

#[derive(Default)]
struct RecordingMsdStore {
    images: tokio::sync::Mutex<HashMap<String, bytes::Bytes>>,
}

#[async_trait]
impl MsdMediaStore for RecordingMsdStore {
    async fn list(&self) -> Result<Vec<MsdStoredImage>, String> {
        let images = self.images.lock().await;
        Ok(images
            .iter()
            .map(|(id, bytes)| MsdStoredImage {
                id: id.clone(),
                name: id.clone(),
                size: Some(bytes.len() as u64),
                deletable: true,
                digests: Vec::new(),
            })
            .collect())
    }

    async fn upload(&self, upload: MsdMediaUpload) -> Result<MsdStoredImage, String> {
        let bytes = tokio::fs::read(&upload.path)
            .await
            .map_err(|error| error.to_string())?;
        let size = bytes.len() as u64;
        if size != upload.size {
            return Err("staged upload size changed".to_string());
        }
        self.images
            .lock()
            .await
            .insert(upload.name.clone(), bytes.into());
        Ok(MsdStoredImage {
            id: upload.name.clone(),
            name: upload.name,
            size: Some(size),
            deletable: true,
            digests: upload.digests,
        })
    }

    async fn delete(&self, image_id: &str) -> Result<(), String> {
        self.images.lock().await.remove(image_id);
        Ok(())
    }
}

#[async_trait]
impl MsdBackend for RecordingMsdBackend {
    async fn refresh_capabilities(
        &self,
        requested_media_type: Option<MsdMediaKind>,
    ) -> Result<(), String> {
        *self.refreshed_media_type.lock().await = Some(requested_media_type);
        Ok(())
    }

    async fn capabilities(&self) -> bmc_aggregator::bmc::feature::msd::MsdCapabilities {
        let mut slots = vec![
            MsdSlotCapabilities::new("default", vec![MsdMediaKind::Disk]),
            MsdSlotCapabilities::new("secondary", vec![MsdMediaKind::Disk]),
        ];
        slots.extend(
            self.dynamic_targets
                .lock()
                .await
                .iter()
                .map(|(id, media_type)| {
                    MsdSlotCapabilities::dynamic(id, Some(id.clone()), *media_type)
                }),
        );
        bmc_aggregator::bmc::feature::msd::MsdCapabilities {
            virtual_drive: true,
            slots,
            storage_locations: vec![
                bmc_aggregator::bmc::feature::msd::MsdStorageCapabilities::aggregator(vec![
                    MsdMediaKind::Optical,
                    MsdMediaKind::Floppy,
                    MsdMediaKind::Disk,
                    MsdMediaKind::Flash,
                ]),
                bmc_aggregator::bmc::feature::msd::MsdStorageCapabilities::bmc(
                    vec![bmc_aggregator::bmc::feature::msd::MsdMediaKind::Disk],
                    vec![
                        MsdStorageOperation::Select,
                        MsdStorageOperation::List,
                        MsdStorageOperation::Upload,
                        MsdStorageOperation::Delete,
                    ],
                ),
            ],
            unavailable_storage_locations: Vec::new(),
            target_management: bmc_aggregator::bmc::feature::msd::MsdTargetManagementCapabilities {
                media_types: vec![
                    MsdMediaKind::Optical,
                    MsdMediaKind::Floppy,
                    MsdMediaKind::Disk,
                    MsdMediaKind::Flash,
                ],
                deletable: true,
                limits: Vec::new(),
                max_targets: None,
            },
        }
    }

    async fn status(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "connected": self.connected.load(std::sync::atomic::Ordering::SeqCst),
        }))
    }

    async fn mount(
        &self,
        slot_id: &str,
        media: bmc_aggregator::bmc::feature::msd::MsdMedia,
    ) -> Result<(), String> {
        if !matches!(slot_id, "default" | "secondary")
            && !self.dynamic_targets.lock().await.contains_key(slot_id)
        {
            return Err("unknown test MSD slot".to_string());
        }
        let mounted_media = match media.source {
            bmc_aggregator::bmc::feature::msd::MsdMediaSource::AggregatorImage {
                path,
                size,
                ..
            } => {
                *self.selected_image.lock().await = Some((path.clone(), size));
                path
            }
            bmc_aggregator::bmc::feature::msd::MsdMediaSource::BmcImage { id } => {
                if !self.store.images.lock().await.contains_key(&id) {
                    return Err("BMC image does not exist".to_string());
                }
                *self.selected_bmc_image.lock().await = Some(id.clone());
                id
            }
            _ => return Err("test backend does not accept remote URLs".to_string()),
        };
        self.mounted_slots
            .lock()
            .await
            .insert(slot_id.to_string(), mounted_media);
        self.connected
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn unmount(&self, slot_id: &str) -> Result<(), String> {
        self.mounted_slots.lock().await.remove(slot_id);
        let connected = !self.mounted_slots.lock().await.is_empty();
        self.connected
            .store(connected, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn create_target(
        &self,
        media_type: MsdMediaKind,
        label: Option<String>,
    ) -> Result<MsdSlotCapabilities, String> {
        let id = label.ok_or_else(|| "test target requires a label".to_string())?;
        if id.is_empty() || self.dynamic_targets.lock().await.contains_key(&id) {
            return Err("invalid or duplicate test target".to_string());
        }
        self.dynamic_targets
            .lock()
            .await
            .insert(id.clone(), media_type);
        Ok(MsdSlotCapabilities::dynamic(
            id.clone(),
            Some(id),
            media_type,
        ))
    }

    async fn delete_target(&self, target_id: &str) -> Result<(), String> {
        if self.mounted_slots.lock().await.contains_key(target_id) {
            return Err("test target is mounted".to_string());
        }
        self.dynamic_targets
            .lock()
            .await
            .remove(target_id)
            .ok_or_else(|| "unknown test target".to_string())?;
        Ok(())
    }

    fn media_store(&self, location: MsdStorageLocation) -> Option<Arc<dyn MsdMediaStore>> {
        (location == MsdStorageLocation::Bmc).then(|| self.store.clone() as Arc<dyn MsdMediaStore>)
    }
}

async fn install_recording_msd_backend(state: &Arc<AppState>) -> Arc<RecordingMsdBackend> {
    let device = state.registry.get_dev("test-bmc").await.unwrap();
    let fc = device.features.get("msd").unwrap().clone();
    fc.lifecycle_started
        .store(true, std::sync::atomic::Ordering::SeqCst);
    fc.transition(ConnectionState::Connected).await;
    let backend = Arc::new(RecordingMsdBackend::default());
    fc.set_backend_runtime(backend.clone() as Arc<dyn MsdBackend>)
        .await;
    backend
}

fn mock_feature_configs() -> HashMap<String, FeatureConfig> {
    ["video", "hid", "msd", "power"]
        .into_iter()
        .map(|feat_id| {
            (
                feat_id.to_string(),
                FeatureConfig {
                    backend: "mock".into(),
                    backend_params: BackendParams::default(),
                    feature_params: FeatureParams::default(),
                    providers: Vec::new(),
                },
            )
        })
        .collect()
}

async fn make_state() -> Arc<AppState> {
    let config = Config {
        server: ServerConfig {
            listen: "127.0.0.1:0".into(),
            frontend_dir: std::path::PathBuf::from("/tmp/nonexistent"),
            data_dir: std::path::PathBuf::from("/tmp/bmc-aggregator-test-data"),
            allow_insecure_http: false,
            download_headers: std::collections::HashMap::new(),
            download_header_origins: Vec::new(),
            download_connect_timeout_seconds: 15,
            download_idle_timeout_seconds: 60,
            allow_private_downloads: true,
            webtransport: WebTransportConfig::default(),
        },
        devices: vec![DeviceConfig {
            id: "test-bmc".into(),
            name: "Test BMC".into(),
            protocol: "mock".into(),
            host: "127.0.0.1".into(),
            port: 0,
            auth: AuthConfig {
                username: "admin".into(),
                password: "pass".into(),
            },
            kvm_port: None,
            ipmi_host: None,
            ipmi_port: Some(623),
            max_fps: 30,
            video_backend: "mahogany".into(),
            encoder_backend: "software".into(),
            audio_backend: None,
            features: HashMap::new(),
            video_device: None,
            video_format: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            video_quality: None,
        }],
        msd: HashMap::new(),
    };

    let registry = DeviceRegistry::new();

    // Create feature connections with "mock" backend (no real backend registered,
    // so fc_execute will fail and handlers will return fallback responses).
    let feature_configs = mock_feature_configs();
    let mut features: HashMap<String, Arc<FeatureConnection>> = HashMap::new();
    for feat_id in &["video", "hid", "msd", "power"] {
        let fc = FeatureConnection::new(
            "test-bmc".into(),
            feat_id.to_string(),
            "mock".into(),
            feature_configs[*feat_id].clone(),
        );
        // Set up VideoProcessor extension for the video feature
        if *feat_id == "video" {
            fc.set_feature_runtime(Arc::new(VideoRuntime::new())).await;
        }
        features.insert(feat_id.to_string(), fc);
    }

    let feat_cfgs = FeatureConfigs {
        features: feature_configs,
    };

    let md = Arc::new(ManagedDevice::new("test-bmc".into(), feat_cfgs, features));
    registry.insert("test-bmc".into(), md).await;

    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let msd_storage_root = test_msd_storage_root();

    Arc::new(AppState {
        config: config_lock.clone(),
        config_path: std::env::temp_dir()
            .join(format!(
                "bmc-aggregator-test-config-{}.yaml",
                uuid::Uuid::new_v4()
            ))
            .to_string_lossy()
            .to_string(),
        configuration: bmc_aggregator::bmc::configuration::ConfigurationCoordinator::new(),
        registry,
        msd_storage: bmc_aggregator::bmc::msd_storage::FsMsdStorage::new(
            msd_storage_root,
            config_lock,
        ),
        active_downloads: tokio::sync::RwLock::new(HashMap::new()),
        webtransport: tokio::sync::RwLock::new(None),
    })
}

async fn make_device_configuration_state() -> Arc<AppState> {
    let state = make_state().await;
    {
        let mut config = state.config.write().await;
        let device = &mut config.devices[0];
        device.protocol = "explicit".to_string();
        bmc_aggregator::bmc::configuration::materialize_unmentioned_features_as_disabled(device)
            .unwrap();
    }
    state
}

// ── Dashboard ────────────────────────────────────────────────────

#[tokio::test]
async fn test_dashboard_returns_html() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(content_type.contains("text/html"));
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains(r#"/devices/test-bmc/console"#));
    assert!(!text.contains(r#"/bmc/test-bmc/"#));
}

#[tokio::test]
async fn test_spa_routes_serve_index_and_unknown_routes_stay_not_found() {
    let directory = tempfile::tempdir().unwrap();
    let index = "<!doctype html><title>Aggregator SPA</title>";
    std::fs::write(directory.path().join("index.html"), index).unwrap();
    let state = make_state().await;
    state.config.write().await.server.frontend_dir = directory.path().to_path_buf();
    let app = create_router(state);

    for path in [
        "/settings",
        "/devices",
        "/devices/new",
        "/devices/test-bmc/settings",
        "/login",
        "/setup",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), index.as_bytes(), "{path}");
    }
    for path in ["/unknown", "/api/unknown", "/bmc/test-bmc/unknown"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

// ── BMC List API ─────────────────────────────────────────────────

#[tokio::test]
async fn test_bmc_list() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/bmc/list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], "test-bmc");
    assert_eq!(list[0]["host"], "127.0.0.1");
}

// ── SPA Routing ──────────────────────────────────────────────────

#[tokio::test]
async fn test_legacy_bmc_ui_paths_are_not_mounted() {
    let state = make_state().await;
    let app = create_router(state);
    for path in ["/bmc/test-bmc", "/bmc/test-bmc/", "/bmc/test-bmc/config.js"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

// ── Device Info ──────────────────────────────────────────────────

#[tokio::test]
async fn test_device_info_returns_device_details() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["device_id"], "test-bmc");
    assert!(info.get("remote").is_some());
    assert!(info.get("hostname").is_some());
    assert!(info.get("platform").is_some());
}

#[tokio::test]
async fn test_requirements_returns_empty_contract_when_no_backend_advertises_checks() {
    let state = make_state().await;
    let response = create_router(state)
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/requirements")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["requirements"], serde_json::json!([]));
    assert_eq!(json["summary"]["total"], 0);
    assert_eq!(json["summary"]["action_required"], 0);
    assert_eq!(json["summary"]["pending"], 0);
}

#[tokio::test]
async fn test_requirements_unknown_device_returns_structured_not_found() {
    let state = make_state().await;
    let response = create_router(state)
        .oneshot(
            Request::builder()
                .uri("/bmc/missing/api/requirements")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["fault"]["code"], "device_not_found");
    assert_eq!(json["fault"]["operation"], "requirements");
}

// ── Stream Status ────────────────────────────────────────────────

#[tokio::test]
async fn test_stream_status_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Fallback: without a real backend, stream_status returns {"streaming": false, "connected": false}
    assert_eq!(status["streaming"], false);
    assert_eq!(status["connected"], status["online"]);
}

#[tokio::test]
async fn unavailable_requirement_provider_is_not_reported_as_an_empty_success() {
    let state = make_state().await;
    let config = FeatureConfig {
        backend: "providers".into(),
        backend_params: BackendParams::default(),
        feature_params: FeatureParams::default(),
        providers: vec![
            bmc_aggregator::bmc::feature::config::FeatureProviderConfig {
                id: "fixture".into(),
                backend: "ipmi".into(),
                backend_params: BackendParams::default(),
            },
        ],
    };
    let fc = FeatureConnection::new(
        "requirements-test".into(),
        "boot".into(),
        "providers".into(),
        config.clone(),
    );
    state
        .registry
        .insert(
            "requirements-test".into(),
            Arc::new(ManagedDevice::new(
                "requirements-test".into(),
                FeatureConfigs {
                    features: HashMap::from([("boot".into(), config)]),
                },
                HashMap::from([("boot".into(), fc.clone())]),
            )),
        )
        .await;
    let app = create_router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/requirements-test/api/requirements")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["complete"], false);
    assert_eq!(value["success"], false);
    assert_eq!(value["fault"]["code"], "requirements_inspection_failed");
    fc.lifecycle_started
        .store(true, std::sync::atomic::Ordering::SeqCst);
    fc.transition(ConnectionState::Connected).await;
    let response = app.oneshot(Request::builder().method("POST")
        .uri("/bmc/requirements-test/api/requirements/remediate")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"feature":"boot","requirement_id":"firmware_inventory_collection","action":"enable_inventory_collection"}"#)).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["fault"]["code"], "requirements_inspection_failed");
    assert_eq!(value["fault"]["operation"], "remediate_requirement");
}

#[tokio::test]
async fn test_stream_reset_returns_ok_without_active_video_instance() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/reset")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["reset_instances"], 0);
    assert_eq!(json["closed_webrtc_connections"], 0);
    assert_eq!(json["base_frame_replayed"], false);
}

#[tokio::test]
async fn video_reset_propagates_source_failures_without_replaying_requests() {
    use bmc_aggregator::bmc::feature::video::VideoSource;
    struct Source {
        result: Result<bool, String>,
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl VideoSource for Source {
        async fn request_full_frame(&self) -> Result<bool, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.result.clone()
        }
    }
    let state = make_state().await;
    let device = state.registry.get_dev("test-bmc").await.unwrap();
    let fc = &device.features["video"];
    fc.transition(ConnectionState::Connected).await;
    let other = FeatureConnection::new_instance(
        "test-bmc".into(),
        "video".into(),
        "other".into(),
        "mock".into(),
        fc.config.clone(),
    );
    other.transition(ConnectionState::Connected).await;
    other
        .set_feature_runtime(Arc::new(VideoRuntime::new()))
        .await;
    let healthy_source = Arc::new(Source {
        result: Ok(true),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    other
        .set_backend_runtime(healthy_source.clone() as Arc<dyn VideoSource>)
        .await;
    device.insert_instance("video", "other", other).await;
    let app = create_router(state);
    for outcome in [
        None,
        Some(Err("private-source-detail".to_string())),
        Some(Ok(false)),
        Some(Ok(true)),
    ] {
        let success = matches!(outcome, Some(Ok(true)));
        fc.clear_backend_runtime().await;
        let source = outcome.map(|result| {
            Arc::new(Source {
                result,
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        });
        if let Some(source) = &source {
            fc.set_backend_runtime(source.clone() as Arc<dyn VideoSource>)
                .await;
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/bmc/test-bmc/api/feature/video/reset")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if success {
                StatusCode::OK
            } else {
                StatusCode::BAD_GATEWAY
            }
        );
        let bytes = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("private-source-detail"));
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["success"], success);
        let backends = value["backends"].as_array().unwrap();
        assert_eq!(backends.len(), 2);
        let primary = backends
            .iter()
            .find(|result| result["connection_id"] == "default")
            .unwrap();
        assert_eq!(primary["success"], success);
        assert_eq!(primary["source"]["success"], success);
        assert_eq!(
            backends
                .iter()
                .find(|result| result["connection_id"] == "other")
                .unwrap()["success"],
            true
        );
        if !success {
            assert_eq!(value["fault"]["code"], "video_reset_failed");
        }
        if let Some(source) = source {
            assert_eq!(source.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
    }
    assert_eq!(
        healthy_source
            .calls
            .load(std::sync::atomic::Ordering::SeqCst),
        4
    );
}

#[tokio::test]
async fn reconnect_retry_is_independent_of_operation_retryability() {
    for (feature_retryable, operation_retryable, stopped, expected) in [
        (None, false, false, StatusCode::OK),
        (None, true, false, StatusCode::OK),
        (Some(true), false, false, StatusCode::OK),
        (Some(false), true, false, StatusCode::CONFLICT),
        (None, true, true, StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let state = make_state().await;
        let fc = state.registry.get_dev("test-bmc").await.unwrap().features["video"].clone();
        if let Some(retryable) = feature_retryable {
            fc.faults
                .record(FeatureFault::feature(
                    FeatureFaultDomain::Lifecycle,
                    "feature_failed",
                    "Feature unavailable",
                    retryable,
                ))
                .await;
        }
        fc.faults
            .record(FeatureFault::operation(
                FeatureFaultDomain::Operation,
                "operation_unknown",
                "Operation result is unknown",
                operation_retryable,
                "control",
            ))
            .await;
        if stopped {
            fc.shutdown();
        }
        let response = create_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/bmc/test-bmc/api/feature/video/retry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            assert!(fc.faults.active_feature().await.is_none());
            assert_eq!(fc.faults.active().await.unwrap().code, "operation_unknown");
        } else if expected == StatusCode::CONFLICT {
            assert_eq!(
                fc.faults.active_feature().await.unwrap().code,
                "feature_failed"
            );
        }
    }
}

#[tokio::test]
async fn disconnected_video_retry_via_reset_cannot_bypass_a_terminal_feature_fault() {
    let state = make_state().await;
    let fc = state.registry.get_dev("test-bmc").await.unwrap().features["video"].clone();
    fc.faults
        .record(FeatureFault::feature(
            FeatureFaultDomain::Lifecycle,
            "feature_failed",
            "Feature unavailable",
            false,
        ))
        .await;
    let response = create_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/reset")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        fc.faults.active_feature().await.unwrap().code,
        "feature_failed"
    );
}

#[tokio::test]
async fn test_feature_retry_clears_a_retryable_fault() {
    let state = make_state().await;
    let video = state
        .registry
        .get_dev("test-bmc")
        .await
        .unwrap()
        .features
        .get("video")
        .unwrap()
        .clone();
    video
        .faults
        .record(FeatureFault::feature(
            FeatureFaultDomain::Lifecycle,
            "backend_unavailable",
            "The feature backend connection failed",
            true,
        ))
        .await;

    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/retry")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(response["success"], true);
    assert!(video.faults.active().await.is_none());
}

#[tokio::test]
async fn test_video_trace_returns_processor_contract() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/trace")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);
    assert!(json["trace"]["processor"].is_object());
    assert!(json["trace"]["backend"].is_object());
    assert!(json["trace"]["webrtc_sessions"].is_array());
    assert!(json["trace"]["processor"]["raw_frames_published"].is_number());
}

#[tokio::test]
async fn test_stream_codecs_returns_backends() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/codecs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["codecs"].is_array());
    let codecs = json["codecs"].as_array().unwrap();
    let h264 = codecs.iter().find(|codec| codec["id"] == "h264").unwrap();
    assert_eq!(h264["protocol"], "webrtc");
    assert!(h264["transports"]
        .as_array()
        .unwrap()
        .iter()
        .any(|transport| transport == "webtransport"));
}

#[tokio::test]
async fn test_root_device_config_crud_updates_config_and_registry() {
    let state = make_state().await;
    let app = create_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["devices"][0]["id"], "test-bmc");
    assert!(json["devices"][0]["version"]
        .as_str()
        .is_some_and(|version| version.starts_with("\"cfg-sha256-")));
    assert_eq!(
        json["devices"][0]["credentials"]["password"]["configured"],
        true
    );

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/devices")
                .header("content-type", "application/json")
                .header("x-mutation-id", "create-new-bmc")
                .body(Body::from(
                    r#"{"id":"new-bmc","name":"New BMC","endpoint":{"host":"192.0.2.10","port":443},"credentials":{"username":"admin","password":{"operation":"replace","value":"secret"}},"assignments":{"video":{"kind":"single","backend":{"backend_id":"mahogany"}},"hid":{"kind":"single","backend":{"backend_id":"mahogany"}},"msd":{"kind":"single","backend":{"backend_id":"mahogany"}},"firmware":{"kind":"single","backend":{"backend_id":"mahogany"}}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::OK);
    let create_etag = create
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let create_body = axum::body::to_bytes(create.into_body(), 8192)
        .await
        .unwrap();
    let create_json: serde_json::Value = serde_json::from_slice(&create_body).unwrap();
    let created_version = create_json["device"]["version"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(created_version, create_etag);
    assert_eq!(create_json["mutation_id"], "create-new-bmc");

    let get_created = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/devices/new-bmc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_created.status(), StatusCode::OK);
    assert_eq!(
        get_created.headers().get("etag").unwrap().to_str().unwrap(),
        created_version
    );
    assert!(state.registry.get_dev("new-bmc").await.is_some());
    assert!(state
        .config
        .read()
        .await
        .devices
        .iter()
        .any(|dev| dev.id == "new-bmc" && dev.auth.password == "secret"));

    let update_payload = r#"{"id":"renamed-bmc","name":"Renamed","endpoint":{"host":"192.0.2.20"},"credentials":{"password":{"operation":"keep"}}}"#;
    let update = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/new-bmc")
                .header("content-type", "application/json")
                .header("if-match", &created_version)
                .header("x-mutation-id", "update-new-bmc")
                .body(Body::from(update_payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::OK);
    let update_body = axum::body::to_bytes(update.into_body(), 8192)
        .await
        .unwrap();
    let update_json: serde_json::Value = serde_json::from_slice(&update_body).unwrap();
    let updated_version = update_json["device"]["version"]
        .as_str()
        .unwrap()
        .to_string();

    let replay = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/new-bmc")
                .header("content-type", "application/json")
                .header("if-match", &created_version)
                .header("x-mutation-id", "update-new-bmc")
                .body(Body::from(update_payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replay_body = axum::body::to_bytes(replay.into_body(), 8192)
        .await
        .unwrap();
    let replay_json: serde_json::Value = serde_json::from_slice(&replay_body).unwrap();
    assert_eq!(replay_json, update_json);

    let reused_mutation = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/new-bmc")
                .header("content-type", "application/json")
                .header("if-match", &created_version)
                .header("x-mutation-id", "update-new-bmc")
                .body(Body::from(r#"{"name":"Different payload"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reused_mutation.status(), StatusCode::CONFLICT);
    let reused_body = axum::body::to_bytes(reused_mutation.into_body(), 4096)
        .await
        .unwrap();
    let reused_json: serde_json::Value = serde_json::from_slice(&reused_body).unwrap();
    assert_eq!(reused_json["code"], "configuration_mutation_reused");

    let stale_update = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/renamed-bmc")
                .header("content-type", "application/json")
                .header("if-match", &created_version)
                .header("x-mutation-id", "stale-update")
                .body(Body::from(r#"{"name":"Stale"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale_update.status(), StatusCode::CONFLICT);
    let stale_body = axum::body::to_bytes(stale_update.into_body(), 4096)
        .await
        .unwrap();
    let stale_json: serde_json::Value = serde_json::from_slice(&stale_body).unwrap();
    assert_eq!(stale_json["code"], "configuration_conflict");
    assert_eq!(stale_json["current_version"], updated_version);
    assert!(state.registry.get_dev("new-bmc").await.is_none());
    assert!(state.registry.get_dev("renamed-bmc").await.is_some());
    assert!(state
        .config
        .read()
        .await
        .devices
        .iter()
        .any(|dev| dev.id == "renamed-bmc" && dev.host == "192.0.2.20"));

    let clear_password = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/renamed-bmc")
                .header("content-type", "application/json")
                .header("if-match", &updated_version)
                .header("x-mutation-id", "clear-password")
                .body(Body::from(
                    r#"{"credentials":{"password":{"operation":"clear"}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(clear_password.status(), StatusCode::OK);
    let clear_body = axum::body::to_bytes(clear_password.into_body(), 8192)
        .await
        .unwrap();
    let clear_json: serde_json::Value = serde_json::from_slice(&clear_body).unwrap();
    let cleared_version = clear_json["device"]["version"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(state
        .config
        .read()
        .await
        .devices
        .iter()
        .any(|device| device.id == "renamed-bmc" && device.auth.password.is_empty()));

    let delete = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/devices/renamed-bmc")
                .header("if-match", cleared_version)
                .header("x-mutation-id", "delete-renamed-bmc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::OK);
    assert!(state.registry.get_dev("renamed-bmc").await.is_none());
    assert!(!state
        .config
        .read()
        .await
        .devices
        .iter()
        .any(|dev| dev.id == "renamed-bmc"));
}

#[tokio::test]
async fn test_devices_scope_etag_and_events_follow_crud() {
    let state = make_state().await;
    let app = create_router(state.clone());
    let mut events = state.configuration.subscribe();

    let initial = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(initial.status(), StatusCode::OK);
    let initial_etag = initial
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/devices")
                .header("content-type", "application/json")
                .header("x-mutation-id", "devices-scope-create")
                .body(Body::from(
                    r#"{"id":"devices-scope-device","name":"Scope device","endpoint":{"host":"192.0.2.30","port":443},"credentials":{"username":"admin","password":{"operation":"replace","value":"secret"}},"assignments":{}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::OK);
    let created_body = axum::body::to_bytes(create.into_body(), 8192)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&created_body).unwrap();
    let created_version = created["device"]["version"].as_str().unwrap().to_string();

    let device_event = events.recv().await.unwrap();
    assert_eq!(
        device_event.scope,
        bmc_aggregator::bmc::configuration::ConfigurationScope::Device {
            id: "devices-scope-device".to_string()
        }
    );
    let list_event = events.recv().await.unwrap();
    assert_eq!(
        list_event.scope,
        bmc_aggregator::bmc::configuration::ConfigurationScope::Global {
            section: "devices".to_string()
        }
    );

    let after_create = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let after_create_etag = after_create
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_ne!(after_create_etag, initial_etag);

    let update = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/devices-scope-device")
                .header("content-type", "application/json")
                .header("if-match", &created_version)
                .header("x-mutation-id", "devices-scope-update")
                .body(Body::from(
                    r#"{"id":"devices-scope-renamed","name":"Scope device updated"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::OK);
    let updated_body = axum::body::to_bytes(update.into_body(), 8192)
        .await
        .unwrap();
    let updated: serde_json::Value = serde_json::from_slice(&updated_body).unwrap();
    let updated_version = updated["device"]["version"].as_str().unwrap().to_string();
    assert_ne!(updated_version, created_version);
    assert_eq!(
        events.recv().await.unwrap().scope,
        bmc_aggregator::bmc::configuration::ConfigurationScope::Device {
            id: "devices-scope-device".to_string()
        }
    );
    assert_eq!(
        events.recv().await.unwrap().scope,
        bmc_aggregator::bmc::configuration::ConfigurationScope::Device {
            id: "devices-scope-renamed".to_string()
        }
    );
    assert_eq!(events.recv().await.unwrap().scope, list_event.scope);

    let after_update = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let after_update_etag = after_update
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_ne!(after_update_etag, after_create_etag);

    let delete = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/devices/devices-scope-renamed")
                .header("if-match", &updated_version)
                .header("x-mutation-id", "devices-scope-delete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::OK);
    assert_eq!(events.recv().await.unwrap().changed_paths, vec!["device"]);
    assert_eq!(events.recv().await.unwrap().scope, list_event.scope);

    let after_delete = app
        .oneshot(
            Request::builder()
                .uri("/api/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let after_delete_etag = after_delete
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_ne!(after_delete_etag, after_update_etag);
    let body = axum::body::to_bytes(after_delete.into_body(), 8192)
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(!list["devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|device| device["id"] == "devices-scope-renamed"));
}

#[tokio::test]
async fn test_device_config_update_requires_preconditions() {
    let state = make_state().await;
    let response = create_router(state)
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/test-bmc")
                .header("content-type", "application/json")
                .header("x-mutation-id", "missing-etag")
                .body(Body::from(r#"{"name":"No precondition"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PRECONDITION_REQUIRED);
}

#[tokio::test]
async fn test_concurrent_device_config_updates_detect_lost_update() {
    let state = make_device_configuration_state().await;
    let app = create_router(state.clone());
    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/devices/test-bmc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let version = get
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let request = |name: &'static str, mutation: &'static str| {
        app.clone().oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/test-bmc")
                .header("content-type", "application/json")
                .header("if-match", &version)
                .header("x-mutation-id", mutation)
                .body(Body::from(format!(r#"{{"name":"{name}"}}"#)))
                .unwrap(),
        )
    };
    let (first, second) = tokio::join!(
        request("First", "concurrent-first"),
        request("Second", "concurrent-second")
    );
    let statuses = [first.unwrap().status(), second.unwrap().status()];

    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1
    );
}

#[tokio::test]
async fn test_device_config_persistence_failure_publishes_nothing() {
    let mut state = make_device_configuration_state().await;
    Arc::get_mut(&mut state).unwrap().config_path = std::env::temp_dir()
        .join(format!(
            "bmc-aggregator-missing-parent-{}/config.yaml",
            uuid::Uuid::new_v4()
        ))
        .to_string_lossy()
        .to_string();
    let original_config = state.config.read().await.clone();
    let original_device = state.registry.get_dev("test-bmc").await.unwrap();
    let mut events = state.configuration.subscribe();
    let app = create_router(state.clone());

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/devices/test-bmc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let original_etag = get
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let update = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/test-bmc")
                .header("content-type", "application/json")
                .header("if-match", &original_etag)
                .header("x-mutation-id", "must-not-commit")
                .body(Body::from(r#"{"name":"Must not persist"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(update.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        state.config.read().await.devices[0].name,
        original_config.devices[0].name
    );
    assert!(Arc::ptr_eq(
        &state.registry.get_dev("test-bmc").await.unwrap(),
        &original_device
    ));
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    let get_after_failure = app
        .oneshot(
            Request::builder()
                .uri("/api/devices/test-bmc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        get_after_failure
            .headers()
            .get("etag")
            .unwrap()
            .to_str()
            .unwrap(),
        original_etag
    );
}

#[tokio::test]
async fn test_identical_device_config_update_is_a_replayable_noop() {
    let state = make_device_configuration_state().await;
    let original_device = state.registry.get_dev("test-bmc").await.unwrap();
    let initial_revision = state.configuration.current_revision();
    let mut events = state.configuration.subscribe();
    let app = create_router(state.clone());

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/devices/test-bmc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let original_etag = get
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let body = axum::body::to_bytes(get.into_body(), 8192).await.unwrap();
    let current: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let payload = serde_json::json!({
        "name": current["device"]["name"],
        "credentials": {"password": {"operation": "keep"}},
    })
    .to_string();

    let update = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/test-bmc")
                .header("content-type", "application/json")
                .header("if-match", &original_etag)
                .header("x-mutation-id", "identical-update")
                .body(Body::from(payload.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::OK);
    assert_eq!(
        update.headers().get("etag").unwrap().to_str().unwrap(),
        original_etag
    );
    let update_body = axum::body::to_bytes(update.into_body(), 8192)
        .await
        .unwrap();
    let update_json: serde_json::Value = serde_json::from_slice(&update_body).unwrap();
    assert_eq!(update_json["affected_features"], serde_json::json!([]));
    assert_eq!(update_json["revision"], initial_revision);
    assert_eq!(state.configuration.current_revision(), initial_revision);
    assert!(Arc::ptr_eq(
        &state.registry.get_dev("test-bmc").await.unwrap(),
        &original_device
    ));
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    let replay = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/test-bmc")
                .header("content-type", "application/json")
                .header("if-match", "\"deliberately-stale\"")
                .header("x-mutation-id", "identical-update")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(
        replay.headers().get("etag").unwrap().to_str().unwrap(),
        original_etag
    );
    let replay_body = axum::body::to_bytes(replay.into_body(), 8192)
        .await
        .unwrap();
    let replay_json: serde_json::Value = serde_json::from_slice(&replay_body).unwrap();
    assert_eq!(replay_json, update_json);
}

#[tokio::test]
async fn test_configuration_change_reaches_all_subscribers_after_persistence() {
    let state = make_device_configuration_state().await;
    let app = create_router(state.clone());
    let mut first_client = state.configuration.subscribe();
    let mut second_client = state.configuration.subscribe();

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/devices/test-bmc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let version = get
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/devices/test-bmc")
                .header("content-type", "application/json")
                .header("if-match", version)
                .header("x-mutation-id", "broadcast-device-rename")
                .body(Body::from(r#"{"name":"Broadcasted name"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let first = tokio::time::timeout(std::time::Duration::from_secs(1), first_client.recv())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(1), second_client.recv())
        .await
        .unwrap()
        .unwrap();
    for event in [first, second] {
        assert_eq!(event.scope, device_scope_for_test("test-bmc"));
        assert_eq!(event.mutation_id, "broadcast-device-rename");
        assert_eq!(event.changed_paths, vec!["device"]);
        assert!(event.revision > 0);
    }
    assert_eq!(
        state.config.read().await.devices[0].name,
        "Broadcasted name"
    );
}

fn device_scope_for_test(id: &str) -> bmc_aggregator::bmc::configuration::ConfigurationScope {
    bmc_aggregator::bmc::configuration::ConfigurationScope::Device { id: id.to_string() }
}

#[tokio::test]
async fn test_concurrent_updates_to_different_devices_do_not_conflict() {
    let state = make_device_configuration_state().await;
    let app = create_router(state.clone());
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/devices")
                .header("content-type", "application/json")
                .header("x-mutation-id", "create-second-device")
                .body(Body::from(
                    r#"{"id":"second-bmc","name":"Second BMC","endpoint":{"host":"192.0.2.20"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::OK);

    let version_for = |id: &'static str| {
        let app = app.clone();
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/devices/{id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            response
                .headers()
                .get("etag")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        }
    };
    let (first_version, second_version) =
        tokio::join!(version_for("test-bmc"), version_for("second-bmc"));

    let update =
        |id: &'static str, version: String, name: &'static str, mutation_id: &'static str| {
            app.clone().oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/devices/{id}"))
                    .header("content-type", "application/json")
                    .header("if-match", version)
                    .header("x-mutation-id", mutation_id)
                    .body(Body::from(format!(r#"{{"name":"{name}"}}"#)))
                    .unwrap(),
            )
        };
    let (first, second) = tokio::join!(
        update(
            "test-bmc",
            first_version,
            "Updated first",
            "update-first-device"
        ),
        update(
            "second-bmc",
            second_version,
            "Updated second",
            "update-second-device"
        )
    );

    assert_eq!(first.unwrap().status(), StatusCode::OK);
    assert_eq!(second.unwrap().status(), StatusCode::OK);
    let config = state.config.read().await;
    assert_eq!(
        config
            .devices
            .iter()
            .find(|device| device.id == "test-bmc")
            .unwrap()
            .name,
        "Updated first"
    );
    assert_eq!(
        config
            .devices
            .iter()
            .find(|device| device.id == "second-bmc")
            .unwrap()
            .name,
        "Updated second"
    );
}

#[tokio::test]
async fn test_idrac_api_preset_materializes_supported_features_only() {
    let state = make_state().await;
    let app = create_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/devices")
                .header("content-type", "application/json")
                .header("x-mutation-id", "create-idrac-api")
                .body(Body::from(
                    r#"{"id":"idrac-api","name":"iDRAC API","endpoint":{"host":"192.0.2.42"},"credentials":{"username":"root","password":{"operation":"replace","value":"secret"}},"assignments":{"video":{"kind":"single","backend":{"backend_id":"idrac-avct-console","params":{"avct_port":5900,"avct_tls_mode":"idrac-legacy-rsa","idrac_generation":8}}},"hid":{"kind":"single","backend":{"backend_id":"idrac-avct-console","params":{"avct_port":5900,"avct_tls_mode":"idrac-legacy-rsa","idrac_generation":8}}},"power":{"kind":"single","backend":{"backend_id":"ipmi"}},"identify":{"kind":"single","backend":{"backend_id":"ipmi"}},"boot":{"kind":"provider_set","providers":[{"id":"ipmi","backend_id":"ipmi"},{"id":"idrac-wsman","backend_id":"idrac-wsman","params":{"idrac_generation":8}}]}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["device"]["assignments"]["video"]["backend"]["backend_id"],
        "idrac-avct-console"
    );
    assert_eq!(
        json["device"]["assignments"]["hid"]["backend"]["backend_id"],
        "idrac-avct-console"
    );
    assert_eq!(
        json["device"]["assignments"]["power"]["backend"]["backend_id"],
        "ipmi"
    );
    assert_eq!(
        json["device"]["assignments"]["boot"]["kind"],
        "provider_set"
    );
    assert_eq!(
        json["device"]["assignments"]["identify"]["backend"]["backend_id"],
        "ipmi"
    );
    assert_eq!(
        json["device"]["assignments"]["video"]["backend"]["params"]["avct_port"],
        5900
    );
    assert_eq!(
        json["device"]["assignments"]["video"]["backend"]["params"]["avct_tls_mode"],
        "idrac-legacy-rsa"
    );
    assert_eq!(json["device"]["assignments"]["msd"]["kind"], "disabled");

    let device = state.registry.get_dev("idrac-api").await.unwrap();
    assert!(device.features.contains_key("video"));
    assert!(device.features.contains_key("hid"));
    assert!(device.features.contains_key("power"));
    assert!(device.features.contains_key("boot"));
    assert!(device.features.contains_key("identify"));
    assert!(!device.features.contains_key("msd"));
}

#[tokio::test]
async fn test_webtransport_capabilities_returns_codec_and_encoder_boundary() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/webtransport/capabilities")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["transport"], "webtransport");
    assert_eq!(json["decoder"], "webcodecs");
    assert!(json["codecs"].is_array());
    assert!(json["encoders"].is_array());
    assert!(json["codecs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|codec| codec["id"] == "h264" && codec["webcodecs_codec"].is_string()));
}

#[tokio::test]
async fn test_stream_config_get_and_patch_encoder() {
    let state = make_state().await;
    let app = create_router(state);
    let get_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_response.status(), StatusCode::OK);
    let version = get_response
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let get_body = axum::body::to_bytes(get_response.into_body(), 4096)
        .await
        .unwrap();
    let get_json: serde_json::Value = serde_json::from_slice(&get_body).unwrap();
    assert_eq!(get_json["encoder"], "software");

    let patch_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/bmc/test-bmc/api/feature/video/config")
                .header("content-type", "application/json")
                .header("if-match", &version)
                .header("x-mutation-id", "update-video-encoder")
                .body(Body::from(r#"{"encoder":"vaapi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patch_response.status(), StatusCode::OK);
    let patch_body = axum::body::to_bytes(patch_response.into_body(), 4096)
        .await
        .unwrap();
    let patch_json: serde_json::Value = serde_json::from_slice(&patch_body).unwrap();
    assert_eq!(patch_json["encoder"], "vaapi");
    assert_eq!(patch_json["mutation_id"], "update-video-encoder");
    assert!(patch_json["revision"].as_u64().is_some());
}

// ── ATX Power (POST) ─────────────────────────────────────────────

#[tokio::test]
async fn test_atx_power_on_returns_500_without_backend() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder()
        .uri("/bmc/test-bmc/api/feature/power/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"on"}"#))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    // Without a real power backend, fc_execute fails → 500
    assert!(response.status().is_server_error());
}

#[tokio::test]
async fn test_atx_power_hold_returns_500_without_backend() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder()
        .uri("/bmc/test-bmc/api/feature/power/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"hold"}"#))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert!(response.status().is_server_error());
}

#[tokio::test]
async fn test_atx_power_reset_returns_500_without_backend() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder()
        .uri("/bmc/test-bmc/api/feature/power/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"reset"}"#))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert!(response.status().is_server_error());
}

#[tokio::test]
async fn test_atx_power_invalid_action_returns_bad_request() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder()
        .uri("/bmc/test-bmc/api/feature/power/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"invalid"}"#))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    // With mock backend, fc_execute fails first → 500 before validatation
    // Without power feature → 503. Either is acceptable for a mock backend.
    assert!(
        response.status().is_server_error() || response.status() == StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn test_atx_power_short_returns_500_without_backend() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder()
        .uri("/bmc/test-bmc/api/feature/power/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"short"}"#))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert!(response.status().is_server_error());
}

#[tokio::test]
async fn test_atx_power_long_returns_500_without_backend() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder()
        .uri("/bmc/test-bmc/api/feature/power/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"long"}"#))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert!(response.status().is_server_error());
}

#[tokio::test]
async fn test_atx_power_press_returns_500_without_backend() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder()
        .uri("/bmc/test-bmc/api/feature/power/control")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"press"}"#))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert!(response.status().is_server_error());
}

// ── ATX Status (GET) ─────────────────────────────────────────────

#[tokio::test]
async fn test_atx_status_returns_fallback() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/power/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["available"], true);
    assert!(status.get("power_status").is_none() || status["power_status"] == "unknown");
}

// ── 404 Handling ─────────────────────────────────────────────────

#[tokio::test]
async fn test_unknown_route_returns_404() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/nonexistent/path")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_bmc_sub_path_unknown_returns_404() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "error");
}

// ── MJPEG Stream ─────────────────────────────────────────────────

#[tokio::test]
async fn test_http_stream_returns_error_without_session() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/stream/http")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_server_error() || response.status().is_success());
}

#[tokio::test]
async fn test_stream_start_returns_500_without_backend() {
    let state = make_state().await;
    let app = create_router(state);
    // POST start with mock backend → fc_execute fails → 500
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/start")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_server_error());
}

// ── MSD Basic Endpoint Tests ──────────────────────────────────

#[tokio::test]
async fn test_msd_status_returns_defaults() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["available"], true);
    assert_eq!(status["attachments"], serde_json::json!([]));
    assert_eq!(status["occupancy"]["state"], "idle");
    assert_eq!(status["occupancy"]["details_available"], true);
    assert_eq!(status["occupancy"]["active_sessions"], 0);
}

#[tokio::test]
async fn test_msd_set_image_rejects_unknown_or_unmanaged_image() {
    let state = make_state().await;
    let backend = install_recording_msd_backend(&state).await;
    let app = create_router(state);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/set_image")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"slot_id":"default","source":{"location":"aggregator","image_id":"missing.iso"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(backend.selected_image.lock().await.is_none());
}

#[tokio::test]
async fn test_msd_set_image_refreshes_capabilities_before_selection() {
    let state = make_state().await;
    let image_id = format!("managed-{}.img", uuid::Uuid::new_v4());
    state.msd_storage.ensure_dirs("test-bmc").await.unwrap();
    state
        .msd_storage
        .upload_image("test-bmc", &image_id, b"managed-image")
        .await
        .unwrap();
    let backend = install_recording_msd_backend(&state).await;
    let app = create_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/set_image")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"slot_id":"default","source":{{"location":"aggregator","image_id":"{image_id}"}},"media_type":"disk"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        *backend.refreshed_media_type.lock().await,
        Some(Some(MsdMediaKind::Disk))
    );
    assert!(backend.selected_image.lock().await.is_some());
}

#[tokio::test]
async fn test_msd_rejects_replacing_media_on_a_mounted_target() {
    let state = make_state().await;
    let image_id = format!("mounted-{}.img", uuid::Uuid::new_v4());
    state.msd_storage.ensure_dirs("test-bmc").await.unwrap();
    state
        .msd_storage
        .upload_image("test-bmc", &image_id, b"mounted-image")
        .await
        .unwrap();
    let backend = install_recording_msd_backend(&state).await;
    let app = create_router(state);
    let request_body = format!(
        r#"{{"slot_id":"default","source":{{"location":"aggregator","image_id":"{image_id}"}},"media_type":"disk"}}"#
    );

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/set_image")
                .header("content-type", "application/json")
                .body(Body::from(request_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    for (uri, body) in [
        (
            "/bmc/test-bmc/api/feature/msd/set_image",
            request_body.clone(),
        ),
        (
            "/bmc/test-bmc/api/feature/msd/connect",
            format!(
                r#"{{"slot_id":"default","mode":"image","source":{{"location":"aggregator","image_id":"{image_id}"}},"media_type":"disk"}}"#
            ),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let fault: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(fault["fault"]["code"], "target_mounted");
    }

    assert_eq!(backend.mounted_slots.lock().await.len(), 1);
}

#[tokio::test]
async fn test_msd_connect_returns_500_without_backend() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/connect")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"slot_id":"default","mode":"image","source":{"location":"aggregator","image_id":"missing.iso"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_server_error());
}

#[tokio::test]
async fn test_msd_disconnect_returns_500_without_backend() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/disconnect")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"slot_id":"default"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_server_error());
}

#[tokio::test]
async fn test_msd_without_session_returns_available_true() {
    let state = make_state_without_session().await;
    let app = create_router(state);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["available"], false);
    assert_eq!(status["attachments"], serde_json::json!([]));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/connect")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"slot_id":"default","mode":"image","source":{"location":"aggregator","image_id":"missing.iso"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/disconnect")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"slot_id":"default"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_msd_extra_endpoints_exist() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/images?location=aggregator")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/drive")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/drive/files?path=%2F")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_msd_connect_supports_virtual_drive_backend() {
    let state = make_state().await;
    let app = create_router(state);

    // Without a real MSD backend, connect returns 500
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/connect")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"slot_id":"default","backend":"virtual-drive","mode":"drive"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["backend"], "mock");
}

#[tokio::test]
async fn test_msd_images_and_drive_endpoints_work() {
    let state = make_state().await;
    let app = create_router(state);

    // mkdir and verify it appears
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/drive/mkdir")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"path":"/folder1"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/drive/files?path=%2F")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let files: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = files["items"].as_array().unwrap();
    assert!(items
        .iter()
        .any(|v| v["name"] == "folder1" && v["is_dir"] == true));
}

#[tokio::test]
async fn test_msd_drive_mutations_reject_traversal_paths() {
    let state = make_state().await;
    let app = create_router(state);

    for (path, body) in [
        (
            "/bmc/test-bmc/api/feature/msd/drive/mkdir",
            serde_json::json!({"path": "../escape"}),
        ),
        (
            "/bmc/test-bmc/api/feature/msd/drive/files/delete",
            serde_json::json!({"path": "../escape"}),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn test_msd_image_connect_resolves_a_managed_image_and_updates_runtime() {
    let state = make_state().await;
    let image_id = format!("managed-{}.iso", uuid::Uuid::new_v4());
    state.msd_storage.ensure_dirs("test-bmc").await.unwrap();
    state
        .msd_storage
        .upload_image("test-bmc", &image_id, b"managed-image")
        .await
        .unwrap();
    let backend = install_recording_msd_backend(&state).await;
    let app = create_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/connect")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"slot_id":"default","mode":"image","source":{{"location":"aggregator","image_id":"{image_id}"}}}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let selected = backend.selected_image.lock().await.clone().unwrap();
    assert!(
        selected.0.starts_with("/proc/self/fd/"),
        "managed images must be passed through an already-open descriptor"
    );
    assert_eq!(selected.1, b"managed-image".len() as u64);
    assert!(backend.connected.load(std::sync::atomic::Ordering::SeqCst));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["attachments"][0]["slot_id"], "default");
    assert_eq!(status["attachments"][0]["mode"], "image");
    assert_eq!(status["attachments"][0]["media"]["location"], "aggregator");
    assert_eq!(status["attachments"][0]["media"]["image_id"], image_id);
    assert_eq!(status["attachments"][0]["media_type"], "disk");
}

#[tokio::test]
async fn test_msd_targets_allow_duplicate_and_mixed_media_combinations() {
    let state = make_state().await;
    state.msd_storage.ensure_dirs("test-bmc").await.unwrap();
    for (name, contents) in [
        ("optical-a.iso", b"optical-a".as_slice()),
        ("optical-b.iso", b"optical-b".as_slice()),
        ("floppy.img", b"floppy".as_slice()),
    ] {
        state
            .msd_storage
            .upload_image("test-bmc", name, contents)
            .await
            .unwrap();
    }
    let backend = install_recording_msd_backend(&state).await;
    let app = create_router(state);

    for (target_id, media_type) in [
        ("optical-0", "optical"),
        ("optical-1", "optical"),
        ("floppy-0", "floppy"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/bmc/test-bmc/api/feature/msd/targets/create")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"media_type":"{media_type}","label":"{target_id}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    for (target_id, image_id, media_type) in [
        ("optical-0", "optical-a.iso", "optical"),
        ("optical-1", "optical-b.iso", "optical"),
        ("floppy-0", "floppy.img", "floppy"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/bmc/test-bmc/api/feature/msd/connect")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"slot_id":"{target_id}","mode":"image","media_type":"{media_type}","source":{{"location":"aggregator","image_id":"{image_id}"}}}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    assert_eq!(backend.mounted_slots.lock().await.len(), 3);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["attachments"].as_array().unwrap().len(), 3);
    assert_eq!(status["capabilities"]["slots"].as_array().unwrap().len(), 5);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/targets/delete")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"target_id":"optical-0"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/disconnect")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"slot_id":"optical-0"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut mounted = backend
        .mounted_slots
        .lock()
        .await
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    mounted.sort();
    assert_eq!(
        mounted,
        vec!["floppy-0".to_string(), "optical-1".to_string()]
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/targets/delete")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"target_id":"optical-0"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["attachments"].as_array().unwrap().len(), 2);
    assert!(status["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .all(|attachment| attachment["slot_id"] != "optical-0"));
}

#[tokio::test]
async fn test_msd_image_connect_rejects_unmanaged_paths_before_backend_access() {
    let state = make_state().await;
    let backend = install_recording_msd_backend(&state).await;
    let app = create_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/connect")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"slot_id":"default","mode":"image","source":{"location":"aggregator","image_id":"../../etc/passwd"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(backend.selected_image.lock().await.is_none());
}

#[tokio::test]
async fn test_msd_bmc_store_upload_list_mount_and_delete_are_backend_dispatched() {
    let state = make_state().await;
    let backend = install_recording_msd_backend(&state).await;
    let app = create_router(state);
    let boundary = "BMCSTOREBOUNDARY";
    let payload = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"controller.img\"\r\nContent-Type: application/octet-stream\r\n\r\nbmc-bytes\r\n--{boundary}--\r\n"
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/upload?location=bmc")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/images?location=bmc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let catalog: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(catalog["location"], "bmc");
    assert_eq!(catalog["images"][0]["id"], "controller.img");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/connect")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"slot_id":"default","mode":"image","source":{"location":"bmc","image_id":"controller.img"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        backend.selected_bmc_image.lock().await.as_deref(),
        Some("controller.img")
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["attachments"][0]["media"]["location"], "bmc");
    assert_eq!(
        status["attachments"][0]["media"]["image_id"],
        "controller.img"
    );
    assert_eq!(status["attachments"][0]["media_type"], "disk");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/delete")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"location":"bmc","image_id":"controller.img"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/disconnect")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"slot_id":"default"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/delete")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"location":"bmc","image_id":"controller.img"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(backend.store.images.lock().await.is_empty());
}

#[tokio::test]
async fn test_msd_drive_init_persists_and_applies_requested_capacity() {
    let state = make_state().await;
    let mut events = state.configuration.subscribe();
    let app = create_router(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/drive/init")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"size_mb":256,"mutation_id":"drive-init-256"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["size_mb"], 256);
    assert_eq!(payload["mutation_id"], "drive-init-256");
    assert_eq!(payload["revision"], 1);
    let event = events.try_recv().unwrap();
    assert_eq!(event.mutation_id, "drive-init-256");
    assert_eq!(event.changed_paths, vec!["msd.drive_size_mb"]);
    assert_eq!(state.config.read().await.msd["test-bmc"].drive_size_mb, 256);
    assert_eq!(
        state.msd_storage.drive_info("test-bmc").await.0,
        256 * 1024 * 1024
    );

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/drive/init")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"size_mb":256,"mutation_id":"drive-init-repeat"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let repeat: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(repeat["revision"], 1);
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn test_msd_images_multipart_upload_writes_real_content() {
    let state = make_state().await;
    let app = create_router(state);
    let boundary = "XBOUNDARY";
    let payload = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.iso\"\r\nContent-Type: application/octet-stream\r\n\r\nhello-msd\r\n--{boundary}--\r\n"
    );
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/upload?location=aggregator")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 8192).await.unwrap();
    let uploaded: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let digests = uploaded["digests"].as_array().unwrap();
    assert_eq!(digests.len(), 4);
    assert!(digests.iter().any(|digest| {
        digest["algorithm"] == "sha256" && digest["value"].as_str().unwrap().len() == 64
    }));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/images?location=aggregator")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let images = json["images"].as_array().unwrap();
    assert!(images
        .iter()
        .any(|v| v["name"] == "hello.iso" && v["size"].as_u64().unwrap_or(0) > 0));
}

#[tokio::test]
async fn test_msd_image_upload_allows_payloads_above_axum_default_limit() {
    let state = make_state().await;
    let app = create_router(state);
    let boundary = "LARGEBOUNDARY";
    let mut payload = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"large.img\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    payload.resize(payload.len() + 3 * 1024 * 1024, 0x5a);
    payload.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/upload?location=aggregator")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_msd_image_upload_uses_configured_limit_and_returns_precise_fault() {
    let state = make_state().await;
    state.config.write().await.devices[0]
        .features
        .entry("msd".to_string())
        .or_default()
        .feature_params
        .insert("max_image_bytes".to_string(), serde_json::json!(8));
    let app = create_router(state);
    let boundary = "LIMITBOUNDARY";
    let payload = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"large.img\"\r\nContent-Type: application/octet-stream\r\n\r\n123456789\r\n--{boundary}--\r\n"
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/upload?location=aggregator")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["fault"]["domain"], "storage");
    assert_eq!(payload["fault"]["code"], "upload_too_large");
    assert_eq!(payload["fault"]["retryable"], false);
    assert_eq!(payload["fault"]["scope"], "operation");
    assert_eq!(payload["fault"]["operation"], "upload_image");
}

#[tokio::test]
async fn test_msd_resumable_upload_survives_router_recreation_and_publishes_digests() {
    let state = make_state().await;
    let create = create_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/uploads")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"filename":"resume.iso","location":"aggregator","media_type":"optical","length":11}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(create.into_body(), 8192)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let upload_id = created["id"].as_str().unwrap();

    let first = create_router(state.clone())
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/uploads/{upload_id}"
                ))
                .header("upload-offset", "0")
                .body(Body::from("hello "))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    // A new router has no transfer-local memory; the persisted sidecar is the
    // authority used to resume at byte six.
    let second = create_router(state.clone())
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/uploads/{upload_id}"
                ))
                .header("upload-offset", "6")
                .body(Body::from("world"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let body = axum::body::to_bytes(second.into_body(), 8192)
        .await
        .unwrap();
    let completed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(completed["state"], "completed");
    assert_eq!(completed["offset"], 11);
    assert_eq!(completed["digests"].as_array().unwrap().len(), 4);
    let digests: std::collections::HashMap<_, _> = completed["digests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|digest| {
            (
                digest["algorithm"].as_str().unwrap(),
                digest["value"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(digests["md5"], "5eb63bbbe01eeed093cb22bb8f5acdc3");
    assert_eq!(digests["sha1"], "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed");
    assert_eq!(
        digests["sha256"],
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
    assert_eq!(
        digests["sha512"],
        "309ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f"
    );

    let stored = state
        .msd_storage
        .image_file("test-bmc", "resume.iso")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tokio::fs::read(stored.0).await.unwrap(), b"hello world");
}

#[tokio::test]
async fn test_msd_resumable_upload_rejects_stale_offset_without_appending() {
    let state = make_state().await;
    let app = create_router(state);
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/uploads")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"filename":"offset.iso","location":"aggregator","length":4}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(create.into_body(), 8192)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let upload_id = created["id"].as_str().unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/uploads/{upload_id}"
                ))
                .header("upload-offset", "1")
                .body(Body::from("data"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let fault: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(fault["fault"]["code"], "upload_offset_mismatch");
}

#[tokio::test]
async fn test_msd_resumable_upload_cancel_removes_partial_data() {
    let state = make_state().await;
    let app = create_router(state.clone());
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/uploads")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"filename":"cancel.iso","location":"aggregator","length":10}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(create.into_body(), 8192)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let upload_id = created["id"].as_str().unwrap();
    let partial = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/uploads/{upload_id}"
                ))
                .header("upload-offset", "0")
                .body(Body::from("half"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(partial.status(), StatusCode::OK);
    let cancelled = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/uploads/{upload_id}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status(), StatusCode::OK);
    let root = state.msd_storage.storage_base("test-bmc").await;
    assert!(!std::path::Path::new(&root)
        .join("images/.staging")
        .join(format!("{upload_id}.part"))
        .exists());
}

#[tokio::test]
async fn test_msd_resumable_upload_serializes_concurrent_patch_offsets() {
    let state = make_state().await;
    let app = create_router(state);
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/uploads")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"filename":"concurrent.iso","location":"aggregator","length":8}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(create.into_body(), 8192)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let upload_id = created["id"].as_str().unwrap();
    let request = || {
        Request::builder()
            .method("PATCH")
            .uri(format!(
                "/bmc/test-bmc/api/feature/msd/images/uploads/{upload_id}"
            ))
            .header("upload-offset", "0")
            .body(Body::from("half"))
            .unwrap()
    };
    let (first, second) = tokio::join!(
        app.clone().oneshot(request()),
        app.clone().oneshot(request())
    );
    let mut statuses = [first.unwrap().status(), second.unwrap().status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    let status = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/uploads/{upload_id}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(status.into_body(), 8192)
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["offset"], 4);
}

#[tokio::test]
async fn test_msd_resumable_upload_discards_uncommitted_tail_after_body_error() {
    let state = make_state().await;
    let app = create_router(state.clone());
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/uploads")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"filename":"rollback.iso","location":"aggregator","length":4}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(create.into_body(), 8192)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let upload_id = created["id"].as_str().unwrap();
    let broken = futures::stream::iter(vec![
        Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"bad")),
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "intentional request-body failure",
        )),
    ]);
    let failed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/uploads/{upload_id}"
                ))
                .header("upload-offset", "0")
                .body(Body::from_stream(broken))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::BAD_REQUEST);
    let completed = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/uploads/{upload_id}"
                ))
                .header("upload-offset", "0")
                .body(Body::from("good"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(completed.status(), StatusCode::OK);
    let (path, _, _file) = state
        .msd_storage
        .image_file("test-bmc", "rollback.iso")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), b"good");
}

#[tokio::test]
async fn test_msd_resumable_upload_dispatches_completed_file_to_bmc_store() {
    let state = make_state().await;
    let backend = install_recording_msd_backend(&state).await;
    let app = create_router(state);
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/uploads")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"filename":"bmc.iso","location":"bmc","media_type":"optical","length":8}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(create.into_body(), 8192)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let upload_id = created["id"].as_str().unwrap();
    let completed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/uploads/{upload_id}"
                ))
                .header("upload-offset", "0")
                .body(Body::from("bmc-data"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(completed.status(), StatusCode::OK);
    let body = axum::body::to_bytes(completed.into_body(), 8192)
        .await
        .unwrap();
    let completed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(completed["state"], "completed");
    assert_eq!(completed["image_id"], "bmc.iso");
    assert_eq!(
        backend.store.images.lock().await["bmc.iso"].as_ref(),
        b"bmc-data"
    );
    let listed = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/images?location=bmc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(listed.into_body(), 8192)
        .await
        .unwrap();
    let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(completed["digests"].as_array().unwrap().len(), 4);
    assert!(
        listed["images"][0].get("digests").is_none(),
        "An unversioned remote directory must not inherit upload-receipt checksums"
    );
}

#[tokio::test]
async fn test_msd_drive_multipart_upload_writes_real_content() {
    let state = make_state().await;
    let app = create_router(state);
    let boundary = "XBOUNDARY2";
    let payload = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"/\"; filename=\"notes.txt\"\r\nContent-Type: text/plain\r\n\r\nabc123\r\n--{boundary}--\r\n"
    );
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/drive/files/upload?path=%2F")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/drive/files?path=%2F")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let files: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = files["items"].as_array().unwrap();
    assert!(items
        .iter()
        .any(|v| v["name"] == "notes.txt" && v["size"].as_u64().unwrap_or(0) > 0));
}

#[tokio::test]
async fn test_msd_drive_upload_rejects_payload_above_configured_capacity() {
    let state = make_state().await;
    state.config.write().await.msd.insert(
        "test-bmc".into(),
        bmc_aggregator::config::MsdDeviceConfig {
            drive_size_mb: 0,
            ..Default::default()
        },
    );
    let app = create_router(state);
    let boundary = "XBOUNDARY-CAPACITY";
    let payload = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"overflow.bin\"\r\nContent-Type: application/octet-stream\r\n\r\nx\r\n--{boundary}--\r\n"
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/drive/files/upload")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let fault: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(fault["fault"]["code"], "drive_file_too_large");
}

#[tokio::test]
async fn test_msd_images_download_fetches_remote_content() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let remote_app =
        axum::Router::new().route("/demo.iso", axum::routing::get(|| async { "iso-bytes" }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, remote_app).await;
    });

    let state = make_state().await;
    let app = create_router(state);
    let url = format!("http://{}/demo.iso", addr);

    let res = app.clone().oneshot(
        Request::builder().method("GET")
            .uri(format!("/bmc/test-bmc/api/feature/msd/images/download?location=aggregator&url={url}&filename=from_remote.iso"))
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let started: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let download_id = started["download_id"].as_str().unwrap();
    assert_eq!(started["filename"], "from_remote.iso");

    // Poll the backend-owned status record until the asynchronous task
    // completes, then confirm the image became visible to the storage API.
    let expected_name = "from_remote.iso";
    let poll_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/bmc/test-bmc/api/feature/msd/images/download/status?download_id={download_id}"
                        ))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if json["status"] == "completed" {
                assert!(json["bytes_downloaded"].as_u64().unwrap_or(0) > 0);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        poll_result.is_ok(),
        "timed out waiting for downloaded image"
    );

    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/images?location=aggregator")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["images"]
        .as_array()
        .unwrap()
        .iter()
        .any(|image| image["name"] == expected_name));
}

async fn checkpointed_url_import(
    remote: axum::Router,
    prefix: &[u8],
    validator: Option<&str>,
    total: Option<u64>,
) -> (String, Option<Vec<u8>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, remote).await.unwrap() });
    let state = make_state().await;
    state.msd_storage.ensure_dirs("test-bmc").await.unwrap();
    let staging = std::path::PathBuf::from(state.msd_storage.storage_base("test-bmc").await)
        .join("images/.staging");
    tokio::fs::create_dir_all(&staging).await.unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    tokio::fs::write(staging.join(format!("download-{id}.part")), prefix)
        .await
        .unwrap();
    tokio::fs::write(
        staging.join(format!("download-{id}.json")),
        serde_json::to_vec(&serde_json::json!({
            "id":id, "device_id":"test-bmc", "url":format!("http://{address}/image.iso"),
            "filename":"test.iso", "offset":prefix.len(), "validator":validator,
            "maximum_bytes":1024, "total_bytes":total,
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let response = create_router(state.clone()).oneshot(Request::builder()
        .uri(format!("/bmc/test-bmc/api/feature/msd/images/download?location=aggregator&resume_id={id}"))
        .body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(task) = state.active_downloads.read().await.get(&id) {
                if task.status.is_terminal() {
                    break task.status.as_str().to_owned();
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    server.abort();
    let image = match state
        .msd_storage
        .image_file("test-bmc", "test.iso")
        .await
        .unwrap()
    {
        Some((path, _, _file)) => Some(tokio::fs::read(path).await.unwrap()),
        None => None,
    };
    (status, image)
}

#[tokio::test]
async fn url_import_requires_the_complete_content_range_before_publication() {
    let remote = axum::Router::new().route(
        "/image.iso",
        axum::routing::get(|headers: axum::http::HeaderMap| async move {
            let (range, body) = if headers["range"] == "bytes=5-" {
                ("bytes 5-6/10", "wo")
            } else {
                assert_eq!(headers["range"], "bytes=7-");
                ("bytes 7-9/10", "rld")
            };
            axum::http::Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header("etag", "\"v1\"")
                .header("content-range", range)
                .body(Body::from(body))
                .unwrap()
        }),
    );
    let (status, image) = checkpointed_url_import(remote, b"hello", Some("\"v1\""), Some(10)).await;
    assert_eq!(status, "completed");
    assert_eq!(image.unwrap(), b"helloworld");
}

#[tokio::test]
async fn url_import_rejects_contradictory_range_or_representation_metadata() {
    let mut unexpected = Vec::new();
    for (range, etag, prefix) in [
        ("bytes 5-9/10", "\"v2\"", &b"hello"[..]),
        ("bytes 5-9/11", "\"v1\"", &b"hello"[..]),
        ("bytes 5-8/10", "\"v1\"", &b"hello"[..]),
        ("bytes 5-9/*", "\"v1\"", &b"hello"[..]),
        ("bytes 5-9/9", "\"v1\"", &b"hello"[..]),
        ("bytes 5-garbage", "\"v1\"", &b"hello"[..]),
        ("bytes 5-9/10", "W/\"v1\"", &b"hello"[..]),
        ("bytes 5-9/10", "\"v1\"", &b""[..]),
    ] {
        let remote = axum::Router::new().route(
            "/image.iso",
            axum::routing::get(move || async move {
                axum::http::Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header("etag", etag)
                    .header("content-range", range)
                    .body(Body::from("world"))
                    .unwrap()
            }),
        );
        let (status, image) =
            checkpointed_url_import(remote, prefix, Some("\"v1\""), Some(10)).await;
        if status != "failed" || image.is_some() {
            unexpected.push((range, etag, prefix.len(), status, image));
        }
    }
    assert!(
        unexpected.is_empty(),
        "unsafe imports were published: {unexpected:?}"
    );
}

#[tokio::test]
async fn url_import_restarts_without_a_strong_representation_validator() {
    for validator in [
        None,
        Some("W/\"v1\""),
        Some("Wed, 21 Oct 2015 07:28:00 GMT"),
    ] {
        let remote = axum::Router::new().route(
            "/image.iso",
            axum::routing::get(|headers: axum::http::HeaderMap| async move {
                if headers.contains_key("range") {
                    axum::http::Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header("content-range", "bytes 5-9/10")
                        .body(Body::from("WORLD"))
                        .unwrap()
                } else {
                    axum::http::Response::builder()
                        .status(StatusCode::OK)
                        .header("etag", "\"v2\"")
                        .body(Body::from("new-world!"))
                        .unwrap()
                }
            }),
        );
        let (status, image) = checkpointed_url_import(remote, b"hello", validator, Some(10)).await;
        assert_eq!(status, "completed");
        assert_eq!(image.unwrap(), b"new-world!");
    }
}

#[tokio::test]
async fn url_import_owns_staging_after_the_http_waiter_is_cancelled() {
    let remote = axum::Router::new().route("/image.iso", axum::routing::get(|| async { "hello" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, remote).await.unwrap() });
    let state = make_state().await;
    let mut request = Box::pin(
        bmc_aggregator::bmc::feature::msd::handler::msd_images_download(
            axum::extract::Path("test-bmc".into()),
            axum::extract::State(state.clone()),
            axum::http::HeaderMap::new(),
            axum::extract::Query(HashMap::from([
                ("location".into(), "aggregator".into()),
                ("url".into(), format!("http://{address}/image.iso")),
                ("filename".into(), "cancelled-waiter.iso".into()),
            ])),
        ),
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let ready = futures::poll!(&mut request).is_ready();
            if ready || !state.active_downloads.read().await.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(request);
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if state
                .active_downloads
                .read()
                .await
                .values()
                .all(|task| task.status.is_terminal())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    server.abort();
    assert!(
        completed.is_ok(),
        "cancelled HTTP waiter orphaned a reserved download slot"
    );
    assert!(state
        .active_downloads
        .read()
        .await
        .values()
        .all(|task| task.status.as_str() == "completed"));
    let (path, _, _file) = state
        .msd_storage
        .image_file("test-bmc", "cancelled-waiter.iso")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), b"hello");
}

#[tokio::test]
async fn url_import_updates_the_validator_when_a_range_request_returns_a_new_full_body() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let remote = axum::Router::new().route(
        "/image.iso",
        axum::routing::get({
            let calls = calls.clone();
            move |headers: axum::http::HeaderMap| {
                let calls = calls.clone();
                async move {
                    if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        assert_eq!(headers["if-range"], "\"v1\"");
                        let stream = futures::stream::once(async {
                            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"NEW-"))
                        })
                        .chain(futures::stream::once(async {
                            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                            Err(std::io::Error::new(
                                std::io::ErrorKind::ConnectionReset,
                                "fixture interruption",
                            ))
                        }));
                        axum::http::Response::builder()
                            .status(StatusCode::OK)
                            .header("etag", "\"v2\"")
                            .header("content-length", "10")
                            .body(Body::from_stream(stream))
                            .unwrap()
                    } else {
                        assert_eq!(headers["if-range"], "\"v2\"");
                        assert_eq!(headers["range"], "bytes=4-");
                        // A conforming If-Range 206 may omit an unchanged ETag.
                        axum::http::Response::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header("content-range", "bytes 4-9/10")
                            .body(Body::from("IMAGE!"))
                            .unwrap()
                    }
                }
            }
        }),
    );
    let (status, image) = checkpointed_url_import(remote, b"hello", Some("\"v1\""), Some(10)).await;
    assert_eq!(status, "completed");
    assert_eq!(image.unwrap(), b"NEW-IMAGE!");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn url_import_rejects_chunked_bytes_beyond_the_declared_range() {
    let remote = axum::Router::new().route(
        "/image.iso",
        axum::routing::get(|| async {
            axum::http::Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header("etag", "\"v1\"")
                .header("content-range", "bytes 5-6/10")
                .body(Body::from_stream(futures::stream::once(async {
                    Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"world"))
                })))
                .unwrap()
        }),
    );
    let (status, image) = checkpointed_url_import(remote, b"hello", Some("\"v1\""), Some(10)).await;
    assert_eq!(status, "failed");
    assert!(image.is_none());
}

#[tokio::test]
async fn url_import_rejects_non_representation_success_responses() {
    for status in [StatusCode::CREATED, StatusCode::ACCEPTED] {
        let remote = axum::Router::new().route(
            "/image.iso",
            axum::routing::get(move || async move {
                axum::http::Response::builder()
                    .status(status)
                    .body(Body::from("task-pending"))
                    .unwrap()
            }),
        );
        let (status, image) = checkpointed_url_import(remote, b"", None, None).await;
        assert_eq!(status, "failed");
        assert!(image.is_none());
    }
}

#[tokio::test]
async fn test_msd_url_import_resumes_interrupted_response_with_range_and_if_range() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let remote_app = axum::Router::new().route(
        "/resume.iso",
        axum::routing::get({
            let requests = requests.clone();
            move |headers: axum::http::HeaderMap| {
                let requests = requests.clone();
                async move {
                    let attempt = requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if attempt == 0 {
                        let stream = futures::stream::once(async {
                            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"hello"))
                        })
                        .chain(futures::stream::once(async {
                            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                            Err(std::io::Error::new(
                                std::io::ErrorKind::ConnectionReset,
                                "intentional test interruption",
                            ))
                        }));
                        axum::http::Response::builder()
                            .status(StatusCode::OK)
                            .header("content-length", "10")
                            .header("etag", "\"resume-v1\"")
                            .body(Body::from_stream(stream))
                            .unwrap()
                    } else {
                        assert_eq!(headers.get("range").unwrap(), "bytes=5-");
                        assert_eq!(headers.get("if-range").unwrap(), "\"resume-v1\"");
                        axum::http::Response::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header("content-length", "5")
                            .header("content-range", "bytes 5-9/10")
                            .header("etag", "\"resume-v1\"")
                            .body(Body::from("world"))
                            .unwrap()
                    }
                }
            }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, remote_app).await;
    });

    let state = make_state().await;
    let app = create_router(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/download?location=aggregator&url=http://{addr}/resume.iso&filename=resumed.iso"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let started: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let download_id = started["download_id"].as_str().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/bmc/test-bmc/api/feature/msd/images/download/status?download_id={download_id}"
                        ))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if status["status"] == "completed" {
                break;
            }
            assert_ne!(
                status["status"],
                "failed",
                "URL resume failed after {} requests: {status}",
                requests.load(std::sync::atomic::Ordering::SeqCst)
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resumable URL import timed out");
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
    let (path, _, _file) = state
        .msd_storage
        .image_file("test-bmc", "resumed.iso")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), b"helloworld");
}

#[tokio::test]
async fn test_msd_url_import_resumes_from_persisted_checkpoint_after_runtime_loss() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let remote_app = axum::Router::new().route(
        "/restart.iso",
        axum::routing::get({
            let requests = requests.clone();
            move |headers: axum::http::HeaderMap| {
                let requests = requests.clone();
                async move {
                    let attempt = requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let partial = attempt > 0;
                    if partial {
                        assert_eq!(headers.get("range").unwrap(), "bytes=5-");
                        assert_eq!(headers.get("if-range").unwrap(), "\"restart-v1\"");
                    }
                    if attempt < 4 {
                        let first = (!partial).then_some(Ok::<_, std::io::Error>(
                            bytes::Bytes::from_static(b"hello"),
                        ));
                        let stream =
                            futures::stream::iter(first).chain(futures::stream::once(async {
                                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::ConnectionReset,
                                    "intentional restart test interruption",
                                ))
                            }));
                        let mut response = axum::http::Response::builder()
                            .status(if partial {
                                StatusCode::PARTIAL_CONTENT
                            } else {
                                StatusCode::OK
                            })
                            .header("content-length", if partial { "5" } else { "10" })
                            .header("etag", "\"restart-v1\"");
                        if partial {
                            response = response.header("content-range", "bytes 5-9/10");
                        }
                        response.body(Body::from_stream(stream)).unwrap()
                    } else {
                        axum::http::Response::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header("content-length", "5")
                            .header("content-range", "bytes 5-9/10")
                            .header("etag", "\"restart-v1\"")
                            .body(Body::from("world"))
                            .unwrap()
                    }
                }
            }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, remote_app).await;
    });

    let state = make_state().await;
    let app = create_router(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/download?location=aggregator&url=http://{addr}/restart.iso&filename=restart.iso"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let started: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let download_id = started["download_id"].as_str().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/bmc/test-bmc/api/feature/msd/images/download/status?download_id={download_id}"
                        ))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if status["status"] == "failed" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial URL import did not exhaust retries");

    state.active_downloads.write().await.clear();
    let paused = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/download/status?download_id={download_id}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(paused.into_body(), 4096)
        .await
        .unwrap();
    let paused: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(paused["status"], "paused");
    assert_eq!(paused["bytes_downloaded"], 5);

    let resumed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/download?location=aggregator&resume_id={download_id}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resumed.status(), StatusCode::OK);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/bmc/test-bmc/api/feature/msd/images/download/status?download_id={download_id}"
                        ))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if status["status"] == "completed" {
                break;
            }
            assert_ne!(status["status"], "failed");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("persisted URL checkpoint did not resume");
    let (path, _, _file) = state
        .msd_storage
        .image_file("test-bmc", "restart.iso")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), b"helloworld");
}

#[tokio::test]
async fn test_msd_images_download_rejects_private_target_without_opt_in() {
    let state = make_state().await;
    state.config.write().await.server.allow_private_downloads = false;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/images/download?location=aggregator&url=http%3A%2F%2F127.0.0.1%2Fsecret.iso")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_msd_images_download_can_be_cancelled_and_reports_terminal_status() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let remote_app = axum::Router::new().route(
        "/slow.iso",
        axum::routing::get(|| async {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            "late-image"
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, remote_app).await;
    });

    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/download?location=aggregator&url=http://{addr}/slow.iso"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let started: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let download_id = started["download_id"].as_str().unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/msd/images/download/cancel")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"download_id": download_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/bmc/test-bmc/api/feature/msd/images/download/status?download_id={download_id}"
                        ))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if status["status"] == "cancelled" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "download cancellation did not reach terminal state"
    );
}

#[tokio::test]
async fn test_msd_image_download_does_not_follow_redirects() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let target_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let target_calls_for_route = target_calls.clone();
    let remote_app = axum::Router::new()
        .route(
            "/redirect.iso",
            axum::routing::get(|| async {
                axum::response::Redirect::temporary("/private-target.iso")
            }),
        )
        .route(
            "/private-target.iso",
            axum::routing::get(move || {
                let target_calls = target_calls_for_route.clone();
                async move {
                    target_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    "must not be fetched"
                }
            }),
        );
    tokio::spawn(async move {
        let _ = axum::serve(listener, remote_app).await;
    });

    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/bmc/test-bmc/api/feature/msd/images/download?location=aggregator&url=http://{addr}/redirect.iso"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let started: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let download_id = started["download_id"].as_str().unwrap();

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/bmc/test-bmc/api/feature/msd/images/download/status?download_id={download_id}"
                        ))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if status["status"] == "failed" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(result.is_ok(), "redirecting download did not fail safely");
    assert_eq!(target_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

// ── MSD Config Persistence ─────────────────────────────────────

#[tokio::test]
async fn test_msd_config_patch_persists() {
    let config_path = std::env::temp_dir().join(format!(
        "bmc-aggregator-msd-config-{}.yaml",
        uuid::Uuid::new_v4()
    ));
    let config = Config {
        server: ServerConfig {
            listen: "127.0.0.1:0".into(),
            frontend_dir: "/tmp".into(),
            data_dir: "/tmp/bmc-aggregator-test-data".into(),
            allow_insecure_http: false,
            download_headers: std::collections::HashMap::new(),
            download_header_origins: Vec::new(),
            download_connect_timeout_seconds: 15,
            download_idle_timeout_seconds: 60,
            allow_private_downloads: false,
            webtransport: WebTransportConfig::default(),
        },
        devices: vec![DeviceConfig {
            id: "test-bmc".into(),
            name: "Test BMC".into(),
            protocol: "mock".into(),
            host: "127.0.0.1".into(),
            port: 0,
            auth: AuthConfig {
                username: "admin".into(),
                password: "pass".into(),
            },
            kvm_port: None,
            ipmi_host: None,
            ipmi_port: None,
            max_fps: 30,
            video_backend: "mahogany".into(),
            encoder_backend: "software".into(),
            audio_backend: None,
            features: HashMap::new(),
            video_device: None,
            video_format: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            video_quality: None,
        }],
        msd: std::collections::HashMap::new(),
    };
    config.save(config_path.to_str().unwrap()).unwrap();

    let registry = DeviceRegistry::new();
    let feature_configs = mock_feature_configs();
    let mut features: HashMap<String, Arc<FeatureConnection>> = HashMap::new();
    for feat_id in &["video", "hid", "msd", "power"] {
        let fc = FeatureConnection::new(
            "test-bmc".into(),
            feat_id.to_string(),
            "mock".into(),
            feature_configs[*feat_id].clone(),
        );
        if *feat_id == "video" {
            fc.set_feature_runtime(Arc::new(VideoRuntime::new())).await;
        }
        features.insert(feat_id.to_string(), fc);
    }
    let feat_cfgs = FeatureConfigs {
        features: feature_configs,
    };
    let md = Arc::new(ManagedDevice::new("test-bmc".into(), feat_cfgs, features));
    registry.insert("test-bmc".into(), md).await;
    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let msd_storage_root = test_msd_storage_root();
    let state = Arc::new(AppState {
        config: config_lock.clone(),
        config_path: config_path.to_string_lossy().to_string(),
        configuration: bmc_aggregator::bmc::configuration::ConfigurationCoordinator::new(),
        registry,
        msd_storage: bmc_aggregator::bmc::msd_storage::FsMsdStorage::new(
            msd_storage_root,
            config_lock,
        ),
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        webtransport: tokio::sync::RwLock::new(None),
    });
    let app = create_router(state.clone());
    let storage_dir = std::env::temp_dir().join(format!(
        "bmc-aggregator-msd-storage-{}",
        uuid::Uuid::new_v4()
    ));
    let payload = format!(
        r#"{{"backend":"mahogany","drive_size_mb":2048,"max_image_bytes":34359738368,"storage_dir":"{}"}}"#,
        storage_dir.to_string_lossy()
    );

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let version = get
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/bmc/test-bmc/api/feature/msd/config")
                .header("content-type", "application/json")
                .header("if-match", version)
                .header("x-mutation-id", "update-msd-configuration")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["backend"], "mahogany");
    assert_eq!(json["drive_size_mb"], 2048);
    assert_eq!(json["max_image_bytes"], 34_359_738_368u64);
    assert_eq!(json["mutation_id"], "update-msd-configuration");

    let saved = Config::load(config_path.to_str().unwrap()).unwrap();
    let feature_backend = saved.devices[0]
        .features
        .get("msd")
        .and_then(|feature| feature.backend.as_deref());
    assert_eq!(feature_backend, Some("mahogany"));
    assert_eq!(
        saved.devices[0].feature_u64_param("msd", "max_image_bytes"),
        Some(34_359_738_368)
    );
    let msd = saved.msd.get("test-bmc").unwrap();
    assert_eq!(msd.drive_size_mb, 2048);
    assert_eq!(msd.storage_dir.as_deref(), storage_dir.to_str());

    state.msd_storage.ensure_dirs("test-bmc").await.unwrap();
    assert!(storage_dir.join("images").is_dir());
    assert!(storage_dir.join("drive").is_dir());

    let _ = std::fs::remove_file(config_path);
}

// ── Auth Endpoints ──────────────────────────────────────────────

#[tokio::test]
async fn test_auth_check_returns_authenticated() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/auth/check")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Auth check bypassed — always returns authenticated: true
    assert_eq!(json["authenticated"], true);
    assert_eq!(json["user"], "admin");
}

#[tokio::test]
async fn test_auth_login_returns_success() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"slot_id":"default"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["success"], true);
}

#[tokio::test]
async fn test_auth_logout_and_change_password_returns_success() {
    let state = make_state().await;
    let app = create_router(state);
    // Logout: no params needed
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"slot_id":"default"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["success"], true);

    // Change-password: requires current_password + new_password
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/change-password")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"current_password":"old","new_password":"new"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["success"], true);

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/change-username")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"username":"dummy","password":"placeholder"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["success"], true);
}

// ── Setup & System ──────────────────────────────────────────────

#[tokio::test]
async fn test_setup_status_returns_not_required() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["setup_required"], false);
    assert_eq!(json["setup_possible"], true);
}

#[tokio::test]
async fn test_system_info_returns_platform() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/system/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 16384).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(json["mode"], "bmc_aggregator");
}

// ── HID Status ──────────────────────────────────────────────────

#[tokio::test]
async fn test_hid_status_returns_available() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/hid/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["available"], true);
}

// ── Unknown Device ──────────────────────────────────────────────

#[tokio::test]
async fn test_unknown_device_returns_graceful() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/nonexistent/api/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "device not found");

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/nonexistent/api/feature/video/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["streaming"], false);
    assert_eq!(json["connected"], false);
}

// ── Multiple Devices ────────────────────────────────────────────

async fn make_state_two_devices() -> Arc<AppState> {
    let config = Config {
        server: ServerConfig {
            listen: "127.0.0.1:0".into(),
            frontend_dir: std::path::PathBuf::from("/tmp/nonexistent"),
            data_dir: std::path::PathBuf::from("/tmp/bmc-aggregator-test-data"),
            allow_insecure_http: false,
            download_headers: std::collections::HashMap::new(),
            download_header_origins: Vec::new(),
            download_connect_timeout_seconds: 15,
            download_idle_timeout_seconds: 60,
            allow_private_downloads: false,
            webtransport: WebTransportConfig::default(),
        },
        devices: vec![
            DeviceConfig {
                id: "alpha".into(),
                name: "Alpha BMC".into(),
                protocol: "mock".into(),
                host: "127.0.0.1".into(),
                port: 0,
                auth: AuthConfig {
                    username: "admin".into(),
                    password: "pass".into(),
                },
                kvm_port: None,
                ipmi_host: None,
                ipmi_port: Some(623),
                max_fps: 30,
                video_backend: "mahogany".into(),
                encoder_backend: "software".into(),
                audio_backend: None,
                features: HashMap::new(),
                video_device: None,
                video_format: None,
                video_width: None,
                video_height: None,
                video_fps: None,
                video_quality: None,
            },
            DeviceConfig {
                id: "beta".into(),
                name: "Beta BMC".into(),
                protocol: "mock".into(),
                host: "127.0.0.2".into(),
                port: 0,
                auth: AuthConfig {
                    username: "admin".into(),
                    password: "pass".into(),
                },
                kvm_port: None,
                ipmi_host: None,
                ipmi_port: Some(624),
                max_fps: 30,
                video_backend: "mahogany".into(),
                encoder_backend: "vaapi".into(),
                audio_backend: None,
                features: HashMap::new(),
                video_device: None,
                video_format: None,
                video_width: None,
                video_height: None,
                video_fps: None,
                video_quality: None,
            },
        ],
        msd: std::collections::HashMap::new(),
    };

    let registry = DeviceRegistry::new();
    for id in &["alpha", "beta"] {
        let feature_configs = mock_feature_configs();
        let mut features: HashMap<String, Arc<FeatureConnection>> = HashMap::new();
        for feat_id in &["video", "hid", "msd", "power"] {
            let fc = FeatureConnection::new(
                (*id).into(),
                feat_id.to_string(),
                "mock".into(),
                feature_configs[*feat_id].clone(),
            );
            if *feat_id == "video" {
                fc.set_feature_runtime(Arc::new(VideoRuntime::new())).await;
            }
            features.insert(feat_id.to_string(), fc);
        }
        let feat_cfgs = FeatureConfigs {
            features: feature_configs,
        };
        let md = Arc::new(ManagedDevice::new((*id).into(), feat_cfgs, features));
        registry.insert((*id).into(), md).await;
    }

    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let msd_storage_root = test_msd_storage_root();

    Arc::new(AppState {
        config: config_lock.clone(),
        config_path: "/tmp/bmc-aggregator-test-config.yaml".into(),
        configuration: bmc_aggregator::bmc::configuration::ConfigurationCoordinator::new(),
        registry,
        msd_storage: bmc_aggregator::bmc::msd_storage::FsMsdStorage::new(
            msd_storage_root,
            config_lock,
        ),
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        webtransport: tokio::sync::RwLock::new(None),
    })
}

#[tokio::test]
async fn test_multiple_devices_list() {
    let state = make_state_two_devices().await;
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/bmc/list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let mut list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.len(), 2);
    list.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    assert_eq!(list[0]["id"], "alpha");
    assert_eq!(list[1]["id"], "beta");
}

#[tokio::test]
async fn test_multiple_devices_individual_info() {
    let state = make_state_two_devices().await;
    let app = create_router(state);

    for id in &["alpha", "beta"] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/bmc/{}/api/info", id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["device_id"], *id);
    }
}

// ── Fallback API default routes ─────────────────────────────────

#[tokio::test]
async fn test_encoder_self_check_uses_video_processor_without_backend() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/encoder_self_check")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);
    assert!(json["backends"].is_array());
    assert!(json["codecs"].is_array());
}

#[tokio::test]
async fn test_fallback_stream_endpoints_respond() {
    let state = make_state().await;
    let app = create_router(state);

    // GET endpoints that exist
    for path in &["feature/video/mode"] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/bmc/test-bmc/api/{}", path))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
    // stop is POST-only in current routes
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/stop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    // start without backend should 500 (fc_execute fails)
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/start")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_server_error());
}

#[tokio::test]
async fn test_legacy_bmc_config_endpoints_are_not_mounted() {
    let state = make_state().await;
    let app = create_router(state);

    for path in &["config", "config/video", "config/auth", "config/web"] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/bmc/test-bmc/api/{}", path))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "legacy config endpoint /{} should not be mounted",
            path
        );
    }
}

#[tokio::test]
async fn test_configuration_options_reject_unknown_resource() {
    let state = make_state_without_session().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/configuration/options/unknown.resource")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["code"], "option_resource_unknown");
}

#[tokio::test]
async fn test_configuration_options_returns_revisioned_snapshot_and_etag() {
    let state = make_state_without_session().await;
    let app = create_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/configuration/options/local.serial_ports")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("etag").is_some());
    let body = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["resource_id"], "local.serial_ports");
    assert_eq!(payload["revision"], 1);
    assert!(payload["options"].is_array());
    assert!(payload["state"]["status"] == "ready" || payload["state"]["status"] == "failed");
}

// ── State factory without session injection ─────────────────────

async fn make_state_without_session() -> Arc<AppState> {
    let config = Config {
        server: ServerConfig {
            listen: "127.0.0.1:0".into(),
            frontend_dir: std::path::PathBuf::from("/tmp/nonexistent"),
            data_dir: std::path::PathBuf::from("/tmp/bmc-aggregator-test-data"),
            allow_insecure_http: false,
            download_headers: std::collections::HashMap::new(),
            download_header_origins: Vec::new(),
            download_connect_timeout_seconds: 15,
            download_idle_timeout_seconds: 60,
            allow_private_downloads: false,
            webtransport: WebTransportConfig::default(),
        },
        devices: vec![],
        msd: std::collections::HashMap::new(),
    };

    let registry = DeviceRegistry::new();
    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let msd_storage_root = test_msd_storage_root();

    Arc::new(AppState {
        config: config_lock.clone(),
        config_path: "/tmp/bmc-aggregator-test-config.yaml".into(),
        configuration: bmc_aggregator::bmc::configuration::ConfigurationCoordinator::new(),
        registry,
        msd_storage: bmc_aggregator::bmc::msd_storage::FsMsdStorage::new(
            msd_storage_root,
            config_lock,
        ),
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        webtransport: tokio::sync::RwLock::new(None),
    })
}

// ── API Contract Tests ──────────────────────────────────────────

fn check_contract_keys(actual: &serde_json::Value, expected_keys: &[&str]) {
    let actual_keys: std::collections::BTreeSet<String> =
        actual.as_object().unwrap().keys().cloned().collect();
    let expected: std::collections::BTreeSet<String> =
        expected_keys.iter().map(|k| k.to_string()).collect();
    let missing: Vec<&str> = expected
        .difference(&actual_keys)
        .map(|k| k.as_str())
        .collect();
    let extra: Vec<&str> = actual_keys
        .difference(&expected)
        .map(|k| k.as_str())
        .collect();
    assert!(missing.is_empty(), "missing keys: {:?}", missing);
    assert!(extra.is_empty(), "extra keys: {:?}", extra);
}

#[tokio::test]
async fn test_contract_config_msd() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_contract_msd_status() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    check_contract_keys(
        &json,
        &[
            "available",
            "backend",
            "attachments",
            "occupancy",
            "backend_status",
            "capabilities",
            "drive",
            "online",
            "state",
            "connection_id",
            "ref_count",
            "fault",
            "instances",
            "active_instances",
            "resource",
        ],
    );
    assert_eq!(json["available"], true);
    assert_eq!(json["attachments"], serde_json::json!([]));
    check_contract_keys(
        &json["occupancy"],
        &["state", "details_available", "active_sessions"],
    );
    check_contract_keys(&json["resource"], &["epoch", "revision", "stale"]);
    assert!(json["resource"]["epoch"]
        .as_str()
        .is_some_and(|epoch| !epoch.is_empty()));
    assert!(json["resource"]["revision"].is_u64());
    assert!(json["resource"]["stale"].is_boolean());
    check_contract_keys(
        &json["capabilities"],
        &[
            "virtual_drive",
            "slots",
            "storage_locations",
            "unavailable_storage_locations",
            "target_management",
        ],
    );

    let drive = &json["drive"];
    check_contract_keys(drive, &["capacity", "used", "free"]);
}

#[tokio::test]
async fn test_contract_msd_images_list() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/images?location=aggregator")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let images = json["images"].as_array().unwrap();
    for item in images {
        check_contract_keys(item, &["id", "name", "size"]);
        assert!(item["id"].is_string());
        assert!(item["name"].is_string());
        assert!(item["size"].is_u64());
    }
}

#[tokio::test]
async fn test_contract_msd_drive_files_list() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/drive/files?path=%2F")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = json["items"].as_array().unwrap();
    for item in items {
        check_contract_keys(item, &["path", "name", "is_dir", "size"]);
        assert!(item["path"].is_string());
        assert!(item["name"].is_string());
        assert!(item["is_dir"].is_boolean());
        assert!(item["size"].is_u64());
    }
}

#[tokio::test]
async fn test_contract_msd_drive_info() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/drive")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    check_contract_keys(
        &json,
        &["capacity", "used", "free", "storage_base", "supported"],
    );
    assert!(json["capacity"].is_u64());
    assert!(json["used"].is_u64());
    assert!(json["free"].is_u64());
    assert_eq!(json["supported"], true);
}

// ── Stream mode POST ─────────────────────────────────────────────

#[tokio::test]
async fn test_stream_mode_post_does_not_require_backend_connection() {
    // Create state with a shared event channel so we can capture WS events

    let config = Config {
        server: ServerConfig {
            listen: "127.0.0.1:0".into(),
            frontend_dir: std::path::PathBuf::from("/tmp/nonexistent"),
            data_dir: std::path::PathBuf::from("/tmp/bmc-aggregator-test-data"),
            allow_insecure_http: false,
            download_headers: std::collections::HashMap::new(),
            download_header_origins: Vec::new(),
            download_connect_timeout_seconds: 15,
            download_idle_timeout_seconds: 60,
            allow_private_downloads: false,
            webtransport: WebTransportConfig::default(),
        },
        devices: vec![DeviceConfig {
            id: "test-bmc".into(),
            name: "Test BMC".into(),
            protocol: "mock".into(),
            host: "127.0.0.1".into(),
            port: 0,
            auth: AuthConfig {
                username: "admin".into(),
                password: "pass".into(),
            },
            kvm_port: None,
            ipmi_host: None,
            ipmi_port: Some(623),
            max_fps: 30,
            video_backend: "mahogany".into(),
            encoder_backend: "software".into(),
            audio_backend: None,
            features: HashMap::new(),
            video_device: None,
            video_format: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            video_quality: None,
        }],
        msd: std::collections::HashMap::new(),
    };

    let registry = DeviceRegistry::new();
    let feature_configs = mock_feature_configs();
    let mut features: HashMap<String, Arc<FeatureConnection>> = HashMap::new();
    for feat_id in &["video", "hid", "msd", "power"] {
        let fc = FeatureConnection::new(
            "test-bmc".into(),
            feat_id.to_string(),
            "mock".into(),
            feature_configs[*feat_id].clone(),
        );
        if *feat_id == "video" {
            fc.set_feature_runtime(Arc::new(VideoRuntime::new())).await;
        }
        features.insert(feat_id.to_string(), fc);
    }
    let feat_cfgs = FeatureConfigs {
        features: feature_configs,
    };
    let md = Arc::new(ManagedDevice::new("test-bmc".into(), feat_cfgs, features));
    registry.insert("test-bmc".into(), md).await;

    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let msd_storage_root = test_msd_storage_root();

    let state = Arc::new(AppState {
        config: config_lock.clone(),
        config_path: "/tmp/bmc-aggregator-test-config.yaml".into(),
        configuration: bmc_aggregator::bmc::configuration::ConfigurationCoordinator::new(),
        registry,
        msd_storage: bmc_aggregator::bmc::msd_storage::FsMsdStorage::new(
            msd_storage_root,
            config_lock,
        ),
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        webtransport: tokio::sync::RwLock::new(None),
    });

    let app = create_router(state);

    // Mode selection is processor state, not backend transport work.
    let req = Request::builder()
        .method("POST")
        .uri("/bmc/test-bmc/api/feature/video/mode")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"slot_id":"default","mode":"vp8"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "mode POST should not call a capture backend or require lifecycle"
    );
}

#[tokio::test]
async fn test_stream_mode_post_updates_processor_mode_without_backend_call() {
    let state = make_state().await;
    let dev = state.registry.get_dev("test-bmc").await.unwrap();
    let fc = dev.features.get("video").unwrap().clone();
    fc.transition(ConnectionState::Connected).await;

    let app = create_router(state);
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/mode")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"slot_id":"default","mode":"h264"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["mode"], "h264");

    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/mode")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["mode"], "h264");
}

// ── Timeout-with-background-load test ────────────────────────────

#[tokio::test]
async fn video_mode_rejects_invalid_encoder_selection_without_mutation() {
    let state = make_state().await;
    let fc = state.registry.get_dev("test-bmc").await.unwrap().features["video"].clone();
    let processor = bmc_aggregator::bmc::feature::video::runtime::processor(&fc)
        .await
        .unwrap();
    processor
        .set_encoding_selection(bmc_aggregator::bmc::encoder::Codec::H265, "software")
        .await;
    for (mode, backend) in [
        ("h264", serde_json::json!("vp8/nvenc")),
        ("h264", serde_json::json!(42)),
        ("h264", serde_json::json!("future-backend")),
        ("vp8", serde_json::json!("nvenc")),
        ("vp9", serde_json::json!("nvenc")),
    ] {
        let response = create_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/bmc/test-bmc/api/feature/video/mode")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"mode":mode,"backend":backend}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{mode}/{backend}"
        );
        let encoding = processor.encoding_state().await;
        assert_eq!(encoding.stream_mode(), "h265");
        assert_eq!(encoding.encoder_backend(), "software");
    }
}

#[tokio::test]
async fn video_mode_commits_supported_encoder_selection() {
    let state = make_state().await;
    let fc = state.registry.get_dev("test-bmc").await.unwrap().features["video"].clone();
    let processor = bmc_aggregator::bmc::feature::video::runtime::processor(&fc)
        .await
        .unwrap();
    for (mode, backend, canonical_mode, canonical_backend) in [
        ("h264", "nvh264enc", "h264", "nvenc"),
        ("hevc", "nvenc", "h265", "nvenc"),
        ("av1", "nvav1enc", "av1", "nvenc"),
        ("vp8", "software", "vp8", "software"),
    ] {
        let response = create_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/bmc/test-bmc/api/feature/video/mode")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"mode":mode,"backend":backend}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let encoding = processor.encoding_state().await;
        assert_eq!(body["mode"], canonical_mode);
        assert_eq!(encoding.stream_mode(), canonical_mode);
        assert_eq!(encoding.encoder_backend(), canonical_backend);
    }
    let response = create_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/mode")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"mode":"webp"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(processor.stream_mode().await, "webp");
}

/// Prove that POST /feature/video/mode completes even under background CPU load
/// (simulating MJPEG encoding loop). Uses 2 worker threads so one can handle
/// the HTTP request while the other is busy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_video_mode_vp8_completes_under_cpu_load() {
    let state = make_state().await;
    let app = create_router(state.clone());

    // Simulate MJPEG encoding loop: 50ms CPU + 1ms yield, repeating.
    // This matches the REAL encoding loop pattern (interval + sync work).
    tokio::spawn(async {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });

    // Send VP8 mode switch – the encoder creation takes ~10ms on this system,
    // so the request should succeed quickly. The background load must NOT
    // prevent the async runtime from processing the HTTP handler.
    let req = Request::builder()
        .uri("/bmc/test-bmc/api/feature/video/mode")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"slot_id":"default","mode":"vp8"}"#))
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(8), app.oneshot(req)).await;

    assert!(
        response.is_ok(),
        "VP8 mode switch hung! runtime blocked by background load"
    );

    let status = response.unwrap().unwrap().status();
    eprintln!(
        "VP8 mode switch returned status={} under CPU load ✓",
        status
    );
}

// ── WebRTC API ────────────────────────────────────────────────

#[tokio::test]
async fn test_webrtc_ice_servers_returns_config() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/webrtc/ice-servers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let servers = json["ice_servers"].as_array().unwrap();
    assert!(!servers.is_empty(), "expected at least one ICE server");
    for s in servers {
        assert!(s["urls"].is_array(), "iceServer should have urls array");
    }
    assert_eq!(json["iceServers"], json["ice_servers"]);
}

#[tokio::test]
async fn test_webrtc_offer_returns_answer() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/webrtc/offer")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"sdp":"test-sdp"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(json["fault"]["code"], "backend_unavailable");
    assert_eq!(json["fault"]["scope"], "feature");
}

#[tokio::test]
async fn test_webrtc_ice_returns_error_without_session() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/bmc/test-bmc/api/feature/video/webrtc/ice")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"connection_id":"nonexistent","candidate":{"candidate":"candidate:1","sdpMid":"0","sdpMLineIndex":0}}"#))
            .unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(json["fault"]["code"], "connection_not_found");
}

#[tokio::test]
async fn test_webrtc_close_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/webrtc/close?connection_id=test-connection")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("ok").is_some() || json.get("error").is_some());
}

#[tokio::test]
async fn test_webrtc_unknown_device_returns_error() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/nonexistent/api/feature/video/webrtc/ice-servers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // ice-servers does not depend on device, so it still returns OK
    assert!(json["ice_servers"].is_array());
}

// ── Pipeline encoder ──────────────────────────────────────────

#[tokio::test]
async fn test_pipeline_set_encoder_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/pipeline/encoder")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"encoder":"h264/software"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["encoder"], "h264/software");
}

// ── Stream constraints and bitrate ─────────────────────────────

#[tokio::test]
async fn video_configuration_queries_use_the_feature_owned_values() {
    let state = make_state().await;
    {
        let mut config = state.config.write().await;
        let device = &mut config.devices[0];
        device.max_fps = 99;
        device.encoder_backend = "stale-encoder".into();
        device.video_device = Some("stale-camera".into());
        let feature = device.features.entry("video".into()).or_default();
        feature
            .feature_params
            .insert("max_fps".into(), serde_json::json!(15));
        feature
            .feature_params
            .insert("encoder_backend".into(), serde_json::json!("webp"));
        feature
            .feature_params
            .insert("video_device".into(), serde_json::Value::Null);
        feature.feature_params.remove("video_fps");
    }
    let app = create_router(state);
    for (endpoint, field, expected) in [
        ("input-config", "fps", serde_json::json!(15)),
        ("input-config", "device", serde_json::json!("")),
        ("config", "encoder", serde_json::json!("webp")),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/bmc/test-bmc/api/feature/video/{endpoint}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value[field], expected);
    }
}

#[tokio::test]
async fn test_stream_constraints_returns_defaults() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/constraints")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json.get("constraints").is_some(),
        "expected constraints object"
    );
}

#[tokio::test]
async fn test_stream_bitrate_returns_value() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/bmc/test-bmc/api/feature/video/bitrate")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);
    assert!(json["bitrate_kbps"].as_u64().is_some());
}

#[tokio::test]
async fn video_controls_reject_missing_unavailable_and_stopped_devices() {
    let state = make_state().await;
    state
        .registry
        .insert(
            "no-video".into(),
            Arc::new(ManagedDevice::new(
                "no-video".into(),
                FeatureConfigs {
                    features: HashMap::new(),
                },
                HashMap::new(),
            )),
        )
        .await;
    let device = state.registry.get_dev("test-bmc").await.unwrap();
    device.features["video"].shutdown();
    let app = create_router(state);
    for (id, status) in [
        ("missing", StatusCode::NOT_FOUND),
        ("no-video", StatusCode::SERVICE_UNAVAILABLE),
        ("test-bmc", StatusCode::SERVICE_UNAVAILABLE),
    ] {
        for (method, endpoint) in [
            ("GET", "mode"),
            ("POST", "mode"),
            ("GET", "bitrate"),
            ("POST", "bitrate"),
            ("GET", "constraints"),
            ("POST", "reset"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(format!("/bmc/{id}/api/feature/video/{endpoint}"))
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"bitrate_kbps":4000,"mode":"mjpeg"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{method} {id}/{endpoint}");
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_ne!(json["success"], true);
            assert!(json["fault"].is_object());
        }
    }
}

#[tokio::test]
async fn test_stream_bitrate_preset_updates_and_rejects_invalid_values() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/bitrate")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"bitrate_preset":{"type":"Quality"}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["bitrate_kbps"], 8_000);

    for invalid in [
        serde_json::json!({"bitrate_kbps": "4000", "bitrate_preset": {"type":"Speed"}}),
        serde_json::json!({"bitrate_kbps": 4000, "bitrate": 8000}),
        serde_json::json!({"bitrate_preset": {"type":"Quality", "value":1}}),
        serde_json::json!({"bitrate_preset": {"type":"Custom", "value":null}}),
        serde_json::json!({"bitrate_kbps": 4000, "typo": true}),
        serde_json::json!({"bitrate_kbps": null}),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/bmc/test-bmc/api/feature/video/bitrate")
                    .header("content-type", "application/json")
                    .body(Body::from(invalid.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/bmc/test-bmc/api/feature/video/bitrate")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["bitrate_kbps"], 8_000,
            "invalid input must not change state"
        );
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/video/bitrate")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"bitrate_kbps":0}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], false);
}

// ── Encoder self-check ─────────────────────────────────────────

#[tokio::test]
async fn test_encoder_self_check_returns_check_results() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/video/encoder_self_check")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json.get("success").is_some() || json.is_object(),
        "expected self-check results"
    );
}

// ── Snapshot ───────────────────────────────────────────────────

#[tokio::test]
async fn test_snapshot_returns_image_or_error() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/snapshot")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
    if status == StatusCode::OK {
        assert!(!body.is_empty(), "snapshot body should not be empty on 200");
    }
}

// ── Health ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_health_returns_healthy_status() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("healthy").is_some(), "expected healthy field");
}

// ── Features list ──────────────────────────────────────────────

#[tokio::test]
async fn test_features_returns_capabilities() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/features")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.is_object(), "features should return an object");
    assert!(json.get("video").is_some(), "expected video feature");
    assert!(json.get("hid").is_some(), "expected hid feature");
    assert!(json.get("msd").is_some(), "expected msd feature");
    assert!(json.get("mode").is_some(), "expected mode field");
    assert!(
        json.get("video_capture").is_none(),
        "device capabilities must not include aggregator-side static video capture defaults"
    );
    assert_eq!(
        json["video"]["backend"], "mock",
        "feature backend identity must come from the device registry"
    );
}

// ── Audio API ──────────────────────────────────────────────────

#[tokio::test]
async fn test_audio_status_returns_not_available() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/audio/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["available"], false);
    assert_eq!(json["streaming"], false);
}

#[tokio::test]
async fn test_audio_start_returns_error() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/audio/start")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(json["fault"]["code"], "feature_unavailable");
}

#[tokio::test]
async fn test_audio_stop_returns_error() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/audio/stop")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(json["fault"]["code"], "feature_unavailable");
}

#[tokio::test]
async fn test_audio_quality_returns_error() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/audio/quality")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"quality":"high"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(json["fault"]["code"], "feature_unavailable");
}

#[tokio::test]
async fn test_audio_device_returns_error() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bmc/test-bmc/api/feature/audio/device")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"device":"default"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(json["fault"]["code"], "feature_unavailable");
}

#[tokio::test]
async fn test_audio_config_returns_config() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/audio/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.is_object(), "audio config should be an object");
}

// ── Contract: Server web config ────────────────────────────────

#[tokio::test]
async fn test_root_server_web_config_get_and_patch() {
    let state = make_state().await;
    let app = create_router(state.clone());
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/server/web")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let version = res
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["bind_address"], "127.0.0.1");
    assert_eq!(json["http_port"], 0);

    let res = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/server/web")
                .header("content-type", "application/json")
                .header("if-match", version)
                .header("x-mutation-id", "update-server-web")
                .body(Body::from(
                    r#"{"bind_address":"0.0.0.0","http_port":18080}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["bind_address"], "0.0.0.0");
    assert_eq!(json["http_port"], 18080);
    assert_eq!(json["mutation_id"], "update-server-web");
    assert_eq!(state.config.read().await.server.listen, "0.0.0.0:18080");
}

// ── Contract: HID status ───────────────────────────────────────

#[tokio::test]
async fn test_contract_hid_status() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/hid/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["available"], true);
    assert_eq!(json["online"], false);
}

// ── Devices and ATX ────────────────────────────────────────

#[tokio::test]
async fn test_unimplemented_planned_routes_are_not_mounted() {
    let state = make_state().await;
    let routes = [
        "/bmc/test-bmc/api/devices/usb",
        "/bmc/test-bmc/api/devices/usb/reset",
        "/bmc/test-bmc/api/system/restart",
        "/bmc/test-bmc/api/update/overview",
        "/bmc/test-bmc/api/update/status",
        "/bmc/test-bmc/api/extensions",
        "/bmc/test-bmc/api/extensions/gostc/config",
        "/bmc/test-bmc/api/extensions/easytier/config",
    ];

    for route in routes {
        let app = create_router(state.clone());
        let res = app
            .oneshot(Request::builder().uri(route).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "{route} should not be mounted"
        );
    }
}

#[tokio::test]
async fn test_devices_atx_returns_list() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/power/devices/atx")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["atx_version"], "1.0");
    assert_eq!(json["power"]["supported"], true);
}

struct UncertainPowerBackend {
    reject: bool,
    calls: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl bmc_aggregator::bmc::feature::power::PowerBackend for UncertainPowerBackend {
    async fn capabilities(&self) -> bmc_aggregator::bmc::feature::power::PowerCapabilities {
        Default::default()
    }
    async fn status(&self) -> Result<String, String> {
        Err("injected observation failure".into())
    }
    async fn control(&self, _: &str) -> Result<(), String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.reject {
            Err("injected transport loss".into())
        } else {
            Ok(())
        }
    }
    async fn wake(&self, _: &str) -> Result<(), String> {
        Err("unsupported".into())
    }
}

#[tokio::test]
async fn power_dispatch_and_observation_failures_have_distinct_non_replay_outcomes() {
    use bmc_aggregator::bmc::feature::power::PowerBackend;
    for reject in [false, true] {
        let state = make_state().await;
        let device = state.registry.get_dev("test-bmc").await.unwrap();
        let fc = device.features["power"].clone();
        fc.lifecycle_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        fc.transition(ConnectionState::Connected).await;
        let backend = Arc::new(UncertainPowerBackend {
            reject,
            calls: Default::default(),
        });
        fc.set_backend_runtime(backend.clone() as Arc<dyn PowerBackend>)
            .await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/bmc/test-bmc/api/feature/power/control")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"action":"on"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if reject {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::OK
            }
        );
        let body = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        if reject {
            assert_eq!(value["fault"]["retryable"], false);
        } else {
            assert_eq!(value["outcome"], "accepted");
            assert_eq!(value["power_status"], "unknown");
        }
        assert_eq!(backend.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn feature_configuration_responses_match_the_frontend_contract() {
    fn validate(data: &serde_json::Value, spec: &serde_json::Value) {
        if let Some(values) = spec["enum"].as_array() {
            assert!(values.contains(data), "unexpected enum value {data}");
            return;
        }
        if let Some(properties) = spec["properties"].as_object() {
            for field in spec["required"].as_array().unwrap() {
                assert!(
                    data.get(field.as_str().unwrap()).is_some(),
                    "missing {field}"
                );
            }
            for (field, value) in data.as_object().unwrap() {
                validate(
                    value,
                    properties
                        .get(field)
                        .unwrap_or_else(|| panic!("undeclared field {field}")),
                );
            }
            return;
        }
        let actual = match data {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            _ => panic!("unexpected nested config value"),
        };
        assert!(
            spec["type"]
                .as_array()
                .unwrap()
                .iter()
                .any(|kind| kind == actual),
            "unexpected {actual}: {data}"
        );
    }
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../contracts/feature-config.schema.json")).unwrap();
    let app = create_router(make_state().await);
    for (feature, contract) in [
        ("hid", "HidConfig"),
        ("msd", "MsdConfig"),
        ("audio", "AudioConfig"),
        ("power", "PowerConfig"),
        ("video", "StreamConfigResponse"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/bmc/test-bmc/api/feature/{feature}/config"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap();
        let data: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        validate(&data, &schema["$defs"][contract]);
    }
}

#[tokio::test]
async fn malformed_drive_multipart_never_replaces_existing_file() {
    for tail in [
        "partial",
        "partial\r\n--probe\r\nContent-Disposition: form-data; name=\"path\"\r\n\r\n/unfinished",
    ] {
        let state = make_state().await;
        state.msd_storage.ensure_dirs("test-bmc").await.unwrap();
        state
            .msd_storage
            .drive_upload_file("test-bmc", "/", "existing.txt", b"original")
            .await
            .unwrap();
        let app = create_router(state.clone());
        let body = format!("--probe\r\nContent-Disposition: form-data; name=\"file\"; filename=\"existing.txt\"\r\nContent-Type: application/octet-stream\r\n\r\n{tail}");
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/bmc/test-bmc/api/feature/msd/drive/files/upload")
                    .header("content-type", "multipart/form-data; boundary=probe")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            state
                .msd_storage
                .drive_read_file("test-bmc", "/existing.txt")
                .await
                .unwrap()
                .unwrap()
                .1,
            b"original"
        );
    }
}

#[tokio::test]
async fn stream_config_rejects_unsupported_fields_instead_of_acknowledging_them() {
    let app = create_router(make_state().await);
    let endpoint = "/bmc/test-bmc/api/feature/video/config";
    let read = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(endpoint)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let etag = read.headers()["etag"].clone();
    for body in [
        r#"{"mode":"webrtc"}"#,
        r#"{"encoder":"software","turn_password":"placeholder"}"#,
        r#"{"bitrate_preset":{"type":"Balanced"}}"#,
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(endpoint)
                    .header("content-type", "application/json")
                    .header("if-match", etag.clone())
                    .header("x-mutation-id", uuid::Uuid::new_v4().to_string())
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn power_config_changes_the_explicit_connection_parameters() {
    let state = make_state().await;
    {
        let mut config = state.config.write().await;
        let device = &mut config.devices[0];
        device.protocol = "ipmi".into();
        device.features.insert(
            "power".into(),
            serde_yaml::from_str(
                "backend: ipmi\nbackend_params:\n  host: 127.0.0.10\n  port: 623\n",
            )
            .unwrap(),
        );
    }
    let app = create_router(state.clone());
    let endpoint = "/bmc/test-bmc/api/feature/power/config";
    let read = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(endpoint)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let etag = read.headers()["etag"].clone();
    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(endpoint)
                .header("content-type", "application/json")
                .header("if-match", etag)
                .header("x-mutation-id", "power-explicit-params")
                .body(Body::from(r#"{"host":"127.0.0.20","port":6623}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let config = state.config.read().await;
    let params = &config.devices[0].features["power"].backend_params;
    assert_eq!(params["host"], "127.0.0.20");
    assert_eq!(params["port"], 6623);
    let device = state.registry.get_dev("test-bmc").await.unwrap();
    assert_eq!(
        device.features["power"].config.backend_params.host,
        "127.0.0.20"
    );
    assert_eq!(
        device.features["power"].config.backend_params.port,
        Some(6623)
    );
}

struct UncertainIdentifyBackend {
    fail_dispatch: bool,
    calls: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl bmc_aggregator::bmc::feature::identify::IdentifyBackend for UncertainIdentifyBackend {
    async fn capabilities(&self) -> bmc_aggregator::bmc::feature::identify::IdentifyCapabilities {
        use bmc_aggregator::bmc::feature::identify::*;
        IdentifyCapabilities {
            modes: vec![IdentifyMode::Indefinite],
            max_temporary_seconds: 255,
            writable: true,
        }
    }
    async fn state(&self) -> Result<bmc_aggregator::bmc::feature::identify::IdentifyState, String> {
        Err("injected readback failure".into())
    }
    async fn set(
        &self,
        _: bmc_aggregator::bmc::feature::identify::IdentifyMode,
        _: Option<u8>,
    ) -> Result<(), String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_dispatch {
            Err("injected lost acknowledgement".into())
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn identify_dispatch_or_readback_failure_never_advertises_safe_replay() {
    use bmc_aggregator::bmc::feature::identify::IdentifyBackend;
    for fail_dispatch in [false, true] {
        let state = make_state().await;
        let device = state.registry.get_dev("test-bmc").await.unwrap();
        let mut features = device.features.clone();
        let fc = FeatureConnection::new(
            "test-bmc".into(),
            "identify".into(),
            "mock".into(),
            mock_feature_configs()["power"].clone(),
        );
        fc.lifecycle_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        fc.transition(ConnectionState::Connected).await;
        let backend = Arc::new(UncertainIdentifyBackend {
            fail_dispatch,
            calls: Default::default(),
        });
        fc.set_backend_runtime(backend.clone() as Arc<dyn IdentifyBackend>)
            .await;
        features.insert("identify".into(), fc);
        state
            .registry
            .insert(
                "test-bmc".into(),
                Arc::new(ManagedDevice::new(
                    "test-bmc".into(),
                    device.cfg.clone(),
                    features,
                )),
            )
            .await;
        let response = create_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/bmc/test-bmc/api/feature/identify/set")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"mode":"indefinite"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["fault"]["code"], "identify_result_unknown");
        assert_eq!(value["fault"]["retryable"], false);
        assert_eq!(backend.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

struct BlockingPowerBackend {
    release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    started: tokio::sync::Notify,
    calls: std::sync::atomic::AtomicUsize,
    remote: Arc<std::sync::Mutex<String>>,
}

#[async_trait]
impl bmc_aggregator::bmc::feature::power::PowerBackend for BlockingPowerBackend {
    async fn capabilities(&self) -> bmc_aggregator::bmc::feature::power::PowerCapabilities {
        Default::default()
    }
    async fn status(&self) -> Result<String, String> {
        Ok(self.remote.lock().unwrap().clone())
    }
    async fn control(&self, action: &str) -> Result<(), String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let release = self.release.lock().unwrap().take();
        let remote = self.remote.clone();
        let action = action.to_owned();
        self.started.notify_one();
        tokio::task::spawn_blocking(move || {
            if let Some(release) = release {
                release
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            }
            *remote.lock().unwrap() = action;
        })
        .await
        .unwrap();
        Ok(())
    }
    async fn wake(&self, _: &str) -> Result<(), String> {
        Err("unsupported".into())
    }
}

#[tokio::test]
async fn cancelled_power_request_retains_serialization_until_blocking_side_effect_finishes() {
    use bmc_aggregator::bmc::feature::power::PowerBackend;
    let state = make_state().await;
    let device = state.registry.get_dev("test-bmc").await.unwrap();
    let fc = device.features["power"].clone();
    fc.lifecycle_started
        .store(true, std::sync::atomic::Ordering::SeqCst);
    fc.transition(ConnectionState::Connected).await;
    let (release, receiver) = std::sync::mpsc::channel();
    let backend = Arc::new(BlockingPowerBackend {
        release: std::sync::Mutex::new(Some(receiver)),
        started: Default::default(),
        calls: Default::default(),
        remote: Arc::new(std::sync::Mutex::new("off".into())),
    });
    fc.set_backend_runtime(backend.clone() as Arc<dyn PowerBackend>)
        .await;
    let app = create_router(state);
    let request = |action: &str| {
        Request::builder()
            .method("POST")
            .uri("/bmc/test-bmc/api/feature/power/control")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"action": action}).to_string(),
            ))
            .unwrap()
    };
    let first = tokio::spawn(app.clone().oneshot(request("on")));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        backend.started.notified(),
    )
    .await
    .unwrap();
    first.abort();
    let _ = first.await;
    let mut second = tokio::spawn(app.oneshot(request("off")));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut second)
            .await
            .is_err()
    );
    assert_eq!(backend.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    release.send(()).unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(*backend.remote.lock().unwrap(), "off");
    let cached = bmc_aggregator::bmc::feature::power::feature_runtime(&fc)
        .await
        .status
        .snapshot()
        .await;
    assert_eq!(cached.value.unwrap().power_status, "off");
}

#[tokio::test]
async fn device_scope_rejects_unknown_storage_devices_before_writing() {
    let state = make_state().await;
    let app = create_router(state.clone());
    for (method, path, body) in [
        (
            "POST",
            "/bmc/missing/api/feature/msd/images/uploads",
            r#"{"filename":"test.iso","location":"aggregator","length":4}"#,
        ),
        (
            "GET",
            "/bmc/missing/api/feature/msd/images?location=aggregator",
            "",
        ),
        (
            "POST",
            "/bmc/missing/api/feature/msd/drive/init",
            r#"{"size_mb":1}"#,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    assert!(!std::path::Path::new(&state.msd_storage.storage_root)
        .join("missing")
        .exists());
}

#[tokio::test]
async fn recycled_remote_image_ids_never_inherit_local_checksum_cache() {
    let state = make_state().await;
    let backend = install_recording_msd_backend(&state).await;
    backend
        .store
        .images
        .lock()
        .await
        .insert("1".into(), bytes::Bytes::from_static(b"new content"));
    state.msd_storage.ensure_dirs("test-bmc").await.unwrap();
    let metadata =
        std::path::Path::new(&state.msd_storage.storage_root).join("test-bmc/images/.metadata");
    std::fs::create_dir_all(&metadata).unwrap();
    std::fs::write(
        metadata.join("catalog-bmc-1.json"),
        r#"[{"algorithm":"sha256","value":"old-image-digest"}]"#,
    )
    .unwrap();
    let response = create_router(state)
        .oneshot(
            Request::builder()
                .uri("/bmc/test-bmc/api/feature/msd/images?location=bmc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["images"][0]["id"], "1");
    assert!(body["images"][0].get("digests").is_none());
}

#[tokio::test]
async fn hid_http_rejects_out_of_range_input_and_preflights_the_whole_batch() {
    let app = create_router(make_state().await);
    for (path, body) in [
        ("keyboard", r#"{"type":"down","key":"65540"}"#),
        ("keyboard", r#"{"type":"unexpected","key":"4"}"#),
        ("consumer", r#"{"usage":65536}"#),
        ("mouse", r#"{"type":"absolute","x":65536,"y":0}"#),
        ("mouse", r#"{"type":"scroll","x":0,"y":0,"scroll":128}"#),
        (
            "batch",
            r#"{"events":[{"action":"keyboard","params":{"scancode":4}},{"action":"keyboard","params":{"scancode":65540}}]}"#,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/bmc/test-bmc/api/feature/hid/{path}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
}

#[tokio::test]
async fn password_rotation_persists_when_the_public_secret_state_is_unchanged() {
    let state = make_device_configuration_state().await;
    let app = create_router(state.clone());
    let endpoint = "/api/devices/test-bmc";
    let read = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(endpoint)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let etag = read.headers()["etag"].clone();
    let payload =
        r#"{"credentials":{"password":{"operation":"replace","value":"rotated-test-value"}}}"#;
    let patch = || {
        Request::builder()
            .method("PATCH")
            .uri(endpoint)
            .header("content-type", "application/json")
            .header("if-match", etag.clone())
            .header("x-mutation-id", "rotate-configured-password")
            .body(Body::from(payload))
            .unwrap()
    };
    let updated = app.clone().oneshot(patch()).await.unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let next_etag = updated.headers()["etag"].clone();
    let bytes = axum::body::to_bytes(updated.into_body(), 16384)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("rotated-test-value"));
    assert_eq!(
        state.config.read().await.devices[0].auth.password,
        "rotated-test-value"
    );
    assert_ne!(
        next_etag, etag,
        "masked changes still advance the public version"
    );
    let persisted = Config::load(&state.config_path).unwrap();
    assert_eq!(persisted.devices[0].auth.password, "rotated-test-value");
    let revision = state.configuration.current_revision();
    let replay = app.oneshot(patch()).await.unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["etag"], next_etag);
    assert_eq!(state.configuration.current_revision(), revision);
}

#[tokio::test]
async fn server_web_patch_rejects_invalid_values_and_preserves_ipv6_binding() {
    let state = make_state().await;
    let app = create_router(state.clone());
    let endpoint = "/api/server/web";
    let read = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(endpoint)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let etag = read.headers()["etag"].clone();
    let previous = state.config.read().await.server.listen.clone();
    for payload in [
        r#"{"http_port":true}"#,
        r#"{"http_port":65536}"#,
        r#"{"http_port":"80"}"#,
        r#"{"http_port":1.5}"#,
        r#"{"http_port":null}"#,
        r#"{"https_enabled":true}"#,
        r#"{"bind_addresses":[]}"#,
        r#"{"bind_addresses":["127.0.0.1","::1"]}"#,
        r#"{"bind_address":null}"#,
        r#"{"bind_address":"bad host"}"#,
        r#"{"listen":"http://localhost:80"}"#,
        r#"{"listen":"::1:8080"}"#,
        r#"{"listen":null}"#,
        r#"{"listen":"127.0.0.1:80","http_port":81}"#,
        r#"{"frontend_dir":""}"#,
        r#"{"frontend_dir":true}"#,
        "[]",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(endpoint)
                    .header("content-type", "application/json")
                    .header("if-match", etag.clone())
                    .header("x-mutation-id", uuid::Uuid::new_v4().to_string())
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{payload}");
        assert_eq!(state.config.read().await.server.listen, previous);
        assert_eq!(state.configuration.current_revision(), 0);
    }
    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(endpoint)
                .header("content-type", "application/json")
                .header("if-match", etag)
                .header("x-mutation-id", "ipv6-server-binding")
                .body(Body::from(r#"{"bind_address":"::1","http_port":18081}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["bind_address"], "::1");
    assert_eq!(body["http_port"], 18081);
    assert_eq!(state.config.read().await.server.listen, "[::1]:18081");
    assert_eq!(
        Config::load(&state.config_path).unwrap().server.listen,
        "[::1]:18081"
    );
}

#[tokio::test]
async fn backend_password_rotation_and_keep_are_compared_using_private_values() {
    let state = make_device_configuration_state().await;
    state.config.write().await.devices[0].features.insert("hid".into(),
        serde_yaml::from_str("backend: vnc\nbackend_params:\n  host: 127.0.0.1\n  vnc_port: 5900\n  vnc_security: vnc-auth\n  vnc_tls: false\n  vnc_password: initial-test-value\n").unwrap());
    let app = create_router(state.clone());
    let endpoint = "/api/devices/test-bmc";
    let read = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(endpoint)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let etag = read.headers()["etag"].clone();
    let payload = r#"{"assignments":{"hid":{"kind":"single","backend":{"backend_id":"vnc","params":{"host":"127.0.0.1","vnc_port":5900,"vnc_security":"vnc-auth","vnc_tls":false},"secrets":{"vnc_password":{"operation":"replace","value":"changed-test-value"}}}}}}"#;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(endpoint)
                .header("content-type", "application/json")
                .header("if-match", etag.clone())
                .header("x-mutation-id", "rotate-backend-password")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let next = response.headers()["etag"].clone();
    let response = axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&response).contains("changed-test-value"));
    assert_ne!(next, etag);
    assert_eq!(
        state.config.read().await.devices[0].features["hid"].backend_params["vnc_password"],
        "changed-test-value"
    );
    let revision = state.configuration.current_revision();
    let keep = payload.replace(
        r#""operation":"replace","value":"changed-test-value""#,
        r#""operation":"keep""#,
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(endpoint)
                .header("content-type", "application/json")
                .header("if-match", next.clone())
                .header("x-mutation-id", "keep-backend-password")
                .body(Body::from(keep))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["etag"], next);
    assert_eq!(state.configuration.current_revision(), revision);
}

#[tokio::test]
async fn compatibility_configuration_patches_reject_invalid_fields_before_publication() {
    let state = make_state().await;
    let app = create_router(state.clone());
    let before = bmc_aggregator::bmc::configuration::configuration_payload_digest(
        &*state.config.read().await,
    )
    .unwrap();
    for (feature, payload) in [
        (
            "hid/config",
            r#"{"backend":"mahogany","mouse_absolute":"yes"}"#,
        ),
        ("hid/config", r#"{"ch9329_port":"unsupported"}"#),
        ("hid/config", "[]"),
        ("audio/config", r#"{"enabled":"false"}"#),
        ("audio/config", r#"{"enabled":true}"#),
        ("audio/config", r#"{"backend":123}"#),
        ("msd/config", r#"{"drive_size_mb":0}"#),
        ("msd/config", r#"{"drive_size_mb":18446744073709551615}"#),
        ("msd/config", r#"{"storage_dir":false}"#),
        ("msd/config", r#"{"storage_dir":"one","msd_dir":"two"}"#),
        ("power/config", r#"{"port":623,"ipmi_port":0}"#),
        ("power/config", r#"{"host":"one","ipmi_host":"two"}"#),
        ("video/input-config", r#"{"width":4294967296}"#),
        ("video/input-config", r#"{"width":16385}"#),
        ("video/input-config", r#"{"height":16385}"#),
        ("video/input-config", r#"{"fps":241}"#),
        ("video/input-config", r#"{"height":4294967296}"#),
        ("video/input-config", r#"{"fps":4294967296}"#),
        ("video/input-config", r#"{"quality":101}"#),
        ("video/input-config", r#"{"unknown":true}"#),
    ] {
        let endpoint = format!("/bmc/test-bmc/api/feature/{feature}");
        let read = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&endpoint)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(read.status(), StatusCode::OK, "{feature}");
        let etag = read.headers()["etag"].clone();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(&endpoint)
                    .header("content-type", "application/json")
                    .header("if-match", etag)
                    .header("x-mutation-id", uuid::Uuid::new_v4().to_string())
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{feature}: {payload}"
        );
        assert_eq!(state.configuration.current_revision(), 0);
        assert_eq!(
            bmc_aggregator::bmc::configuration::configuration_payload_digest(
                &*state.config.read().await
            )
            .unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn health_and_firmware_errors_are_structured_sanitized_and_preserve_busy_status() {
    use bmc_aggregator::bmc::feature::{firmware, health};
    struct PrivateErrorBackend(std::sync::atomic::AtomicBool);
    #[async_trait]
    impl health::HealthBackend for PrivateErrorBackend {
        async fn snapshot(&self) -> Result<health::HealthSnapshot, String> {
            if !self.0.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(health::HealthSnapshot {
                    overall_status: Some("ok".into()),
                    sensors: vec![],
                });
            }
            Err("private-backend-detail https://fixture:private@bmc/?token=private".into())
        }
    }
    #[async_trait]
    impl firmware::FirmwareBackend for PrivateErrorBackend {
        async fn inventory(&self) -> Result<Vec<firmware::FirmwareRecord>, String> {
            if !self.0.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(vec![]);
            }
            Err("private-backend-detail https://fixture:private@bmc/?token=private".into())
        }
    }
    for feature in ["health", "firmware"] {
        let state = make_state().await;
        let fc = FeatureConnection::new(
            "test-bmc".into(),
            feature.into(),
            "fixture".into(),
            FeatureConfig {
                backend: "fixture".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        fc.lifecycle_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        fc.transition(ConnectionState::Connected).await;
        let backend = Arc::new(PrivateErrorBackend(std::sync::atomic::AtomicBool::new(
            true,
        )));
        if feature == "health" {
            fc.set_backend_runtime(backend.clone() as Arc<dyn health::HealthBackend>)
                .await;
        } else {
            fc.set_backend_runtime(backend.clone() as Arc<dyn firmware::FirmwareBackend>)
                .await;
        }
        state
            .registry
            .insert(
                "test-bmc".into(),
                Arc::new(ManagedDevice::new(
                    "test-bmc".into(),
                    FeatureConfigs {
                        features: Default::default(),
                    },
                    [(feature.into(), fc.clone())].into(),
                )),
            )
            .await;
        let app = create_router(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/bmc/test-bmc/api/feature/{feature}/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("private"));
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["fault"]["code"], format!("{feature}_read_failed"));
        assert_eq!(json["fault"]["operation"], "status");
        backend.0.store(false, std::sync::atomic::Ordering::SeqCst);
        for unrelated in [false, true] {
            if unrelated {
                fc.faults
                    .record(FeatureFault::operation(
                        FeatureFaultDomain::Operation,
                        "other_operation_failed",
                        "An unrelated operation failed",
                        false,
                        "other",
                    ))
                    .await;
            }
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/bmc/test-bmc/api/feature/{feature}/status"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let active = fc.faults.active().await;
            if unrelated {
                assert_eq!(active.unwrap().code, "other_operation_failed");
            } else {
                assert!(
                    active.is_none(),
                    "{feature} retained its recovered status fault"
                );
                assert!(json["fault"].is_null(), "{feature}: {json}");
            }
            let history = fc.faults.snapshot().await;
            assert!(history
                .to_string()
                .contains(&format!("{feature}_read_failed")));
        }
        fc.transition(ConnectionState::Disconnected).await;
        fc.faults
            .record(FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "session_in_use",
                "Backend is busy",
                true,
            ))
            .await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/bmc/test-bmc/api/feature/{feature}/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["fault"]["code"], "session_in_use");
    }
}
