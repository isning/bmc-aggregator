use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::bmc::encoder::registry::{
    codec_available_for_webrtc, probe_webrtc_codec_for_backend, EncoderRegistry,
};
use crate::bmc::feature::connection::{ConnectionState, FeatureGuard};
use crate::bmc::feature::fault::{
    acquisition_fault_response, fault_response, record_fault_response, unavailable_fault_response,
    FeatureFault, FeatureFaultDomain,
};
use crate::bmc::feature::video::runtime;
use crate::bmc::feature::{acquire_connected_feature, feature_connect_status, retry_feature};
use crate::bmc::feature::{build_feature_instances_status, build_feature_status};
use crate::web::state::AppState;

fn transports_for_protocol(protocol: &str) -> Vec<&'static str> {
    match protocol {
        "http" => vec!["http"],
        "webrtc" => vec!["webrtc", "webtransport"],
        _ => vec![],
    }
}

fn device_not_found() -> Response {
    fault_response(
        StatusCode::NOT_FOUND,
        FeatureFault::feature(
            FeatureFaultDomain::Lifecycle,
            "device_not_found",
            "The requested BMC device was not found",
            false,
        ),
    )
}

pub(super) async fn control_processor(
    state: &Arc<AppState>,
    device_id: &str,
) -> Result<Arc<super::processor::VideoProcessor>, Response> {
    let device = state
        .registry
        .get_dev(device_id)
        .await
        .ok_or_else(device_not_found)?;
    let fc = device
        .features
        .get("video")
        .ok_or_else(|| unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video"))?;
    if fc.is_stopped() {
        return Err(unavailable_fault_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "video",
        ));
    }
    runtime::processor(fc)
        .await
        .map_err(|_| unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video"))
}

async fn runtime_unavailable(
    fc: &crate::bmc::feature::connection::FeatureConnection,
    operation: &'static str,
) -> Response {
    record_fault_response(
        fc,
        StatusCode::SERVICE_UNAVAILABLE,
        FeatureFault::operation(
            FeatureFaultDomain::Lifecycle,
            "backend_runtime_unavailable",
            "The video backend runtime is not available",
            true,
            operation,
        ),
    )
    .await
}

#[derive(Deserialize)]
pub struct TraceQuery {
    pub connection_id: Option<String>,
}

#[derive(Deserialize)]
pub struct ColorDepthSetRequest {
    pub profile_id: String,
}

/// Capability-driven source control. Backends that do not implement DVC (or
/// any equivalent source-side colour-depth control) return an empty list.
pub async fn color_depth_get(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return device_not_found();
    };
    let Some(fc) = dev.features.get("video").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
    };
    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => guard,
        Err(error) => {
            return acquisition_fault_response(&fc, feature_connect_status(&error), &error).await
        }
    };
    let response = match runtime::source(&fc).await {
        Ok(source) => {
            let profiles = source.color_depth_profiles();
            Json(serde_json::json!({
                "supported": !profiles.is_empty(),
                "profiles": profiles,
                "active_profile_id": source.active_color_depth_profile_id(),
            }))
            .into_response()
        }
        Err(_) => runtime_unavailable(&fc, "get_color_depth_profiles").await,
    };
    drop(guard);
    response
}

pub async fn color_depth_set(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<ColorDepthSetRequest>,
) -> Response {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return device_not_found();
    };
    let Some(fc) = dev.features.get("video").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
    };
    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => guard,
        Err(error) => {
            return acquisition_fault_response(&fc, feature_connect_status(&error), &error).await
        }
    };
    let response = match runtime::source(&fc).await {
        Ok(source) if source.color_depth_profiles().is_empty() => {
            record_fault_response(
                &fc,
                StatusCode::NOT_IMPLEMENTED,
                FeatureFault::operation(
                    FeatureFaultDomain::Operation,
                    "color_depth_unsupported",
                    "This video source does not support color-depth control",
                    false,
                    "set_color_depth",
                ),
            )
            .await
        }
        Ok(source) => match source.set_color_depth(&request.profile_id).await {
            Ok(()) => Json(serde_json::json!({"success": true, "profile_id": request.profile_id}))
                .into_response(),
            Err(error) => {
                record_fault_response(
                    &fc,
                    StatusCode::BAD_REQUEST,
                    FeatureFault::operation(
                        FeatureFaultDomain::Operation,
                        "color_depth_rejected",
                        format!(
                            "The video source rejected the requested color-depth profile: {error}"
                        ),
                        false,
                        "set_color_depth",
                    ),
                )
                .await
            }
        },
        Err(_) => runtime_unavailable(&fc, "set_color_depth").await,
    };
    drop(guard);
    response
}

