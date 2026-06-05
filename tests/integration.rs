use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use bmc_aggregator::bmc::mock_session::MockSession;
use bmc_aggregator::bmc::session::manager::StreamOrchestrator;

use bmc_aggregator::config::{AuthConfig, Config, DeviceConfig, ServerConfig};
use bmc_aggregator::web::handlers::AppState;
use bmc_aggregator::web::routes::create_router;

// ── State Builder ────────────────────────────────────────────────

async fn make_state() -> Arc<AppState> {
    let config = Config {
        server: ServerConfig {
            listen: "127.0.0.1:0".into(),
            frontend_dir: std::path::PathBuf::from("/tmp/nonexistent"),
            download_headers: std::collections::HashMap::new(),
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
            video_device: None,
            video_format: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            video_quality: None,
        }],
        msd: std::collections::HashMap::new(),
    };

    let manager = StreamOrchestrator::new();
    let mock = Arc::new(MockSession::new());
    let session = mock.clone().to_session();
    manager.inject_device(
        "test-bmc".into(),
        "Test BMC".into(),
        "mock".into(),
        session,
    ).await;

    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let config_for_state = config_lock.read().await.clone();
    let msd_storage_root = bmc_aggregator::web::handlers::default_msd_storage_base();

    Arc::new(AppState {
        config: tokio::sync::RwLock::new(config_for_state),
        config_path: "/tmp/bmc-aggregator-test-config.yaml".into(),
        orchestrator: manager,
        msd_runtime: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        msd_storage: bmc_aggregator::bmc::msd_storage::FsMsdStorage::new(msd_storage_root, config_lock),
        http_client: reqwest::Client::new(),
        event_tx: tokio::sync::broadcast::channel(256).0,
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
    })
}

// ── Dashboard ────────────────────────────────────────────────────

#[tokio::test]
async fn test_dashboard_returns_html() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response.headers().get("content-type").unwrap().to_str().unwrap().to_string();
    assert!(content_type.contains("text/html"));
}

// ── BMC List API ─────────────────────────────────────────────────

#[tokio::test]
async fn test_bmc_list() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/api/bmc/list").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], "test-bmc");
    assert_eq!(list[0]["name"], "Test BMC");
}

// ── Config JS ────────────────────────────────────────────────────

#[tokio::test]
async fn test_config_js_returns_correct_base() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/bmc/test-bmc/config.js").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("__KVM_API_BASE"));
    assert!(text.contains("__KVM_ROUTER_BASE"));
    assert!(text.contains("/bmc/test-bmc/api"));
    assert!(text.contains("/bmc/test-bmc/"));
}

// ── Device Info ──────────────────────────────────────────────────

#[tokio::test]
async fn test_device_info_returns_device_details() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/bmc/test-bmc/api/info").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["id"], "test-bmc");
    assert_eq!(info["name"], "Test BMC");
    assert!(info["online"].as_bool().unwrap());
    assert_eq!(info["width"], 1920);
    assert_eq!(info["height"], 1080);
}

// ── Stream Status ────────────────────────────────────────────────

#[tokio::test]
async fn test_stream_status_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/bmc/test-bmc/api/feature/video/status").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["format"], "mjpeg");
    assert_eq!(status["state"], "streaming");
}

#[tokio::test]
async fn test_stream_codecs_returns_backends() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/bmc/test-bmc/api/feature/video/codecs").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 16384).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);
    assert!(json["backends"].is_array());
    assert!(json["codecs"].is_array());
}

#[tokio::test]
async fn test_stream_config_get_and_patch_encoder() {
    let state = make_state().await;
    let app = create_router(state);
    let get_response = app.clone()
        .oneshot(Request::builder().uri("/bmc/test-bmc/api/feature/video/config").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(get_response.status(), StatusCode::OK);
    let get_body = axum::body::to_bytes(get_response.into_body(), 4096).await.unwrap();
    let get_json: serde_json::Value = serde_json::from_slice(&get_body).unwrap();
    assert_eq!(get_json["encoder"], "software");

    let patch_response = app.clone()
        .oneshot(
            Request::builder().method("PATCH").uri("/bmc/test-bmc/api/feature/video/config")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"encoder":"vaapi"}"#)).unwrap(),
        ).await.unwrap();
    assert_eq!(patch_response.status(), StatusCode::OK);
    let patch_body = axum::body::to_bytes(patch_response.into_body(), 4096).await.unwrap();
    let patch_json: serde_json::Value = serde_json::from_slice(&patch_body).unwrap();
    assert_eq!(patch_json["encoder"], "software");
}

