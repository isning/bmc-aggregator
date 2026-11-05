use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Json;
use axum::Router;

use crate::bmc::feature::mount_all_feature_routes;
use crate::web::handlers;
use crate::web::state::AppState;

pub fn create_router(state: Arc<AppState>) -> Router {
    let bmc_api = "/bmc/:id/api";

    let r = Router::new()
        // ── Root-level routes ──────────────────────────────
        .route("/", get(dashboard))
        .route("/api/bmc/list", get(handlers::bmc_list_api))
        .route(
            "/api/configuration/catalog",
            get(crate::bmc::configuration::configuration_catalog_response),
        )
        .route(
            "/api/configuration/ws",
            get(crate::bmc::configuration::configuration_ws_upgrade),
        )
        .route(
            "/api/configuration/options/:resource_id",
            get(crate::bmc::configuration::configuration_options_get),
        )
        .route(
            "/api/configuration/device-drafts/:profile_id",
            post(crate::bmc::configuration::device_creation_draft),
        )
        .route(
            "/api/devices",
            get(handlers::devices_config_list).post(handlers::devices_config_create),
        )
        .route(
            "/api/devices/:device_id",
            get(handlers::devices_config_get)
                .patch(handlers::devices_config_update)
                .delete(handlers::devices_config_delete),
        )
        .route(
            "/api/server/web",
            get(handlers::server_web_config).patch(handlers::server_web_config_update),
        )
        // ── Root-level auth (SPA login page, no /bmc/:id/) ─
        .route("/api/auth/check", get(handlers::auth_check))
        .route("/api/auth/login", post(handlers::auth_login))
        .route("/api/auth/logout", post(handlers::auth_logout))
        .route(
            "/api/auth/change-password",
            post(handlers::auth_change_password),
        )
        .route(
            "/api/auth/change-username",
            post(handlers::auth_change_username),
        )
        .route("/api/setup/status", get(handlers::setup_status))
        .route("/api/setup", post(handlers::setup_init))
        .route("/api/system/info", get(handlers::system_info))
        // ── General BMC info ───────────────────────────────
        .route(&format!("{bmc_api}/info"), get(handlers::device_info))
        .route(
            &format!("{bmc_api}/features"),
            get(handlers::device_features),
        )
        .route(
            &format!("{bmc_api}/features/status"),
            get(handlers::device_features_status),
        )
        .route(&format!("{bmc_api}/snapshot"), get(handlers::snapshot))
        .route(&format!("{bmc_api}/health"), get(handlers::health))
        // ── WebSocket ──────────────────────────────────────
        .route(&format!("{bmc_api}/ws"), get(handlers::ws_event_upgrade))
        // ── Device discovery ───────────────────────────────
        .route(&format!("{bmc_api}/devices"), get(handlers::devices_info));

    // ── Feature routes via mount_all_feature_routes ─────────
    let r = mount_all_feature_routes(r);

    // The SPA and API are deliberately same-origin. Do not enable a broad
    // CORS policy here: most routes mutate physical-machine state.
    r.fallback(fallback_handler)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_device_scope,
        ))
        .with_state(state)
}

async fn require_device_scope(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if let Some(path) = request.uri().path().strip_prefix("/bmc/") {
        if let Some((raw_id, tail)) = path.split_once('/') {
            // Discovery/status endpoints retain their graceful unavailable responses.
            // Persistent storage operations require a registered device before extraction or I/O.
            if tail.starts_with("api/feature/msd/images")
                || tail.starts_with("api/feature/msd/drive")
            {
                let exists = match urlencoding::decode(raw_id) {
                    Ok(id) => state.registry.get_dev(&id).await.is_some(),
                    Err(_) => false,
                };
                if !exists {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({"success": false,"code":"device_not_found"})),
                    )
                        .into_response();
                }
            }
        }
    }
    next.run(request).await
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
            r#"<li><a href="/devices/{0}/console">{1}</a> <small>({2})</small></li>"#,
            html_escape(&device.id),
            html_escape(&device.name),
            html_escape(&device.protocol)
        ));
    }
    html.push_str("</ul></body></html>");
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