pub async fn video_devices(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let _ = bmc_id;
    let _ = state;
    Json(serde_json::json!({"devices": []}))
}

pub async fn encoder_self_check(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    if let Some(dev) = state.registry.get_dev(&bmc_id).await {
        if let Some(fc) = dev.features.get("video").cloned() {
            if let Ok(processor) = runtime::processor(&fc).await {
                return Json(processor.encoder_self_check());
            }
        }
    }
    Json(serde_json::json!({"status": "unavailable"}))
}

pub async fn stream_start(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return device_not_found();
    };
    let Some(fc) = dev.features.get("video").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
    };
    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(30)).await {
        Ok(guard) => guard,
        Err(error) => {
            return acquisition_fault_response(&fc, feature_connect_status(&error), &error).await
        }
    };
    let response = match runtime::processor(&fc).await {
        Ok(processor) => Json(processor.stream_start().await).into_response(),
        Err(_) => runtime_unavailable(&fc, "start_stream").await,
    };
    drop(guard);
    response
}

pub async fn stream_stop(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return device_not_found();
    };
    let Some(fc) = dev.features.get("video").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
    };
    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => guard,
        Err(error) => {
            return acquisition_fault_response(&fc, feature_connect_status(&error), &error).await
        }
    };
    let response = match runtime::processor(&fc).await {
        Ok(processor) => Json(processor.stream_stop().await).into_response(),
        Err(_) => runtime_unavailable(&fc, "stop_stream").await,
    };
    drop(guard);
    response
}

pub async fn stream_reset(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return device_not_found();
    };
    let Some(fc) = dev.features.get("video") else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
    };
    if fc.is_stopped() {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
    }

    let mut reset_instances = 0usize;
    let mut closed_webrtc_connections = 0usize;
    let mut base_frame_replayed = false;
    let mut backend_results = Vec::new();
    let mut all_succeeded = true;

    for instance in dev.instances_for_feature("video").await {
        if instance.is_stopped() {
            all_succeeded = false;
            backend_results.push(serde_json::json!({
                "connection_id": instance.instance_id,
                "success": false,
                "fault": FeatureFault::operation(
                    FeatureFaultDomain::Lifecycle, "connection_stopped",
                    "The video connection has stopped", false, "reset_stream",
                ),
            }));
            continue;
        }
        if *instance.state.read().await != ConnectionState::Connected {
            if let Err(fault) = instance.faults.prepare_retry().await {
                all_succeeded = false;
                backend_results.push(serde_json::json!({
                    "connection_id": instance.instance_id,
                    "success": false,
                    "fault": fault,
                }));
                continue;
            }
            instance.wake.notify_one();
            backend_results.push(serde_json::json!({
                "connection_id": instance.instance_id,
                "success": true,
                "retry_requested": true,
            }));
            continue;
        }

        let guard = FeatureGuard::new(instance.clone());
        reset_instances += 1;

        match runtime::runtime(&instance).await {
            Some(runtime) => {
                let processor = runtime.processor();
                processor.clear_feature_fault().await;
                let source = runtime::source(&instance).await.ok();
                let backend_result = processor.reset_stream(source).await;
                let success = backend_result
                    .get("success")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true);
                all_succeeded &= success;
                closed_webrtc_connections += backend_result
                    .get("closed_webrtc_connections")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0) as usize;
                base_frame_replayed |= backend_result
                    .get("base_frame_replayed")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                backend_results.push(serde_json::json!({
                    "connection_id": instance.instance_id,
                    "success": success,
                    "source": backend_result,
                }));
            }
            None => {
                all_succeeded = false;
                let fault = FeatureFault::operation(
                    FeatureFaultDomain::Lifecycle,
                    "backend_runtime_unavailable",
                    "The video backend runtime is not available",
                    true,
                    "reset_stream",
                );
                instance.faults.record(fault.clone()).await;
                backend_results.push(serde_json::json!({
                    "connection_id": instance.instance_id,
                    "success": false,
                    "fault": fault,
                }));
            }
        }

        drop(guard);
    }

    let mut response = serde_json::json!({
        "success": all_succeeded,
        "reset_instances": reset_instances,
        "closed_webrtc_connections": closed_webrtc_connections,
        "base_frame_replayed": base_frame_replayed,
        "backends": backend_results,
    });
    if !all_succeeded {
        response["fault"] = serde_json::json!(FeatureFault::operation(
            FeatureFaultDomain::Operation,
            "video_reset_failed",
            "One or more video connections could not reset; inspect connection faults",
            true,
            "reset_stream",
        ));
    }
    (
        if all_succeeded {
            StatusCode::OK
        } else {
            StatusCode::BAD_GATEWAY
        },
        Json(response),
    )
        .into_response()
}