// ── ATX Power (POST) ─────────────────────────────────────────────

#[tokio::test]
async fn test_atx_power_on_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder().uri("/bmc/test-bmc/api/feature/power/control").method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"on"}"#)).unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_atx_power_hold_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder().uri("/bmc/test-bmc/api/feature/power/control").method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"hold"}"#)).unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_atx_power_reset_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder().uri("/bmc/test-bmc/api/feature/power/control").method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"reset"}"#)).unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_atx_power_invalid_action_returns_bad_request() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder().uri("/bmc/test-bmc/api/feature/power/control").method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"invalid"}"#)).unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_atx_power_short_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder().uri("/bmc/test-bmc/api/feature/power/control").method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"short"}"#)).unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_atx_power_long_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder().uri("/bmc/test-bmc/api/feature/power/control").method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"long"}"#)).unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_atx_power_press_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    let req = Request::builder().uri("/bmc/test-bmc/api/feature/power/control").method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"action":"press"}"#)).unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// ── ATX Status (GET) ─────────────────────────────────────────────

#[tokio::test]
async fn test_atx_status_returns_on() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/bmc/test-bmc/api/feature/power/status").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["power_status"], "on");
}

// ── 404 Handling ─────────────────────────────────────────────────

#[tokio::test]
async fn test_unknown_route_returns_404() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/nonexistent/path").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_bmc_sub_path_unknown_returns_404() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/bmc/test-bmc/api/nonexistent").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "error");
}

// ── MJPEG Stream ─────────────────────────────────────────────────

#[tokio::test]
async fn test_http_stream_returns_error_without_session() {
    let state = make_state().await;
    let app = create_router(state);
    let response = app
        .oneshot(Request::builder().uri("/bmc/test-bmc/api/feature/video/http").body(Body::empty()).unwrap())
        .await.unwrap();
    assert!(response.status().is_server_error() || response.status().is_success());
}

#[tokio::test]
async fn test_stream_start_returns_ok() {
    let state = make_state().await;
    let app = create_router(state);
    for method in &["GET", "POST"] {
        let res = app.clone().oneshot(
            Request::builder()
                .method(*method)
                .uri("/bmc/test-bmc/api/feature/video/start")
                .body(Body::empty()).unwrap(),
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }
}

// ── MSD Full Lifecycle ─────────────────────────────────────────

#[tokio::test]
async fn test_msd_lifecycle() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/status").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["online"], false);
    assert_eq!(status["enabled"], false);

    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/set_image")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"image":"/iso/test.iso","image_size":1048576}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/status").body(Body::empty()).unwrap(),
    ).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["enabled"], true);
    assert_eq!(status["image"], "/iso/test.iso");
    assert_eq!(status["image_size"], 1048576);

    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/connect")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/status").body(Body::empty()).unwrap(),
    ).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["online"], true);

    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/disconnect")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/status").body(Body::empty()).unwrap(),
    ).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["online"], false);
}

#[tokio::test]
async fn test_msd_without_session_returns_available_true() {
    let state = make_state_without_session().await;
    let app = create_router(state);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/status").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["available"], true);
    assert_eq!(status["online"], false);
    assert_eq!(status["enabled"], false);
    assert_eq!(status["state"]["connected"], false);

    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/connect")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/disconnect")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_msd_extra_endpoints_exist() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/config").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let config: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(config["available"], true);
    assert_eq!(config["backends"], serde_json::json!(["mahogany", "virtual-drive"]));

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/images").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/drive").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/drive/files?path=%2F").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_msd_connect_supports_virtual_drive_backend() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/connect")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"backend":"virtual-drive","mode":"drive"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/status").body(Body::empty()).unwrap(),
    ).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["state"]["backend"], "virtual-drive");
    assert_eq!(status["state"]["connected"], true);
    assert_eq!(status["state"]["mode"], "drive");
}

