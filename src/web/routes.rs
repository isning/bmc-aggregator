use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::Json;
use axum::Router;
use tower_http::cors::CorsLayer;

use crate::web::handlers::{self, AppState};

pub fn create_router(state: Arc<AppState>) -> Router {
    let bmc_api = "/bmc/:id/api";

    let r = Router::new()
        // ── Root-level routes ──────────────────────────────
        .route("/", get(dashboard))
        .route("/api/bmc/list", get(handlers::bmc_list_api))
        .route("/bmc/:id/config.js", get(config_js))

        // ── Root-level auth (SPA login page, no /bmc/:id/) ─
        .route("/api/auth/check", get(handlers::auth_check))
        .route("/api/auth/login", post(handlers::auth_login))
        .route("/api/auth/logout", post(handlers::auth_logout))
        .route("/api/auth/change-password", post(handlers::auth_change_password))
        .route("/api/auth/change-username", post(handlers::auth_change_username))

        // ── General BMC info ───────────────────────────────
        .route(&format!("{bmc_api}/info"), get(handlers::device_info))
        .route(&format!("{bmc_api}/features"), get(handlers::device_features))
        .route(&format!("{bmc_api}/snapshot"), get(handlers::snapshot))
        .route(&format!("{bmc_api}/health"), get(handlers::health))

        // ── WebSocket ──────────────────────────────────────
        .route(&format!("{bmc_api}/ws"), get(handlers::ws_event_upgrade))
        .route(&format!("{bmc_api}/ws/hid"), get(handlers::ws_hid_upgrade))

        // ── Config (non-feature) ───────────────────────────
        .route(&format!("{bmc_api}/config"), get(handlers::config_generic))
        .route(&format!("{bmc_api}/config/video"), get(handlers::config_video).patch(handlers::config_video_update))
        .route(&format!("{bmc_api}/config/auth"), get(handlers::config_auth))
        .route(&format!("{bmc_api}/config/web"), get(handlers::config_web))

        // ── Device discovery ───────────────────────────────
        .route(&format!("{bmc_api}/devices"), get(handlers::devices_info))
        .route(&format!("{bmc_api}/devices/usb"), get(handlers::devices_usb))
        .route(&format!("{bmc_api}/devices/atx"), get(handlers::devices_atx))

        // ── Auth ───────────────────────────────────────────
        .route(&format!("{bmc_api}/auth/check"), get(handlers::auth_check))
        .route(&format!("{bmc_api}/auth/login"), post(handlers::auth_login))
        .route(&format!("{bmc_api}/auth/logout"), post(handlers::auth_logout))
        .route(&format!("{bmc_api}/auth/change-password"), post(handlers::auth_change_password))
        .route(&format!("{bmc_api}/auth/change-username"), post(handlers::auth_change_username))

        // ── System / Setup ─────────────────────────────────
        .route(&format!("{bmc_api}/setup/status"), get(handlers::setup_status))
        .route(&format!("{bmc_api}/setup"), get(handlers::setup_status))
        .route(&format!("{bmc_api}/system/info"), get(handlers::system_info))

        // ── WebRTC ─────────────────────────────────────────
        .route(&format!("{bmc_api}/webrtc/offer"), post(handlers::webrtc_offer))
        .route(&format!("{bmc_api}/webrtc/ice"), post(handlers::webrtc_ice))
        .route(&format!("{bmc_api}/webrtc/close"), post(handlers::webrtc_close))
        .route(&format!("{bmc_api}/webrtc/ice-servers"), get(handlers::webrtc_ice_servers))
        .route(&format!("{bmc_api}/pipeline/encoder"), post(handlers::pipeline_set_encoder))

        // ── Extensions ─────────────────────────────────────
        .route(&format!("{bmc_api}/extensions"), get(handlers::extensions_list));

    // ── Feature routes (primary) ───────────────────────────
    let feat = &format!("{bmc_api}/feature");
    let r = mount_video(r, &format!("{feat}/video"));
    let r = mount_hid(r, &format!("{feat}/hid"));
    let r = mount_msd(r, &format!("{feat}/msd"));
    let r = mount_power(r, &format!("{feat}/power"));
    let r = mount_audio(r, &format!("{feat}/audio"));

    r.fallback(fallback_handler)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ── Feature mount helpers ─────────────────────────────────

fn mount_video(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handlers::stream_status))
        .route(&format!("{base}/codecs"), get(handlers::stream_codecs))
        .route(&format!("{base}/mode"), get(handlers::stream_mode).post(handlers::stream_mode_post))
        .route(&format!("{base}/start"), get(handlers::stream_start).post(handlers::stream_start))
        .route(&format!("{base}/stop"), get(handlers::stream_stop).post(handlers::stream_stop))
        .route(&format!("{base}/constraints"), get(handlers::stream_constraints))
        .route(&format!("{base}/bitrate"), get(handlers::stream_bitrate))
        .route(&format!("{base}/devices"), get(handlers::video_devices))
        .route(&format!("{base}/config"), get(handlers::stream_config_get).patch(handlers::stream_config_patch))
        .route(&format!("{base}/encoder/self-check"), get(handlers::encoder_self_check))
        .route(&format!("{base}/http"), get(crate::web::http_stream::http_stream))
}

fn mount_hid(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handlers::hid_status))
        .route(&format!("{base}/otg/self-check"), get(handlers::hid_otg_self_check))
        .route(&format!("{base}/reset"), post(handlers::hid_reset))
        .route(&format!("{base}/config"), get(handlers::config_hid))
}

