use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use futures::stream::StreamExt;
use tokio::time::timeout;
use tracing::{info, warn};

use crate::bmc::encoder::backend::{FrameEncoder, SoftwareEncoder};
use crate::bmc::feature::connection::{ConnectionState, FeatureGuard};
use crate::bmc::feature::fault::{
    fault_response, record_fault_response, unavailable_fault_response, FeatureFault,
    FeatureFaultDomain,
};
use crate::bmc::feature::video::replay_initial_base_frame;
use crate::bmc::feature::video::runtime;
use crate::web::state::AppState;

fn encode_mjpeg_part(jpeg: &[u8]) -> Vec<u8> {
    // Keep every yielded chunk as one conventional, self-contained multipart
    // part. The browser transport commits at Content-Length rather than
    // relying on native <img> multipart lookahead.
    let mut part = Vec::with_capacity(jpeg.len() + 128);
    part.extend_from_slice(b"--frame\r\nContent-Type: image/jpeg\r\nContent-Length: ");
    part.extend_from_slice(jpeg.len().to_string().as_bytes());
    part.extend_from_slice(b"\r\n\r\n");
    part.extend_from_slice(jpeg);
    part.extend_from_slice(b"\r\n");
    part
}

/// A source lifecycle fault is terminal for a pending HTTP stream request.
/// The lifecycle may already be starting its next reconnect attempt, which
/// leaves the connection state at `Connecting`; that is not evidence that the
/// request can still receive a first frame.
async fn active_connect_fault(fc: &FeatureGuard) -> Option<FeatureFault> {
    // A lifecycle fault describes a failed *connection attempt*. Once the
    // feature is connected, a preceding retryable fault is historical
    // diagnostic state and must not reject a usable browser stream.
    if *fc.fc().state.read().await == ConnectionState::Connected {
        return None;
    }
    let fault = fc.fc().faults.active().await?;
    (fault.domain == FeatureFaultDomain::Lifecycle).then_some(fault)
}