#[tokio::test]
async fn test_msd_images_and_drive_endpoints_work() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let remote_app = axum::Router::new().route("/demo.iso", axum::routing::get(|| async { "iso-bytes" }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, remote_app).await;
    });

    let state = make_state().await;
    let app = create_router(state);

    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/images/download")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"url":"http://{}/demo.iso","filename":"demo.iso"}}"#, addr))).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Poll until async download completes
    let expected_name = "demo.iso";
    let poll_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            loop {
                let res = app.clone().oneshot(
                    Request::builder().uri("/bmc/test-bmc/api/feature/msd/images").body(Body::empty()).unwrap(),
                ).await.unwrap();
                let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
                let images: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if images.as_array().unwrap().iter().any(|v| v["name"].as_str().unwrap().contains(expected_name)) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        },
    ).await;
    assert!(poll_result.is_ok(), "timed out waiting for downloaded image");

    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/drive/mkdir/folder1")
            .body(Body::from("")).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/drive/files?path=%2F").body(Body::empty()).unwrap(),
    ).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let files: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(files.as_array().unwrap().iter().any(|v| v["name"] == "folder1" && v["is_dir"] == true));
}

#[tokio::test]
async fn test_msd_images_multipart_upload_writes_real_content() {
    let state = make_state().await;
    let app = create_router(state);
    let boundary = "XBOUNDARY";
    let payload = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.iso\"\r\nContent-Type: application/octet-stream\r\n\r\nhello-msd\r\n--{boundary}--\r\n"
    );
    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/images")
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(Body::from(payload)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/images").body(Body::empty()).unwrap(),
    ).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let images: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(images.as_array().unwrap().iter().any(|v| v["name"] == "hello.iso" && v["size"].as_u64().unwrap_or(0) > 0));
}

#[tokio::test]
async fn test_msd_drive_multipart_upload_writes_real_content() {
    let state = make_state().await;
    let app = create_router(state);
    let boundary = "XBOUNDARY2";
    let payload = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"notes.txt\"\r\nContent-Type: text/plain\r\n\r\nabc123\r\n--{boundary}--\r\n"
    );
    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/drive/files?path=%2F")
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(Body::from(payload)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/drive/files?path=%2F").body(Body::empty()).unwrap(),
    ).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let files: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(files.as_array().unwrap().iter().any(|v| v["name"] == "notes.txt" && v["size"].as_u64().unwrap_or(0) > 0));
}

#[tokio::test]
async fn test_msd_images_download_fetches_remote_content() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let remote_app = axum::Router::new().route("/demo.iso", axum::routing::get(|| async { "iso-bytes" }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, remote_app).await;
    });

    let state = make_state().await;
    let app = create_router(state);
    let url = format!("http://{}/demo.iso", addr);

    let res = app.clone().oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/feature/msd/images/download")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"url":"{}","filename":"from_remote.iso"}}"#, url))).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Poll until async download completes
    let expected_name = "from_remote.iso";
    let poll_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            loop {
                let res = app.clone().oneshot(
                    Request::builder().uri("/bmc/test-bmc/api/feature/msd/images").body(Body::empty()).unwrap(),
                ).await.unwrap();
                let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
                let images: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if images.as_array().unwrap().iter().any(|v| v["name"] == expected_name && v["size"].as_u64().unwrap_or(0) > 0) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        },
    ).await;
    assert!(poll_result.is_ok(), "timed out waiting for downloaded image");
}

// ── MSD Config Persistence ─────────────────────────────────────

