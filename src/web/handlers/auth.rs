use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct AuthCheckResponse {
    pub authenticated: bool,
    pub user: String,
}

#[derive(Serialize)]
pub struct AuthLoginResponse {
    pub success: bool,
    pub message: String,
}

pub async fn auth_check() -> Json<AuthCheckResponse> {
    Json(AuthCheckResponse {
        authenticated: true,
        user: "admin".into(),
    })
}

pub async fn auth_login() -> Json<AuthLoginResponse> {
    Json(AuthLoginResponse {
        success: true,
        message: "ok".into(),
    })
}

pub async fn auth_logout() -> Json<AuthLoginResponse> {
    Json(AuthLoginResponse {
        success: true,
        message: "ok".into(),
    })
}

pub async fn auth_change_password() -> Json<AuthLoginResponse> {
    Json(AuthLoginResponse {
        success: true,
        message: "ok".into(),
    })
}

pub async fn auth_change_username() -> Json<AuthLoginResponse> {
    Json(AuthLoginResponse {
        success: true,
        message: "ok".into(),
    })
}
