use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::web::state::AppState;

#[derive(Deserialize)]
#[allow(dead_code)] // Compatibility endpoint; authentication is intentionally still a stub.
pub struct AuthLoginParams {
    username: Option<String>,
    password: Option<String>,
    keep: Option<bool>,
}

#[derive(Deserialize)]
#[allow(dead_code)] // Compatibility endpoint; authentication is intentionally still a stub.
pub struct ChangePasswordParams {
    current_password: String,
    new_password: String,
}

#[derive(Deserialize)]
#[allow(dead_code)] // Compatibility endpoint; authentication is intentionally still a stub.
pub struct ChangeUsernameParams {
    username: String,
    password: String,
}

pub async fn auth_check(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({"authenticated": true, "user": "admin"}))
}

pub async fn auth_login(
    State(_state): State<Arc<AppState>>,
    Json(_params): Json<AuthLoginParams>,
) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"ok": true, "success": true, "authenticated": true, "token": "", "user": {"username": "admin", "is_admin": true}}),
    )
}

pub async fn auth_logout() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "success": true}))
}

pub async fn auth_change_password(
    State(_state): State<Arc<AppState>>,
    Json(_params): Json<ChangePasswordParams>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "success": true}))
}

pub async fn auth_change_username(
    State(_state): State<Arc<AppState>>,
    Json(_params): Json<ChangeUsernameParams>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "success": true}))
}