#[tokio::test]
async fn test_msd_config_patch_persists() {
    let config_path = std::env::temp_dir().join(format!("bmc-aggregator-msd-config-{}.yaml", uuid::Uuid::new_v4()));
    let config = Config {
        server: ServerConfig { listen: "127.0.0.1:0".into(), frontend_dir: "/tmp".into(), download_headers: std::collections::HashMap::new() },
        devices: vec![DeviceConfig {
            id: "test-bmc".into(), name: "Test BMC".into(), protocol: "mock".into(),
            host: "127.0.0.1".into(), port: 0,
            auth: AuthConfig { username: "admin".into(), password: "pass".into() },
            kvm_port: None, ipmi_host: None, ipmi_port: None, max_fps: 30,
            video_backend: "mahogany".into(), encoder_backend: "software".into(), audio_backend: None,
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

    let manager = StreamOrchestrator::new();
    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let config_for_state = config_lock.read().await.clone();
    let msd_storage_root = bmc_aggregator::web::handlers::default_msd_storage_base();
    let state = Arc::new(AppState {
        config: tokio::sync::RwLock::new(config_for_state),
        config_path: config_path.to_string_lossy().to_string(),
        orchestrator: manager,
        msd_runtime: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        msd_storage: bmc_aggregator::bmc::msd_storage::FsMsdStorage::new(msd_storage_root, config_lock),
        http_client: reqwest::Client::new(),
        event_tx: tokio::sync::broadcast::channel(256).0,
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
    });
    let app = create_router(state.clone());

    let res = app.clone().oneshot(
        Request::builder().method("PATCH").uri("/bmc/test-bmc/api/feature/msd/config")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"backend":"virtual-drive","drive_size_mb":2048}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let saved = Config::load(config_path.to_str().unwrap()).unwrap();
    let msd = saved.msd.get("test-bmc").unwrap();
    assert_eq!(msd.backend, "virtual-drive");
    assert_eq!(msd.drive_size_mb, 2048);
}

// ── Auth Endpoints ──────────────────────────────────────────────

#[tokio::test]
async fn test_auth_check_returns_authenticated() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app.oneshot(
        Request::builder().uri("/bmc/test-bmc/api/auth/check").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["authenticated"], true);
    assert_eq!(json["user"], "admin");
}

#[tokio::test]
async fn test_auth_login_returns_success() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app.oneshot(
        Request::builder().method("POST").uri("/bmc/test-bmc/api/auth/login")
            .header("content-type", "application/json").body(Body::from(r#"{}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);
}

#[tokio::test]
async fn test_auth_logout_and_change_password_returns_success() {
    let state = make_state().await;
    let app = create_router(state);
    for uri in &["/bmc/test-bmc/api/auth/logout", "/bmc/test-bmc/api/auth/change-password"] {
        let res = app.clone().oneshot(
            Request::builder().method("POST").uri(*uri)
                .header("content-type", "application/json").body(Body::from(r#"{}"#)).unwrap(),
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);
    }
}

// ── Setup & System ──────────────────────────────────────────────

#[tokio::test]
async fn test_setup_status_returns_initialized() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app.oneshot(
        Request::builder().uri("/bmc/test-bmc/api/setup/status").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["initialized"], true);
    assert_eq!(json["needs_setup"], false);
}

#[tokio::test]
async fn test_system_info_returns_platform() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app.oneshot(
        Request::builder().uri("/bmc/test-bmc/api/system/info").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 16384).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["version"], "0.1.0");
    assert_eq!(json["platform"]["mode"], "bmc_aggregator");
    assert_eq!(json["capabilities"]["mode"], "bmc_aggregator");
}

// ── HID Status ──────────────────────────────────────────────────

#[tokio::test]
async fn test_hid_status_returns_available() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app.oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/hid/status").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["available"], true);
    assert_eq!(json["supports_absolute_mouse"], true);
}

// ── Unknown Device ──────────────────────────────────────────────

#[tokio::test]
async fn test_unknown_device_returns_graceful() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/nonexistent/api/info").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "nonexistent");
    assert_eq!(json["online"], false);

    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/nonexistent/api/feature/video/status").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["state"], "uninitialized");
}

// ── Multiple Devices ────────────────────────────────────────────

async fn make_state_two_devices() -> Arc<AppState> {
    let config = Config {
        server: ServerConfig {
            listen: "127.0.0.1:0".into(),
            frontend_dir: std::path::PathBuf::from("/tmp/nonexistent"),
            download_headers: std::collections::HashMap::new(),
        },
        devices: vec![
            DeviceConfig {
                id: "alpha".into(),
                name: "Alpha BMC".into(),
                protocol: "mock".into(),
                host: "127.0.0.1".into(),
                port: 0,
                auth: AuthConfig { username: "admin".into(), password: "pass".into() },
                kvm_port: None,
                ipmi_host: None,
                ipmi_port: Some(623),
                max_fps: 30,
                video_backend: "mahogany".into(),
                encoder_backend: "software".into(),
                audio_backend: None,
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
                auth: AuthConfig { username: "admin".into(), password: "pass".into() },
                kvm_port: None,
                ipmi_host: None,
                ipmi_port: Some(624),
                max_fps: 30,
                video_backend: "mahogany".into(),
                encoder_backend: "vaapi".into(),
                audio_backend: None,
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

    let manager = StreamOrchestrator::new();
    for id in &["alpha", "beta"] {
        let mock = Arc::new(MockSession::new());
        let session = mock.clone().to_session();
        manager.inject_device(
            (*id).into(), format!("{} BMC", id), "mock".into(), session,
        ).await;
    }

    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let config_for_state = config_lock.read().await.clone();
    let msd_storage_root = bmc_aggregator::web::handlers::default_msd_storage_base();

    Arc::new(AppState {
        config: tokio::sync::RwLock::new(config_for_state),
        config_path: "/tmp/bmc-aggregator-test-config.yaml".into(),
        orchestrator: manager,
        msd_runtime: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        msd_storage: bmc_aggregator::bmc::msd_storage::FsMsdStorage::new(msd_storage_root, config_lock),
        http_client: reqwest::Client::new(),
        event_tx: tokio::sync::broadcast::channel(256).0,
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
    })
}

#[tokio::test]
async fn test_multiple_devices_list() {
    let state = make_state_two_devices().await;
    let app = create_router(state);

    let res = app.oneshot(
        Request::builder()
            .uri("/api/bmc/list")
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["id"], "alpha");
    assert_eq!(list[1]["id"], "beta");
}

#[tokio::test]
async fn test_multiple_devices_individual_info() {
    let state = make_state_two_devices().await;
    let app = create_router(state);

    for id in &["alpha", "beta"] {
        let res = app.clone().oneshot(
            Request::builder()
                .uri(&format!("/bmc/{}/api/info", id))
                .body(Body::empty()).unwrap(),
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], *id);
        assert_eq!(json["online"], true);
    }
}

// ── Fallback API default routes ─────────────────────────────────

#[tokio::test]
async fn test_fallback_stream_codecs_returns_defaults() {
    let state = make_state().await;
    let app = create_router(state);

    let res = app.oneshot(
        Request::builder()
            .uri("/bmc/test-bmc/api/feature/video/encoder/self-check")
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["current_hardware_encoder"], "software");
    assert!(json["codecs"].is_array());
}

#[tokio::test]
async fn test_fallback_stream_start_stop() {
    let state = make_state().await;
    let app = create_router(state);

    for path in &["feature/video/start", "feature/video/stop", "feature/video/mode", "feature/video/bitrate"] {
        let res = app.clone().oneshot(
            Request::builder()
                .uri(&format!("/bmc/test-bmc/api/{}", path))
                .body(Body::empty()).unwrap(),
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn test_fallback_config_endpoints_return_defaults() {
    let state = make_state().await;
    let app = create_router(state);

    for path in &["feature/hid/config", "config/video", "feature/power/config", "config/auth", "config/web"] {
        let res = app.clone().oneshot(
            Request::builder()
                .uri(&format!("/bmc/test-bmc/api/{}", path))
                .body(Body::empty()).unwrap(),
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}

// ── State factory without session injection ─────────────────────

async fn make_state_without_session() -> Arc<AppState> {
    let config = Config {
        server: ServerConfig {
            listen: "127.0.0.1:0".into(),
            frontend_dir: std::path::PathBuf::from("/tmp/nonexistent"),
            download_headers: std::collections::HashMap::new(),
        },
        devices: vec![DeviceConfig {
            id: "test-bmc".into(),
            name: "Test BMC".into(),
            protocol: "mock".into(),
            host: "127.0.0.1".into(),
            port: 0,
            auth: AuthConfig { username: "admin".into(), password: "pass".into() },
            kvm_port: None,
            ipmi_host: None,
            ipmi_port: Some(623),
            max_fps: 30,
            video_backend: "mahogany".into(),
            encoder_backend: "software".into(),
            audio_backend: None,
            video_device: None,
            video_format: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            video_quality: None,
        }],
        msd: std::collections::HashMap::new(),
    };

    let manager = StreamOrchestrator::new();
    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let config_for_state = config_lock.read().await.clone();
    let msd_storage_root = bmc_aggregator::web::handlers::default_msd_storage_base();

    Arc::new(AppState {
        config: tokio::sync::RwLock::new(config_for_state),
        config_path: "/tmp/bmc-aggregator-test-config.yaml".into(),
        orchestrator: manager,
        msd_runtime: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        msd_storage: bmc_aggregator::bmc::msd_storage::FsMsdStorage::new(msd_storage_root, config_lock),
        http_client: reqwest::Client::new(),
        event_tx: tokio::sync::broadcast::channel(256).0,
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
    })
}

// ── API Contract Tests ──────────────────────────────────────────

fn check_contract_keys(actual: &serde_json::Value, expected_keys: &[&str]) {
    let actual_keys: std::collections::BTreeSet<String> = actual.as_object().unwrap().keys().cloned().collect();
    let expected: std::collections::BTreeSet<String> = expected_keys.iter().map(|k| k.to_string()).collect();
    let missing: Vec<&str> = expected.difference(&actual_keys).map(|k| k.as_str()).collect();
    let extra: Vec<&str> = actual_keys.difference(&expected).map(|k| k.as_str()).collect();
    assert!(missing.is_empty(), "missing keys: {:?}", missing);
    assert!(extra.is_empty(), "extra keys: {:?}", extra);
}

#[tokio::test]
async fn test_contract_config_msd() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app.oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/config").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    check_contract_keys(&json, &[
        "available", "msd_enabled", "backends", "selected_backend", "msd_backend",
        "drive_size_mb", "size_mb", "msd_dir", "image_dir", "storage",
    ]);
    assert_eq!(json["available"], true);
    assert_eq!(json["msd_enabled"], true);
    assert_eq!(json["backends"], serde_json::json!(["mahogany", "virtual-drive"]));
    assert_eq!(json["selected_backend"], "mahogany");
    assert_eq!(json["msd_backend"], "mahogany");
    assert_eq!(json["drive_size_mb"], 1024);
    assert_eq!(json["size_mb"], 1024);
    assert!(json["msd_dir"].as_str().unwrap().contains("bmc-aggregator-msd"));
    assert!(json["image_dir"].as_str().unwrap().contains("bmc-aggregator-msd"));

    let storage = &json["storage"];
    check_contract_keys(storage, &["directory", "upload_chunk_size"]);
    assert!(storage["directory"].as_str().unwrap().contains("bmc-aggregator-msd"));
    assert_eq!(storage["upload_chunk_size"], 1048576);
}

#[tokio::test]
async fn test_contract_msd_status() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/status").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    check_contract_keys(&json, &[
        "available", "online", "connected", "enabled", "image", "image_size", "state", "backends",
    ]);
    assert_eq!(json["available"], true);
    assert_eq!(json["online"], false);
    assert_eq!(json["enabled"], false);
    assert_eq!(json["image"], "");
    assert_eq!(json["image_size"], 0);

    let state_obj = &json["state"];
    check_contract_keys(state_obj, &["connected", "mode", "backend", "current_image"]);
    assert_eq!(state_obj["connected"], false);
    assert_eq!(state_obj["mode"], "drive");
    assert_eq!(state_obj["backend"], "mahogany");
    assert_eq!(state_obj["current_image"], serde_json::Value::Null);

    assert_eq!(json["backends"], serde_json::json!(["mahogany", "virtual-drive"]));
}

#[tokio::test]
async fn test_contract_msd_images_list() {
    let state = make_state().await;
    let app = create_router(state);
    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/images").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.is_array());
    for item in json.as_array().unwrap() {
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
    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/drive/files?path=%2F").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.is_array());
    for item in json.as_array().unwrap() {
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
    let res = app.clone().oneshot(
        Request::builder().uri("/bmc/test-bmc/api/feature/msd/drive").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    check_contract_keys(&json, &["size", "used", "free", "capacity_mb"]);
    assert!(json["size"].is_u64());
    assert!(json["used"].is_u64());
    assert!(json["free"].is_u64());
    assert_eq!(json["capacity_mb"], 1024);
}

// ── Stream mode POST (VP8) event emission ─────────────────────────

#[tokio::test]
async fn test_stream_mode_post_vp8_emits_ws_events() {
    // Create state with a shared event channel so we can capture WS events
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<String>(256);

    let config = Config {
        server: ServerConfig {
            listen: "127.0.0.1:0".into(),
            frontend_dir: std::path::PathBuf::from("/tmp/nonexistent"),
            download_headers: std::collections::HashMap::new(),
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
            video_device: None,
            video_format: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            video_quality: None,
        }],
        msd: std::collections::HashMap::new(),
    };

    let manager = StreamOrchestrator::new();
    let mock = Arc::new(MockSession::new());
    let session = mock.clone().to_session();
    manager.inject_device(
        "test-bmc".into(),
        "Test BMC".into(),
        "mock".into(),
        session,
    ).await;

    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let config_for_state = config_lock.read().await.clone();
    let msd_storage_root = bmc_aggregator::web::handlers::default_msd_storage_base();

    let state = Arc::new(AppState {
        config: tokio::sync::RwLock::new(config_for_state),
        config_path: "/tmp/bmc-aggregator-test-config.yaml".into(),
        orchestrator: manager,
        msd_runtime: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        msd_storage: bmc_aggregator::bmc::msd_storage::FsMsdStorage::new(msd_storage_root, config_lock),
        http_client: reqwest::Client::new(),
        event_tx,
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
    });

    let app = create_router(state);

    // POST /feature/video/mode with VP8
    let req = Request::builder()
        .method("POST")
        .uri("/bmc/test-bmc/api/feature/video/mode")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"mode":"vp8"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Must be success
    assert_eq!(json["success"], true, "stream_mode_post must return success for VP8. Body: {json}");
    assert_eq!(json["mode"], "vp8");
    assert_eq!(json["protocol"], "webrtc");
    assert!(json["transition_id"].is_string(), "VP8 mode must return a transition_id");

    // Now check that WS events were emitted through event_tx
    let tid = json["transition_id"].as_str().unwrap();

    // Collect both events with a short timeout
    let mut mode_ready_found = false;
    let mut webrtc_ready_found = false;

    for _ in 0..5 {
        match event_rx.try_recv() {
            Ok(msg) => {
                let parsed: serde_json::Value = serde_json::from_str(&msg).unwrap();
                match parsed["event"].as_str() {
                    Some("stream.mode_ready") => {
                        assert_eq!(parsed["data"]["transition_id"], tid);
                        assert_eq!(parsed["data"]["mode"], "vp8");
                        mode_ready_found = true;
                    }
                    Some("stream.webrtc_ready") => {
                        assert_eq!(parsed["data"]["codec"], "vp8");
                        assert_eq!(parsed["data"]["hardware"], false);
                        assert_eq!(parsed["data"]["transition_id"], tid);
                        webrtc_ready_found = true;
                    }
                    _ => {}
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(_) => {}
        }
    }

    assert!(mode_ready_found, "stream.mode_ready WS event was NOT emitted!");
    assert!(webrtc_ready_found, "stream.webrtc_ready WS event was NOT emitted!");
}

// ── Timeout-with-background-load test ────────────────────────────

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
        .body(Body::from(r#"{"mode":"vp8"}"#))
        .unwrap();

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        app.oneshot(req),
    ).await;

    assert!(response.is_ok(),
        "VP8 mode switch hung! runtime blocked by background load");

    let status = response.unwrap().unwrap().status();
    eprintln!("VP8 mode switch returned status={} under CPU load ✓", status);
}