pub async fn http_stream(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> axum::response::Response {
    let dev = match state.registry.get_dev(&bmc_id).await {
        Some(dev) => dev,
        None => {
            return fault_response(
                StatusCode::NOT_FOUND,
                FeatureFault::feature(
                    FeatureFaultDomain::Lifecycle,
                    "device_not_found",
                    "The requested BMC device was not found",
                    false,
                ),
            );
        }
    };

    let fc = match dev.features.get("video") {
        Some(fc) => fc.clone(),
        None => {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
        }
    };

    if !fc
        .lifecycle_started
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return record_fault_response(
            &fc,
            StatusCode::SERVICE_UNAVAILABLE,
            FeatureFault::operation(
                FeatureFaultDomain::Lifecycle,
                "backend_lifecycle_unavailable",
                "The video backend lifecycle is not available",
                true,
                "http_stream",
            ),
        )
        .await;
    }

    let guard = FeatureGuard::new(fc.clone());

    let mut event_rx = fc.event_rx.clone();
    let mut stopped_rx = fc.stopped_rx();
    let deadline = Duration::from_secs(30);
    let start = tokio::time::Instant::now();
    loop {
        if fc.is_stopped() {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
        }
        if let Some(fault) = active_connect_fault(&guard).await {
            warn!("[{bmc_id}] MJPEG: source connection failed: {}", fault.code);
            return fault_response(StatusCode::BAD_GATEWAY, fault);
        }
        if start.elapsed() >= deadline {
            warn!(
                "[{}] MJPEG: FC did not connect within {}s, returning 504",
                bmc_id,
                deadline.as_secs()
            );
            // The lifecycle may already have a precise source/protocol fault
            // while it is retrying. Do not replace that evidence with the
            // less useful HTTP waiting deadline.
            if let Some(fault) = fc.faults.active().await {
                return fault_response(StatusCode::BAD_GATEWAY, fault);
            }
            return record_fault_response(
                &fc,
                StatusCode::GATEWAY_TIMEOUT,
                FeatureFault::operation(
                    FeatureFaultDomain::Lifecycle,
                    "connect_timeout",
                    "The video backend did not connect in time",
                    true,
                    "http_stream",
                ),
            )
            .await;
        }
        let s = fc.state.read().await;
        if *s == ConnectionState::Connected {
            info!(
                "[{}] MJPEG: FC connected after {:?}",
                bmc_id,
                start.elapsed()
            );
            break;
        }
        let current = s.clone();
        drop(s);
        tokio::select! {
            changed = stopped_rx.changed() => {
                if changed.is_err() || *stopped_rx.borrow() {
                    return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
                }
            }
            event = timeout(Duration::from_secs(1), event_rx.changed()) => match event {
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => {
                    return record_fault_response(
                        &fc,
                        StatusCode::SERVICE_UNAVAILABLE,
                        FeatureFault::operation(
                            FeatureFaultDomain::Lifecycle,
                            "event_channel_closed",
                            "The video backend lifecycle stopped unexpectedly",
                            true,
                            "http_stream",
                        ),
                    )
                    .await;
                }
                Err(_) => {
                    warn!(
                        "[{}] MJPEG: still waiting for FC connect, current state={:?} (elapsed {:?})",
                        bmc_id,
                        current,
                        start.elapsed()
                    );
                }
            }
        }
    }

    let vp = match runtime::processor(&fc).await {
        Ok(processor) => processor,
        Err(_) => {
            return record_fault_response(
                &fc,
                StatusCode::SERVICE_UNAVAILABLE,
                FeatureFault::operation(
                    FeatureFaultDomain::Lifecycle,
                    "backend_runtime_unavailable",
                    "The video backend runtime is not available",
                    true,
                    "http_stream",
                ),
            )
            .await
        }
    };
    let mut raw_rx = vp.frame_tx().subscribe();
    let replayed = replay_initial_base_frame(&fc, &vp, &format!("[{}] MJPEG", bmc_id)).await;
    info!(
        "[{}] MJPEG initial base-frame replay requested, cached_replayed={}",
        bmc_id, replayed
    );

    info!("[{}] MJPEG stream started", bmc_id);

    // A slow HTTP peer must not make encoded JPEG frames accumulate without a
    // bound.  Keep at most two pending frames and drop newer work while the
    // response body catches up; the next source frame will refresh it.
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(2);

    let processor = vp.clone();
    tokio::spawn(async move {
        let _guard = guard;
        loop {
            tokio::select! {
            _ = tx.closed() => {
                // The browser removed or replaced its media element. Exit
                // even when the source is silent; waiting only on raw frames
                // would retain the feature lease forever during no-signal.
                break;
            }
            changed = stopped_rx.changed() => {
                if changed.is_err() || *stopped_rx.borrow() {
                    break;
                }
            }
            frame = raw_rx.recv() => match frame {
                Ok(raw) => {
                    let encode_start = std::time::Instant::now();
                    let work = crate::bmc::cpu::run(move || {
                        let (width, height) = raw.image_dimensions()?;
                        let pixels = raw.to_rgba()?;
                        SoftwareEncoder.encode(&pixels, width, height, 80).map_err(|error| error.to_string())
                    });
                    let result = tokio::select! {
                        biased;
                        _ = tx.closed() => break,
                        _ = stopped_rx.changed() => break,
                        result = work => result,
                    };
                    let encoded = match result {
                        Ok(Ok(data)) => {
                            processor.record_mjpeg_encode_time(encode_start.elapsed());
                            data
                        }
                        _ => {
                            processor
                                .record_fault(FeatureFault::child(
                                    FeatureFaultDomain::Encoder,
                                    "encode_failed",
                                    "MJPEG frame encoding failed",
                                    true,
                                    "mjpeg",
                                    bmc_id.clone(),
                                ))
                                .await;
                            continue;
                        }
                    };
                    let chunk = encode_mjpeg_part(&encoded);
                    match tx.try_send(chunk) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
            }
        }
    });

    let body_stream =
        tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, std::convert::Infallible>);

    let body = Body::from_stream(body_stream);

    match axum::response::Response::builder()
        .header(
            header::CONTENT_TYPE,
            "multipart/x-mixed-replace; boundary=frame",
        )
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(body)
    {
        Ok(response) => response.into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not build MJPEG response: {error}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{active_connect_fault, encode_mjpeg_part};
    use crate::bmc::feature::config::{BackendParams, FeatureConfig, FeatureParams};
    use crate::bmc::feature::connection::{ConnectionState, FeatureConnection, FeatureGuard};
    use crate::bmc::feature::fault::{FeatureFault, FeatureFaultDomain};

    #[test]
    fn mjpeg_part_is_a_self_contained_standard_part() {
        let jpeg = [0xff, 0xd8, 0xff, 0xd9];
        let part = encode_mjpeg_part(&jpeg);

        assert!(
            part.starts_with(b"--frame\r\nContent-Type: image/jpeg\r\nContent-Length: 4\r\n\r\n")
        );
        assert!(part.ends_with(&[0xff, 0xd8, 0xff, 0xd9, b'\r', b'\n']));
    }

    #[tokio::test]
    async fn lifecycle_fault_ends_mjpeg_wait_while_reconnect_is_connecting() {
        let fc = FeatureConnection::new(
            "device".to_string(),
            "video".to_string(),
            "test".to_string(),
            FeatureConfig {
                backend: "test".to_string(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        );
        fc.transition(ConnectionState::Connecting).await;
        fc.faults
            .record(FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "apcp_rejected",
                "The BMC closed the AVCT APCP setup connection",
                true,
            ))
            .await;

        let guard = FeatureGuard::new(Arc::clone(&fc));
        assert_eq!(
            active_connect_fault(&guard)
                .await
                .expect("active fault")
                .code,
            "apcp_rejected"
        );
    }

    #[tokio::test]
    async fn connected_source_does_not_treat_a_stale_lifecycle_fault_as_connect_failure() {
        let fc = FeatureConnection::new(
            "device".to_string(),
            "video".to_string(),
            "test".to_string(),
            FeatureConfig {
                backend: "test".to_string(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        );
        fc.transition(ConnectionState::Connected).await;
        fc.faults
            .record(FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "session_in_use",
                "The previous connection attempt lost the session race",
                true,
            ))
            .await;

        let guard = FeatureGuard::new(Arc::clone(&fc));
        assert!(active_connect_fault(&guard).await.is_none());
    }
}