async fn fallback_handler(
    uri: Uri,
    State(state): State<Arc<AppState>>,
) -> axum::response::Response {
    let path = uri.path();

    if path.contains("/api/") {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "status": "error",
                "error": "unknown endpoint"
            })),
        )
            .into_response();
    }

    let frontend_dir = state.config.read().await.server.frontend_dir.clone();

    if let Some(root_static_path) = static_path_under(&frontend_dir, path).await {
        if root_static_path.is_file() {
            return serve_file(&root_static_path).await;
        }
    }

    match path.strip_prefix("/bmc/") {
        Some(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
        None if path == "/devices"
            || path.starts_with("/devices/")
            || matches!(path, "/settings" | "/login" | "/setup") =>
        {
            return serve_spa_index(&frontend_dir).await;
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// Resolve a static request only if it remains inside the configured frontend
/// directory after following symlinks.  `Path::join` alone permits both `..`
/// traversal and a symlink contained in the bundle to escape the bundle root.
async fn static_path_under(
    frontend_dir: &std::path::Path,
    request_path: &str,
) -> Option<std::path::PathBuf> {
    use std::path::Component;

    let relative = std::path::Path::new(request_path.trim_start_matches('/'));
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }

    let root = tokio::fs::canonicalize(frontend_dir).await.ok()?;
    let candidate = tokio::fs::canonicalize(root.join(relative)).await.ok()?;
    candidate.starts_with(&root).then_some(candidate)
}

async fn serve_spa_index(frontend_dir: &std::path::Path) -> axum::response::Response {
    let index_path = frontend_dir.join("index.html");
    match tokio::fs::read(&index_path).await {
        Ok(data) => match axum::response::Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache, must-revalidate")
            .body(Body::from(data))
        {
            Ok(response) => response,
            Err(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not build frontend response: {error}"),
            )
                .into_response(),
        },
        Err(_) => (StatusCode::NOT_FOUND, "frontend not built").into_response(),
    }
}

async fn serve_file(path: &std::path::Path) -> axum::response::Response {
    match tokio::fs::read(path).await {
        Ok(data) => {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
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

            let mut builder =
                axum::response::Response::builder().header(header::CONTENT_TYPE, content_type);
            if is_hashed {
                builder =
                    builder.header(header::CACHE_CONTROL, "public, max-age=31536000, immutable");
            }
            match builder.body(Body::from(data)) {
                Ok(response) => response,
                Err(error) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("could not build static response: {error}"),
                )
                    .into_response(),
            }
        }
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{html_escape, serve_spa_index, static_path_under};

    #[test]
    fn dashboard_values_are_html_escaped() {
        assert_eq!(
            html_escape("<script>&\"'"),
            "&lt;script&gt;&amp;&quot;&#x27;"
        );
    }

    #[tokio::test]
    async fn spa_index_must_revalidate_before_loading_hashed_assets() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("index.html"), "<!doctype html>").unwrap();

        let response = serve_spa_index(directory.path()).await;

        assert_eq!(
            response.headers().get(axum::http::header::CACHE_CONTROL),
            Some(&axum::http::HeaderValue::from_static(
                "no-cache, must-revalidate"
            ))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn static_path_stays_inside_frontend_root_after_symlink_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let frontend = directory.path().join("frontend");
        std::fs::create_dir_all(&frontend).unwrap();
        let asset = frontend.join("app.js");
        std::fs::write(&asset, "console.log('ok')").unwrap();
        let secret = directory.path().join("secret.txt");
        std::fs::write(&secret, "private").unwrap();
        std::os::unix::fs::symlink(&secret, frontend.join("escape.txt")).unwrap();

        assert_eq!(
            static_path_under(&frontend, "/app.js").await,
            Some(std::fs::canonicalize(&asset).unwrap())
        );
        assert!(static_path_under(&frontend, "/../secret.txt")
            .await
            .is_none());
        assert!(static_path_under(&frontend, "/escape.txt").await.is_none());
    }
}