fn mount_msd(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handlers::msd_status))
        .route(&format!("{base}/connect"), post(handlers::msd_connect))
        .route(&format!("{base}/disconnect"), post(handlers::msd_disconnect))
        .route(&format!("{base}/set_image"), post(handlers::msd_set_image))
        .route(&format!("{base}/images"), get(handlers::msd_images_list).post(handlers::msd_images_upload))
        .route(&format!("{base}/images/:image_id"), delete(handlers::msd_images_delete))
        .route(&format!("{base}/images/download"), post(handlers::msd_images_download))
        .route(&format!("{base}/images/download/cancel"), post(handlers::msd_images_download_cancel))
        .route(&format!("{base}/drive"), get(handlers::msd_drive_info).post(handlers::msd_drive_init).delete(handlers::msd_drive_delete))
        .route(&format!("{base}/drive/init"), post(handlers::msd_drive_init))
        .route(&format!("{base}/drive/files"), get(handlers::msd_drive_files_list).post(handlers::msd_drive_files_upload))
        .route(&format!("{base}/drive/files/*path"), delete(handlers::msd_drive_files_delete))
        .route(&format!("{base}/drive/mkdir/*path"), post(handlers::msd_drive_mkdir))
        .route(&format!("{base}/config"), get(handlers::config_msd).patch(handlers::config_msd_patch))
}

fn mount_power(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handlers::power_status))
        .route(&format!("{base}/control"), post(handlers::power_control))
        .route(&format!("{base}/config"), get(handlers::config_power))
}

fn mount_audio(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handlers::audio_status))
        .route(&format!("{base}/start"), post(handlers::audio_start))
        .route(&format!("{base}/stop"), post(handlers::audio_stop))
        .route(&format!("{base}/quality"), post(handlers::audio_set_quality))
        .route(&format!("{base}/device"), post(handlers::audio_select_device))
        .route(&format!("{base}/config"), get(handlers::config_audio))
}

// ── Dashboard & fallback ──────────────────────────────────

async fn dashboard(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = state.config.read().await;
    let mut html = String::from(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8">
        <meta name="viewport" content="width=device-width,initial-scale=1.0">
        <title>BMC Aggregator</title>
        <style>body{font-family:sans-serif;max-width:800px;margin:40px auto;padding:0 20px}
        h1{color:#333}a{text-decoration:none;color:#1a73e8}a:hover{text-decoration:underline}
        li{margin:8px 0}small{color:#888}</style></head><body>
        <h1>BMC Aggregator</h1><ul>"#,
    );
    for device in &config.devices {
        html.push_str(&format!(
            r#"<li><a href="/bmc/{0}/">{1}</a> <small>({2})</small></li>"#,
            device.id, device.name, device.protocol
        ));
    }
    html.push_str("</ul></body></html>");
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

async fn config_js(Path(bmc_id): Path<String>) -> impl IntoResponse {
    let body = format!(
        "window.__KVM_API_BASE='/bmc/{0}/api';\nwindow.__KVM_ROUTER_BASE='/bmc/{0}/';\n",
        bmc_id
    );
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        body,
    )
}

async fn fallback_handler(
    uri: Uri,
    State(state): State<Arc<AppState>>,
) -> axum::response::Response {
    let path = uri.path();

    // API paths under /bmc/:id/api/ — return 404 JSON
    if path.contains("/api/") {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({
            "status": "error",
            "error": "unknown endpoint"
        }))).into_response();
    }

    let rest = match path.strip_prefix("/bmc/") {
        Some(r) => r,
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };

    let (bmc_id, sub_path) = match rest.split_once('/') {
        Some((id, sp)) => (id, sp),
        None => {
            let redirect_to = format!("/bmc/{}/", rest);
            return (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, redirect_to.as_str())], "").into_response();
        }
    };

    let frontend_dir = state.config.read().await.server.frontend_dir.clone();

    let file_path = frontend_dir.join(sub_path);
    if !sub_path.is_empty() && file_path.exists() && file_path.is_file() {
        return serve_file(&file_path).await;
    }

    // SPA fallback: inject <base> tag
    let index_path = frontend_dir.join("index.html");
    match tokio::fs::read(&index_path).await {
        Ok(data) => {
            let base = format!("/bmc/{}/", bmc_id);
            let injected = match String::from_utf8(data) {
                Ok(s) => s.replace("<head>", &format!("<head><base href=\"{}\">", base)).into_bytes(),
                Err(e) => e.into_bytes(),
            };
            axum::response::Response::builder()
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Body::from(injected))
                .unwrap()
        }
        Err(_) => (StatusCode::NOT_FOUND, "frontend not built").into_response(),
    }
}

async fn serve_file(path: &std::path::Path) -> axum::response::Response {
    match tokio::fs::read(path).await {
        Ok(data) => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let content_type = match ext {
                "js" => "application/javascript",
                "mjs" => "application/javascript",
                "css" => "text/css",
                "html" | "htm" => "text/html; charset=utf-8",
                "png" => "image/png",
                "svg" => "image/svg+xml",
                "ico" | "cur" => "image/x-icon",
                "woff2" => "font/woff2",
                "woff" => "font/woff",
                "ttf" => "font/ttf",
                "json" => "application/json",
                "wasm" => "application/wasm",
                "map" => "application/json",
                _ => "application/octet-stream",
            };
            let is_hashed = matches!(ext, "js" | "css" | "png" | "woff2" | "wasm");

            let mut builder = axum::response::Response::builder()
                .header(header::CONTENT_TYPE, content_type);
            if is_hashed {
                builder = builder
                    .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable");
            }
            builder.body(Body::from(data)).unwrap()
        }
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