pub async fn stream_retry(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    retry_feature(&state, &bmc_id, "video").await
}

pub async fn stream_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    if let Some(dev) = state.registry.get_dev(&bmc_id).await {
        if let Some(fc) = dev.features.get("video") {
            let mut status = build_feature_status(fc).await;
            if let serde_json::Value::Object(ref mut map) = status {
                map.entry("streaming").or_insert(serde_json::json!(false));
                let connected = map
                    .get("online")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                map.insert("connected".into(), serde_json::json!(connected));
                let (instances, active_instances) =
                    build_feature_instances_status(&dev, "video").await;
                map.insert("instances".into(), serde_json::json!(instances));
                map.insert(
                    "active_instances".into(),
                    serde_json::json!(active_instances),
                );
                let fault = fc.faults.active().await;
                map.insert("fault".into(), serde_json::json!(fault));
                if let Some(runtime) = runtime::runtime(fc).await {
                    map.insert("signal".into(), serde_json::json!(runtime.signal_status()));
                }
            }
            return Json(status);
        }
    }
    Json(
        serde_json::json!({"available": false, "online": false, "streaming": false, "connected": false}),
    )
}

pub async fn stream_codecs(
    Path(_bmc_id): Path<String>,
    State(_state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let backends: Vec<serde_json::Value> = EncoderRegistry::discover()
        .iter()
        .map(|b| {
            let formats: Vec<&str> = crate::bmc::encoder::codec::codecs_for_backend(b.id)
                .into_iter()
                .filter(|id| {
                    let Some(codec) = crate::bmc::encoder::Codec::from_str(id) else {
                        return false;
                    };
                    !codec.supports_webrtc() || probe_webrtc_codec_for_backend(b.id, codec)
                })
                .collect();
            serde_json::json!({
                "id": b.id,
                "name": b.name,
                "is_hardware": b.is_hardware,
                "available": b.available,
                "supported_formats": formats,
            })
        })
        .collect();
    let codecs: Vec<serde_json::Value> = EncoderRegistry::codecs()
        .iter()
        .map(|c| {
            let codec = crate::bmc::encoder::Codec::from_str(c.id);
            let available = match codec {
                Some(codec) if codec.supports_webrtc() => codec_available_for_webrtc(codec),
                Some(_) => true,
                None => false,
            };
            let transports = if c.protocol == "webrtc" && !available {
                Vec::<&str>::new()
            } else {
                transports_for_protocol(c.protocol)
            };
            serde_json::json!({
                "id": c.id,
                "name": c.name,
                "protocol": c.protocol,
                "transports": transports,
                "hardware": false,
                "backend": "software",
                "available": available,
            })
        })
        .collect();
    Json(serde_json::json!({
        "success": true,
        "backends": backends,
        "codecs": codecs
    }))
}

pub async fn stream_constraints(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    match control_processor(&state, &bmc_id).await {
        Ok(processor) => Json(processor.constraints().await).into_response(),
        Err(response) => response,
    }
}

pub async fn trace_snapshot(
    Path(bmc_id): Path<String>,
    Query(query): Query<TraceQuery>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return device_not_found();
    };
    let fc = if let Some(connection_id) = query
        .connection_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        let Some(fc) = dev.get_instance("video", connection_id).await else {
            return fault_response(
                StatusCode::NOT_FOUND,
                FeatureFault::operation(
                    FeatureFaultDomain::Operation,
                    "connection_not_found",
                    "The requested video connection was not found",
                    false,
                    "trace",
                ),
            );
        };
        fc
    } else {
        let Some(fc) = dev.features.get("video").cloned() else {
            return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
        };
        fc
    };

    if fc.id != "video" {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "video");
    }

    let processor = match runtime::processor(&fc).await {
        Ok(processor) => processor,
        Err(_) => return runtime_unavailable(&fc, "trace").await,
    };
    let source = runtime::source(&fc)
        .await
        .ok()
        .map(|source| source.trace_snapshot());
    Json(serde_json::json!({
        "success": true,
        "trace": processor.trace_snapshot(source).await,
    }))
    .into_response()
}
