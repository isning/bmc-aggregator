use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::body::Body;
use futures::stream::StreamExt;
use tracing::{info, warn};

use crate::bmc::encoder::codec::detect_frame_format;
use crate::bmc::encoder::registry::EncoderRegistry;
use crate::web::handlers::AppState;

pub async fn http_stream(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let (_guard, session) = match state.orchestrator
        .acquire_wait(&bmc_id, "video").await
    {
        Ok(v) => v,
        Err(e) => {
            warn!("[{}] MJPEG: acquire_wait failed: {}", bmc_id, e);
            return (axum::http::StatusCode::SERVICE_UNAVAILABLE, e).into_response();
        }
    };

    let Some(base) = session.base.as_ref() else {
        warn!("[{}] MJPEG: no video session available", bmc_id);
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "no video".to_string()).into_response();
    };

    let mode = state.orchestrator.device_mode(&bmc_id).await.unwrap_or_else(|| "mjpeg".to_string());
    let content_type = EncoderRegistry::find_codec(&mode)
        .and_then(|c| if c.protocol == "http" { Some(c.mime) } else { None })
        .unwrap_or("image/jpeg").to_string();

    info!("[{}] HTTP stream started: mode={}, Content-Type={}", bmc_id, mode, content_type);

    let stream = base.subscribe_encoded_frames();
    let bmc_id2 = bmc_id.clone();

    let body_stream = stream.enumerate().map(move |(i, frame)| {
        let _ = &_guard;
        if i < 3 {
            let detected = detect_frame_format(&frame)
                .map(|c| c.as_str())
                .unwrap_or("unknown");
            info!("[{}] HTTP stream frame #{}: {} bytes, detected format: {}",
                  bmc_id2, i, frame.len(), detected);
        }
        let mut chunk = Vec::with_capacity(frame.len() + 128);
        chunk.extend_from_slice(b"\r\n--frame\r\nContent-Type: ");
        chunk.extend_from_slice(content_type.as_bytes());
        chunk.extend_from_slice(b"\r\nContent-Length: ");
        chunk.extend_from_slice(frame.len().to_string().as_bytes());
        chunk.extend_from_slice(b"\r\n\r\n");
        chunk.extend_from_slice(&frame);
        Ok::<_, std::convert::Infallible>(chunk)
    });

    let body = Body::from_stream(body_stream);

    let response = axum::response::Response::builder()
        .header(
            header::CONTENT_TYPE,
            "multipart/x-mixed-replace; boundary=frame",
        )
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(body)
        .unwrap();

    response.into_response()
}
