use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;

use crate::bmc::feature::{build_feature_instances_status, build_feature_status};
use crate::web::state::build_platform_capabilities;
use crate::web::state::AppState;

pub async fn device_features(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let capabilities = build_platform_capabilities();

    let mut features = Vec::new();
    if let Some(dev) = state.registry.get_dev(&bmc_id).await {
        let mut ids: Vec<_> = dev.features.keys().cloned().collect();
        ids.sort();
        for id in ids {
            if let Some(fc) = dev.features.get(&id) {
                let state = fc.state.read().await;
                features.push(serde_json::json!({
                    "id": id,
                    "backend": fc.backend,
                    "instance_id": fc.instance_id,
                    "state": format!("{state:?}"),
                    "healthy": fc.healthy.load(Ordering::Relaxed),
                    "ref_count": fc.ref_count.load(Ordering::Relaxed),
                }));
            }
        }
    }

    let mut response = capabilities;
    if let serde_json::Value::Object(ref mut map) = response {
        map.insert("features".into(), serde_json::json!(features));
        if let Some(dev) = state.registry.get_dev(&bmc_id).await {
            for id in [
                "video", "hid", "msd", "power", "boot", "identify", "firmware", "health", "audio",
            ] {
                let value = dev
                    .features
                    .get(id)
                    .map(|fc| {
                        serde_json::json!({
                            "available": true,
                            "configured": true,
                            "online": fc.healthy.load(Ordering::Relaxed),
                            "backend": fc.backend,
                        })
                    })
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "available": false,
                            "configured": false,
                            "online": false,
                            "backend": null,
                        })
                    });
                if id == "power" {
                    map.insert("atx".to_string(), value.clone());
                }
                map.insert(id.to_string(), value);
            }
        }
    }
    Json(response)
}

pub async fn device_features_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return Json(serde_json::json!({
            "features": [],
        }));
    };

    let mut ids: Vec<_> = dev.features.keys().cloned().collect();
    ids.sort();

    let mut features = Vec::new();
    for id in ids {
        if let Some(fc) = dev.features.get(&id) {
            let mut status = build_feature_status(fc).await;
            if let serde_json::Value::Object(ref mut map) = status {
                map.insert("id".into(), serde_json::json!(id));
                let (instances, active_instances) = build_feature_instances_status(&dev, &id).await;
                map.insert("instances".into(), serde_json::json!(instances));
                map.insert(
                    "active_instances".into(),
                    serde_json::json!(active_instances),
                );
            }
            features.push(status);
        }
    }

    Json(serde_json::json!({
        "features": features,
    }))
}
