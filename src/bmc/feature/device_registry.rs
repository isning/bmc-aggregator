use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{broadcast, watch, Mutex, RwLock};
use tracing::{info, warn};

use crate::bmc::feature::config::{
    BackendParams, FeatureConfig, FeatureConfigs, FeatureParams, FeatureProviderConfig,
    ReconnectConfig,
};
use crate::bmc::feature::connection::{
    spawn_ephemeral_lifecycle, spawn_lifecycle, ConnectionEvent, FeatureConnection, FeatureGuard,
};
use crate::bmc::feature::managed::ManagedDevice;
use crate::bmc::feature::registry::FeatureBackendTemplate;
use crate::config::DeviceConfig;

fn string_param(map: &HashMap<String, serde_json::Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn u16_param(map: &HashMap<String, serde_json::Value>, key: &str) -> Option<u16> {
    map.get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| u16::try_from(value).ok())
}

fn u32_param(map: &HashMap<String, serde_json::Value>, key: &str) -> Option<u32> {
    map.get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
}

fn reconnect_param(map: &HashMap<String, serde_json::Value>) -> ReconnectConfig {
    map.get("reconnect")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn backend_params_for(dev: &DeviceConfig, feature: &str, mut base: BackendParams) -> BackendParams {
    let Some(config) = dev.features.get(feature) else {
        return base;
    };
    let params = &config.backend_params;
    if let Some(host) = string_param(params, "host") {
        base.host = host;
    }
    if params.get("port").is_some_and(|value| value.is_null()) {
        base.port = None;
    } else if let Some(port) = u16_param(params, "port") {
        base.port = Some(port);
    }
    if params.get("kvm_port").is_some_and(|value| value.is_null()) {
        base.kvm_port = None;
    } else if let Some(kvm_port) = u16_param(params, "kvm_port") {
        base.kvm_port = Some(kvm_port);
    }
    if params.get("username").is_some_and(|value| value.is_null()) {
        base.username = None;
    } else if let Some(username) = string_param(params, "username") {
        base.username = Some(username);
    }
    if params.get("password").is_some_and(|value| value.is_null()) {
        base.password = None;
    } else if let Some(password) = string_param(params, "password") {
        base.password = Some(password);
    }
    if params.get("broadcast").is_some_and(|value| value.is_null()) {
        base.broadcast = None;
    } else if let Some(broadcast) = string_param(params, "broadcast") {
        base.broadcast = Some(broadcast);
    }
    if params.get("vnc_port").is_some_and(|value| value.is_null()) {
        base.vnc_port = None;
    } else if let Some(vnc_port) = u16_param(params, "vnc_port") {
        base.vnc_port = Some(vnc_port);
    }
    if params
        .get("vnc_password")
        .is_some_and(|value| value.is_null())
    {
        base.vnc_password = None;
    } else if let Some(vnc_password) = string_param(params, "vnc_password") {
        base.vnc_password = Some(vnc_password);
    }
    if params
        .get("vnc_security")
        .is_some_and(|value| value.is_null())
    {
        base.vnc_security = None;
    } else if let Some(vnc_security) = string_param(params, "vnc_security") {
        base.vnc_security = Some(vnc_security);
    }
    if params.get("vnc_tls").is_some_and(|value| value.is_null()) {
        base.vnc_tls = None;
    } else if let Some(vnc_tls) = params.get("vnc_tls").and_then(|value| value.as_bool()) {
        base.vnc_tls = Some(vnc_tls);
    }
    if params.get("avct_port").is_some_and(|value| value.is_null()) {
        base.avct_port = None;
    } else if let Some(avct_port) = u16_param(params, "avct_port") {
        base.avct_port = Some(avct_port);
    }
    if params
        .get("avct_vm_port")
        .is_some_and(|value| value.is_null())
    {
        base.avct_vm_port = None;
    } else if let Some(avct_vm_port) = u16_param(params, "avct_vm_port") {
        base.avct_vm_port = Some(avct_vm_port);
    }
    if params
        .get("avct_tls_mode")
        .is_some_and(|value| value.is_null())
    {
        base.avct_tls_mode = None;
    } else if let Some(avct_tls_mode) = string_param(params, "avct_tls_mode") {
        base.avct_tls_mode = Some(avct_tls_mode);
    }
    if params
        .get("avct_virtual_media_privilege")
        .is_some_and(|value| value.is_null())
    {
        base.avct_virtual_media_privilege = None;
    } else if let Some(virtual_media_privilege) = params
        .get("avct_virtual_media_privilege")
        .and_then(|value| value.as_bool())
    {
        base.avct_virtual_media_privilege = Some(virtual_media_privilege);
    }
    if params
        .get("idrac_generation")
        .is_some_and(|value| value.is_null())
    {
        base.idrac_generation = None;
    } else if let Some(generation) = params
        .get("idrac_generation")
        .and_then(|value| value.as_u64())
        .and_then(|value| u8::try_from(value).ok())
    {
        base.idrac_generation = Some(generation);
    }
    if params
        .get("avct_reconnect_capability")
        .is_some_and(|value| value.is_null())
    {
        base.avct_reconnect_capability = None;
    } else if let Some(value) = params
        .get("avct_reconnect_capability")
        .and_then(|value| value.as_bool())
    {
        base.avct_reconnect_capability = Some(value);
    }
    if params
        .get("avct_reconnect_version")
        .is_some_and(|value| value.is_null())
    {
        base.avct_reconnect_version = None;
    } else if let Some(value) = params
        .get("avct_reconnect_version")
        .and_then(|value| value.as_i64())
        .and_then(|value| i16::try_from(value).ok())
    {
        base.avct_reconnect_version = Some(value);
    }
    if params
        .get("avct_apcp_selector")
        .is_some_and(|value| value.is_null())
    {
        base.avct_apcp_selector = None;
    } else if let Some(value) = u32_param(params, "avct_apcp_selector") {
        base.avct_apcp_selector = Some(value);
    }
    if params
        .get("avct_reconnect_preference")
        .is_some_and(|value| value.is_null())
    {
        base.avct_reconnect_preference = None;
    } else if let Some(value) = params
        .get("avct_reconnect_preference")
        .and_then(|value| value.as_u64())
        .and_then(|value| u16::try_from(value).ok())
    {
        base.avct_reconnect_preference = Some(value);
    }
    if params
        .get("avmp_heartbeat_interval_s")
        .is_some_and(|value| value.is_null())
    {
        base.avmp_heartbeat_interval_s = None;
    } else if let Some(value) = u32_param(params, "avmp_heartbeat_interval_s") {
        base.avmp_heartbeat_interval_s = Some(value);
    }
    if params
        .get("avmp_reconnect_method")
        .is_some_and(|value| value.is_null())
    {
        base.avmp_reconnect_method = None;
    } else if let Some(value) = params
        .get("avmp_reconnect_method")
        .and_then(|value| value.as_u64())
        .and_then(|value| u8::try_from(value).ok())
    {
        base.avmp_reconnect_method = Some(value);
    }
    if params
        .get("redfish_insecure_tls")
        .is_some_and(|value| value.is_null())
    {
        base.redfish_insecure_tls = None;
    } else if let Some(insecure_tls) = params
        .get("redfish_insecure_tls")
        .and_then(|value| value.as_bool())
    {
        base.redfish_insecure_tls = Some(insecure_tls);
    }
    base
}

pub(crate) fn management_backend_params(
    dev: &DeviceConfig,
    feature: &str,
    backend: &str,
) -> BackendParams {
    let (host, port, broadcast) = match backend {
        "ipmi" => (
            dev.ipmi_host.clone().unwrap_or_else(|| dev.host.clone()),
            dev.ipmi_port.or(Some(623)),
            None,
        ),
        "wol" => (dev.host.clone(), None, Some("255.255.255.255".to_string())),
        "redfish" => (dev.host.clone(), Some(dev.port), None),
        "idrac-wsman" => (dev.host.clone(), None, None),
        _ => (dev.host.clone(), None, None),
    };
    backend_params_for(
        dev,
        feature,
        BackendParams {
            host,
            port,
            username: Some(dev.auth.username.clone()),
            password: Some(dev.auth.password.clone()),
            broadcast,
            ..Default::default()
        },
    )
}

fn boot_provider_params(
    dev: &DeviceConfig,
    provider: &crate::config::DeviceFeatureProviderConfig,
) -> BackendParams {
    let mut provider_device = dev.clone();
    if let Some(boot) = provider_device.features.get_mut("boot") {
        boot.backend_params = provider.backend_params.clone();
    }
    management_backend_params(&provider_device, "boot", &provider.backend)
}

fn feature_params_for(dev: &DeviceConfig, feature: &str, mut base: FeatureParams) -> FeatureParams {
    let Some(config) = dev.features.get(feature) else {
        return base;
    };
    let params = &config.feature_params;
    base.reconnect = reconnect_param(params);
    if params.get("max_fps").is_some_and(|value| value.is_null()) {
        base.max_fps = None;
    } else if let Some(max_fps) = u32_param(params, "max_fps") {
        base.max_fps = Some(max_fps);
    }
    for (key, slot) in [
        ("encoder_backend", &mut base.encoder_backend),
        ("video_device", &mut base.video_device),
        ("video_format", &mut base.video_format),
    ] {
        if params.get(key).is_some_and(|value| value.is_null()) {
            *slot = None;
        } else if let Some(value) = string_param(params, key) {
            *slot = Some(value);
        }
    }
    for (key, slot) in [
        ("video_width", &mut base.video_width),
        ("video_height", &mut base.video_height),
        ("video_fps", &mut base.video_fps),
        ("video_quality", &mut base.video_quality),
    ] {
        if params.get(key).is_some_and(|value| value.is_null()) {
            *slot = None;
        } else if let Some(value) = u32_param(params, key) {
            *slot = Some(value);
        }
    }
    base
}

/// Global device registry.
/// Owns all ManagedDevice instances and their feature connections.
pub struct DeviceRegistry {
    pub devices: RwLock<HashMap<String, Arc<ManagedDevice>>>,
    pub keepalive_guards: RwLock<HashMap<String, FeatureGuard>>,
    event_tx: broadcast::Sender<(String, String, ConnectionEvent)>,
    stopped: watch::Sender<bool>,
    reconfigure: Mutex<()>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReconfigurationReport {
    pub affected_features: HashMap<String, Vec<String>>,
}

impl ReconfigurationReport {
    pub fn features_for(&self, device_id: &str) -> Vec<String> {
        self.affected_features
            .get(device_id)
            .cloned()
            .unwrap_or_default()
    }
}

fn spawn_feature_event_forwarders(
    registry_tx: broadcast::Sender<(String, String, ConnectionEvent)>,
    device_id: String,
    feature_id: String,
    fc: Arc<FeatureConnection>,
) {
    let state_tx = registry_tx.clone();
    let state_device_id = device_id.clone();
    let state_feature_id = feature_id.clone();
    let mut state_rx = fc.event_rx.clone();
    let mut state_stopped = fc.stopped_rx();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                changed = state_stopped.changed() => {
                    if changed.is_err() || *state_stopped.borrow() {
                        break;
                    }
                }
                changed = state_rx.changed() => {
                    if changed.is_err() || *state_stopped.borrow() {
                        break;
                    }
                    if let Some(event) = state_rx.borrow().clone() {
                        let _ = state_tx.send((state_device_id.clone(), state_feature_id.clone(), event));
                    }
                }
            }
        }
    });

    let mut data_rx = fc.data_event_tx.subscribe();
    let mut stopped = fc.stopped_rx();
    let data_epoch = fc.data_epoch.clone();
    let instance_id = fc.instance_id.clone();
    tokio::spawn(async move {
        loop {
            let received = tokio::select! {
                biased;
                changed = stopped.changed() => {
                    if changed.is_err() || *stopped.borrow() {
                        break;
                    }
                    continue;
                }
                received = data_rx.recv() => received,
            };
            if *stopped.borrow() {
                break;
            }
            match received {
                Ok(event) => {
                    let _ = registry_tx.send((device_id.clone(), feature_id.clone(), event));
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(%device_id, %feature_id, skipped, "feature data forwarder lagged");
                    let event = ConnectionEvent::DataResyncRequired {
                        feature: feature_id.clone(),
                        instance: instance_id.clone(),
                        epoch: data_epoch.clone(),
                        resource: None,
                        skipped,
                    };
                    let _ = registry_tx.send((device_id.clone(), feature_id.clone(), event));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use futures::future::BoxFuture;

    use super::*;

    use crate::bmc::feature::config::{BackendParams, FeatureParams};
    use crate::bmc::feature::connection::{
        spawn_ephemeral_lifecycle_with_idle_timeout, ConnectionState, FeatureGuard,
    };
    use crate::bmc::feature::fault::{FeatureFault, FeatureFaultDomain};
    use crate::bmc::feature::registry::FeatureBackendTemplate;
    use crate::bmc::feature::video::runtime;
    use crate::config::{AuthConfig, DeviceConfig};

    #[tokio::test]
    async fn global_shutdown_is_visible_to_existing_and_late_subscribers() {
        let registry = DeviceRegistry::new();
        let mut existing = registry.stopped_rx();
        registry.shutdown().await;
        assert!(existing.has_changed().unwrap());
        assert!(*existing.borrow_and_update());
        assert!(*registry.stopped_rx().borrow());
        registry.shutdown().await;
        assert!(*registry.stopped_rx().borrow());
    }

    fn device_config(host: &str) -> DeviceConfig {
        let mut config = DeviceConfig {
            id: "dev1".to_string(),
            name: "Device One".to_string(),
            protocol: "test".to_string(),
            host: host.to_string(),
            port: 443,
            kvm_port: None,
            ipmi_host: None,
            ipmi_port: Some(623),
            auth: AuthConfig {
                username: "admin".to_string(),
                password: "test-password".to_string(),
            },
            max_fps: 30,
            video_backend: "test".to_string(),
            encoder_backend: "software".to_string(),
            audio_backend: None,
            features: HashMap::new(),
            video_device: None,
            video_format: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            video_quality: None,
        };
        config.normalize_feature_config();
        config
    }

    #[tokio::test]
    async fn init_does_not_infer_a_backend_from_protocol_or_legacy_fields() {
        let registry = DeviceRegistry::new();
        let mut config = device_config("bmc.example.test");
        config.features.clear();
        let backends = vec![FeatureBackendTemplate {
            backend: "test",
            feature: "video",
            connect: Arc::new(|_| Box::pin(async { Ok(()) })),
            requirements: None,
        }];

        registry.init(&[config], &backends).await;

        let device = registry.get_dev("dev1").await.unwrap();
        assert!(device.features.is_empty());
    }

    #[tokio::test]
    async fn init_materializes_generic_inventory_and_health_features() {
        let registry = DeviceRegistry::new();
        let mut config = device_config("bmc.example.test");
        for feature in ["firmware", "health"] {
            config.features.insert(
                feature.to_string(),
                crate::config::DeviceFeatureConfig {
                    disabled: false,
                    backend: Some("test-management".to_string()),
                    backend_params: HashMap::new(),
                    feature_params: HashMap::new(),
                    providers: Vec::new(),
                },
            );
        }
        let backends: Vec<_> = ["firmware", "health"]
            .into_iter()
            .map(|feature| FeatureBackendTemplate {
                backend: "test-management",
                feature,
                connect: Arc::new(|_| Box::pin(async { Ok(()) })),
                requirements: None,
            })
            .collect();

        registry.init(&[config], &backends).await;

        let device = registry.get_dev("dev1").await.unwrap();
        assert_eq!(device.features["firmware"].backend, "test-management");
        assert_eq!(device.features["health"].backend, "test-management");
    }

    #[test]
    fn management_backend_params_do_not_cross_protocol_boundaries() {
        let mut device = device_config("bmc.example");
        device.ipmi_host = Some("ipmi.example".to_string());
        device.ipmi_port = Some(6623);

        let wsman = management_backend_params(&device, "boot", "idrac-wsman");
        assert_eq!(wsman.host, "bmc.example");
        assert_eq!(wsman.port, None);
        assert_eq!(wsman.redfish_insecure_tls, None);
        assert_eq!(wsman.broadcast, None);

        let redfish = management_backend_params(&device, "boot", "redfish");
        assert_eq!(redfish.host, "bmc.example");
        assert_eq!(redfish.port, Some(443));
        assert_eq!(redfish.broadcast, None);

        let ipmi = management_backend_params(&device, "boot", "ipmi");
        assert_eq!(ipmi.host, "ipmi.example");
        assert_eq!(ipmi.port, Some(6623));
        assert_eq!(ipmi.broadcast, None);

        let wol = management_backend_params(&device, "power", "wol");
        assert_eq!(wol.host, "bmc.example");
        assert_eq!(wol.port, None);
        assert_eq!(wol.broadcast.as_deref(), Some("255.255.255.255"));
    }

    #[tokio::test]
    async fn creates_isolated_feature_instances() {
        let registry = DeviceRegistry::new();
        let mut features = HashMap::new();
        let feat_cfg = FeatureConfig {
            backend: "test".to_string(),
            backend_params: BackendParams::default(),
            feature_params: FeatureParams::default(),
            providers: Vec::new(),
        };
        features.insert(
            "video".to_string(),
            FeatureConnection::new(
                "dev1".to_string(),
                "video".to_string(),
                "test".to_string(),
                feat_cfg.clone(),
            ),
        );
        let mut cfg = FeatureConfigs {
            features: HashMap::new(),
        };
        cfg.features.insert("video".to_string(), feat_cfg);
        registry
            .insert(
                "dev1".to_string(),
                Arc::new(ManagedDevice::new("dev1".to_string(), cfg, features)),
            )
            .await;

        let connects = Arc::new(AtomicUsize::new(0));
        let connects2 = connects.clone();
        let backends = vec![FeatureBackendTemplate {
            backend: "test",
            feature: "video",
            connect: Arc::new(move |_fc| {
                let connects = connects2.clone();
                Box::pin(async move {
                    connects.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }) as BoxFuture<'static, Result<(), String>>
            }),
            requirements: None,
        }];

        let a = registry
            .get_or_create_feature_instance("dev1", "video", "a", &backends)
            .await
            .unwrap();
        let b = registry
            .get_or_create_feature_instance("dev1", "video", "b", &backends)
            .await
            .unwrap();
        let a2 = registry
            .get_or_create_feature_instance("dev1", "video", "a", &backends)
            .await
            .unwrap();
        let default = registry
            .get_or_create_feature_instance("dev1", "video", "default", &backends)
            .await
            .unwrap();
        let dev = registry.get_dev("dev1").await.unwrap();
        let default_from_instances = dev.get_instance("video", "default").await.unwrap();

        assert_eq!(a.instance_id, "a");
        assert_eq!(b.instance_id, "b");
        assert!(Arc::ptr_eq(&a, &a2));
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(default.instance_id, "default");
        assert!(Arc::ptr_eq(&default, &default_from_instances));
    }

    #[tokio::test]
    async fn concurrent_feature_instance_creation_starts_one_lifecycle() {
        let registry = DeviceRegistry::new();
        let mut features = HashMap::new();
        let feat_cfg = FeatureConfig {
            backend: "test".to_string(),
            backend_params: BackendParams::default(),
            feature_params: FeatureParams::default(),
            providers: Vec::new(),
        };
        features.insert(
            "video".to_string(),
            FeatureConnection::new(
                "dev1".to_string(),
                "video".to_string(),
                "test".to_string(),
                feat_cfg.clone(),
            ),
        );
        let mut cfg = FeatureConfigs {
            features: HashMap::new(),
        };
        cfg.features.insert("video".to_string(), feat_cfg);
        registry
            .insert(
                "dev1".to_string(),
                Arc::new(ManagedDevice::new("dev1".to_string(), cfg, features)),
            )
            .await;

        let connects = Arc::new(AtomicUsize::new(0));
        let connects2 = connects.clone();
        let backends = Arc::new(vec![FeatureBackendTemplate {
            backend: "test",
            feature: "video",
            connect: Arc::new(move |_fc| {
                let connects = connects2.clone();
                Box::pin(async move {
                    connects.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }) as BoxFuture<'static, Result<(), String>>
            }),
            requirements: None,
        }]);

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let registry = registry.clone();
            let backends = backends.clone();
            tasks.push(tokio::spawn(async move {
                registry
                    .get_or_create_feature_instance("dev1", "video", "same", &backends)
                    .await
                    .unwrap()
            }));
        }

        let first = tasks.remove(0).await.unwrap();
        let _guard = FeatureGuard::new(first.clone());
        for task in tasks {
            let fc = task.await.unwrap();
            assert!(Arc::ptr_eq(&first, &fc));
        }

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if connects.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(connects.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ephemeral_feature_instance_is_removed_after_disconnect() {
        let registry = DeviceRegistry::new();
        let mut features = HashMap::new();
        let feat_cfg = FeatureConfig {
            backend: "test".to_string(),
            backend_params: BackendParams::default(),
            feature_params: FeatureParams::default(),
            providers: Vec::new(),
        };
        features.insert(
            "video".to_string(),
            FeatureConnection::new(
                "dev1".to_string(),
                "video".to_string(),
                "test".to_string(),
                feat_cfg.clone(),
            ),
        );
        let mut cfg = FeatureConfigs {
            features: HashMap::new(),
        };
        cfg.features.insert("video".to_string(), feat_cfg.clone());
        let dev = Arc::new(ManagedDevice::new("dev1".to_string(), cfg, features));
        registry.insert("dev1".to_string(), dev.clone()).await;

        let fc = FeatureConnection::new_instance(
            "dev1".to_string(),
            "video".to_string(),
            "ephemeral".to_string(),
            "test".to_string(),
            feat_cfg,
        );
        dev.insert_instance("video", "ephemeral", fc.clone()).await;

        let dev_for_cleanup = dev.clone();
        spawn_ephemeral_lifecycle_with_idle_timeout(
            fc.clone(),
            Arc::new(|_fc| Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), String>>),
            std::time::Duration::from_millis(20),
            Arc::new(move || {
                let dev = dev_for_cleanup.clone();
                Box::pin(async move {
                    let _ = dev.remove_instance("video", "ephemeral").await;
                })
            }),
        );

        let guard = FeatureGuard::new(fc.clone());
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        while tokio::time::Instant::now() < deadline {
            if *fc.state.read().await == ConnectionState::Connected {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        drop(guard);

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        while tokio::time::Instant::now() < deadline {
            if dev.get_instance("video", "ephemeral").await.is_none() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        panic!("ephemeral instance was not removed after disconnect");
    }

    #[tokio::test]
    async fn shutdown_releases_keepalive_guards_and_backend_runtime() {
        let registry = DeviceRegistry::new();
        let feature_config = FeatureConfig {
            backend: "test".to_string(),
            backend_params: BackendParams::default(),
            feature_params: FeatureParams::default(),
            providers: Vec::new(),
        };
        let fc = FeatureConnection::new(
            "dev1".to_string(),
            "power".to_string(),
            "test".to_string(),
            feature_config.clone(),
        );
        fc.set_backend_runtime(Arc::new("backend".to_string()))
            .await;

        let mut features = HashMap::new();
        features.insert("power".to_string(), fc.clone());
        let mut configs = FeatureConfigs {
            features: HashMap::new(),
        };
        configs.features.insert("power".to_string(), feature_config);
        registry
            .insert(
                "dev1".to_string(),
                Arc::new(ManagedDevice::new("dev1".to_string(), configs, features)),
            )
            .await;
        registry
            .keepalive_guards
            .write()
            .await
            .insert("dev1:power".to_string(), FeatureGuard::new(fc.clone()));

        registry.shutdown().await;

        assert!(registry.keepalive_guards.read().await.is_empty());
        assert!(fc.is_stopped());
        assert!(fc.backend_runtime::<Arc<String>>().await.is_none());
    }

    #[tokio::test]
    async fn replace_all_retains_unchanged_devices_and_stops_replaced_ones() {
        let registry = DeviceRegistry::new();
        let backends = vec![FeatureBackendTemplate {
            backend: "test",
            feature: "video",
            connect: Arc::new(|_| Box::pin(async { Ok(()) })),
            requirements: None,
        }];
        let original_config = device_config("bmc-one.example.test");
        registry
            .init(std::slice::from_ref(&original_config), &backends)
            .await;
        let original = registry.get_dev("dev1").await.unwrap();
        let original_video = original.features.get("video").unwrap().clone();

        registry
            .replace_all(std::slice::from_ref(&original_config), &backends)
            .await;
        let retained = registry.get_dev("dev1").await.unwrap();
        assert!(Arc::ptr_eq(&original, &retained));
        assert!(!original_video.is_stopped());

        let replacement_config = device_config("bmc-two.example.test");
        registry.replace_all(&[replacement_config], &backends).await;
        let replacement = registry.get_dev("dev1").await.unwrap();
        assert!(!Arc::ptr_eq(&original, &replacement));
        assert!(original_video.is_stopped());
    }

    fn video_hid_backends() -> Vec<FeatureBackendTemplate> {
        ["video", "hid"]
            .into_iter()
            .map(|feature| FeatureBackendTemplate {
                backend: "test",
                feature,
                connect: Arc::new(|_| Box::pin(async { Ok(()) })),
                requirements: None,
            })
            .collect()
    }

    #[tokio::test]
    async fn replace_all_replaces_only_the_changed_feature_connection() {
        let registry = DeviceRegistry::new();
        let backends = video_hid_backends();
        let original_config = device_config("bmc.example.test");
        registry
            .init(std::slice::from_ref(&original_config), &backends)
            .await;
        let original = registry.get_dev("dev1").await.unwrap();
        let original_video = original.features["video"].clone();
        let original_hid = original.features["hid"].clone();

        let mut changed = original_config;
        changed
            .features
            .get_mut("video")
            .unwrap()
            .feature_params
            .insert("max_fps".to_string(), serde_json::json!(15));
        let report = registry.replace_all(&[changed], &backends).await;
        let replacement = registry.get_dev("dev1").await.unwrap();

        assert!(!Arc::ptr_eq(
            &original_video,
            &replacement.features["video"]
        ));
        assert!(Arc::ptr_eq(&original_hid, &replacement.features["hid"]));
        assert!(original_video.is_stopped());
        assert!(!original_hid.is_stopped());
        assert_eq!(report.features_for("dev1"), vec!["video"]);
    }

    #[tokio::test]
    async fn replace_all_keeps_every_connection_for_display_name_changes() {
        let registry = DeviceRegistry::new();
        let backends = video_hid_backends();
        let original_config = device_config("bmc.example.test");
        registry
            .init(std::slice::from_ref(&original_config), &backends)
            .await;
        let original = registry.get_dev("dev1").await.unwrap();

        let mut renamed = original_config;
        renamed.name = "Renamed device".to_string();
        let report = registry.replace_all(&[renamed], &backends).await;
        let replacement = registry.get_dev("dev1").await.unwrap();

        assert!(Arc::ptr_eq(
            &original.features["video"],
            &replacement.features["video"]
        ));
        assert!(Arc::ptr_eq(
            &original.features["hid"],
            &replacement.features["hid"]
        ));
        assert!(report.features_for("dev1").is_empty());
    }

    #[tokio::test]
    async fn retained_ephemeral_cleanup_reaches_the_current_device_owner() {
        let registry = DeviceRegistry::new();
        let backends = video_hid_backends();
        let mut config = device_config("bmc.example.test");
        registry
            .init(std::slice::from_ref(&config), &backends)
            .await;
        let original = registry.get_dev("dev1").await.unwrap();
        let fc = registry
            .get_or_create_feature_instance("dev1", "video", "browser", &backends)
            .await
            .unwrap();
        let guard = FeatureGuard::new(fc.clone());
        config.name = "New display name".into();
        registry.replace_all(&[config], &backends).await;
        let current = registry.get_dev("dev1").await.unwrap();
        assert!(!Arc::ptr_eq(&original, &current));
        assert!(Arc::ptr_eq(
            &fc,
            &current.get_instance("video", "browser").await.unwrap()
        ));
        fc.shutdown();
        drop(guard);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while current.get_instance("video", "browser").await.is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("terminal cleanup must remove the retained current instance");
        assert!(original.get_instance("video", "browser").await.is_none());
        let replacement = registry
            .get_or_create_feature_instance("dev1", "video", "browser", &backends)
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&fc, &replacement));
        assert!(!replacement.is_stopped());
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn instance_creation_waits_for_registry_publication() {
        let registry = DeviceRegistry::new();
        let backends = video_hid_backends();
        registry
            .init(&[device_config("bmc.example.test")], &backends)
            .await;
        let gate = registry.reconfigure.lock().await;
        let mut creation = tokio::spawn({
            let registry = registry.clone();
            async move {
                registry
                    .get_or_create_feature_instance("dev1", "video", "browser", &backends)
                    .await
            }
        });
        let blocked = tokio::time::timeout(std::time::Duration::from_millis(30), &mut creation)
            .await
            .is_err();
        let published = registry
            .get_dev("dev1")
            .await
            .unwrap()
            .get_instance("video", "browser")
            .await
            .is_some();
        drop(gate);
        let instance = creation.await.unwrap().unwrap();
        assert!(
            blocked && !published,
            "creation must wait for configuration publication"
        );
        assert!(!instance.is_stopped());
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn delayed_cleanup_keeps_a_replacement_with_the_same_instance_id() {
        let registry = DeviceRegistry::new();
        let backends = video_hid_backends();
        registry
            .init(&[device_config("bmc.example.test")], &backends)
            .await;
        let old = registry
            .get_or_create_feature_instance("dev1", "video", "browser", &backends)
            .await
            .unwrap();
        let dev = registry.get_dev("dev1").await.unwrap();
        let gate = registry.reconfigure.lock().await;
        old.shutdown();
        dev.remove_instance("video", "browser").await;
        let replacement = FeatureConnection::new_instance(
            "dev1".into(),
            "video".into(),
            "browser".into(),
            "test".into(),
            old.config.clone(),
        );
        dev.insert_instance("video", "browser", replacement.clone())
            .await;
        drop(gate);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while Arc::strong_count(&old) > 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("old lifecycle and forwarders must finish cleanup");
        assert!(Arc::ptr_eq(
            &replacement,
            &dev.get_instance("video", "browser").await.unwrap()
        ));
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn stopped_registry_rejects_new_feature_instances() {
        let registry = DeviceRegistry::new();
        let backends = video_hid_backends();
        registry
            .init(&[device_config("bmc.example.test")], &backends)
            .await;
        registry.shutdown().await;
        let result = registry
            .get_or_create_feature_instance("dev1", "video", "after-stop", &backends)
            .await;
        assert!(
            result.is_err(),
            "shutdown must be a terminal instance-admission boundary"
        );
        assert!(registry
            .get_dev("dev1")
            .await
            .unwrap()
            .get_instance("video", "after-stop")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn replace_all_replaces_each_effective_shared_credential_consumer() {
        let registry = DeviceRegistry::new();
        let backends = video_hid_backends();
        let original_config = device_config("bmc.example.test");
        registry
            .init(std::slice::from_ref(&original_config), &backends)
            .await;

        let mut changed = original_config;
        changed.auth.password = "replacement-password".to_string();
        let report = registry.replace_all(&[changed], &backends).await;

        assert_eq!(
            report.features_for("dev1"),
            vec!["hid", "msd", "power", "video"]
        );
    }

    #[tokio::test]
    async fn initialized_video_runtime_uses_the_feature_fault_store() {
        let registry = DeviceRegistry::new();
        let backends = vec![FeatureBackendTemplate {
            backend: "test",
            feature: "video",
            connect: Arc::new(|_| Box::pin(async { Ok(()) })),
            requirements: None,
        }];
        registry
            .init(&[device_config("bmc.example.test")], &backends)
            .await;

        let device = registry.get_dev("dev1").await.unwrap();
        let fc = device.features.get("video").unwrap().clone();
        let processor = runtime::processor(&fc).await.unwrap();
        processor
            .record_fault(FeatureFault::feature(
                FeatureFaultDomain::Lifecycle,
                "test_lifecycle_fault",
                "test",
                true,
            ))
            .await;
        assert_eq!(
            fc.faults.active().await.unwrap().code,
            "test_lifecycle_fault"
        );

        fc.faults.clear_active().await;
        assert!(processor.active_feature_fault().await.is_none());
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn concurrent_full_reconfigurations_cannot_interleave_device_sets() {
        let registry = DeviceRegistry::new();
        let first = vec![device_config("bmc-one.example.test")];
        let mut second_device = device_config("bmc-two.example.test");
        second_device.id = "dev2".to_string();
        let second = vec![second_device];

        let (_first_report, _second_report) = tokio::join!(
            registry.replace_all(&first, &[]),
            registry.replace_all(&second, &[]),
        );

        assert_eq!(registry.list_devices().await.len(), 1);
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn stopped_connection_forwarders_drop_late_state_and_data_events() {
        let registry = DeviceRegistry::new();
        let fc = FeatureConnection::new(
            "dev1".to_string(),
            "power".to_string(),
            "test".to_string(),
            FeatureConfig {
                backend: "test".to_string(),
                backend_params: BackendParams::default(),
                feature_params: FeatureParams::default(),
                providers: Vec::new(),
            },
        );
        spawn_feature_event_forwarders(
            registry.event_tx.clone(),
            "dev1".to_string(),
            "power".to_string(),
            fc.clone(),
        );
        let mut events = registry.event_tx.subscribe();

        fc.shutdown();
        fc.transition(ConnectionState::Connected).await;
        let _ = fc.data_event_tx.send(ConnectionEvent::DataChanged {
            feature: "power".to_string(),
            instance: "default".to_string(),
            epoch: fc.data_epoch.clone(),
            resource: "status".to_string(),
            revision: 1,
            cause: "late".to_string(),
            mutation_id: None,
            data: serde_json::json!({"power_status": "on"}),
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), events.recv())
                .await
                .is_err()
        );
    }
}

impl DeviceRegistry {
    pub fn new() -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(256);
        let (stopped, _) = watch::channel(false);
        Arc::new(Self {
            devices: RwLock::new(HashMap::new()),
            keepalive_guards: RwLock::new(HashMap::new()),
            event_tx,
            stopped,
            reconfigure: Mutex::new(()),
        })
    }

    pub fn event_tx(&self) -> broadcast::Sender<(String, String, ConnectionEvent)> {
        self.event_tx.clone()
    }

    pub(crate) fn stopped_rx(&self) -> watch::Receiver<bool> {
        self.stopped.subscribe()
    }

    pub async fn get_dev(&self, id: &str) -> Option<Arc<ManagedDevice>> {
        self.devices.read().await.get(id).cloned()
    }

    pub async fn list_devices(&self) -> Vec<String> {
        self.devices.read().await.keys().cloned().collect()
    }

    pub async fn insert(&self, id: String, dev: Arc<ManagedDevice>) {
        let _reconfigure = self.reconfigure.lock().await;
        self.devices.write().await.insert(id, dev);
    }

    pub async fn remove(&self, id: &str) -> Option<Arc<ManagedDevice>> {
        let _reconfigure = self.reconfigure.lock().await;
        self.devices.write().await.remove(id)
    }

    /// Stop every feature lifecycle and release keepalive guards during
    /// process shutdown.  The registry retains device metadata so diagnostic
    /// callers can still inspect it while shutdown is in progress.
    pub async fn shutdown(&self) {
        self.stopped.send_replace(true);
        let _reconfigure = self.reconfigure.lock().await;
        self.keepalive_guards.write().await.clear();
        let devices: Vec<_> = self.devices.read().await.values().cloned().collect();
        for device in devices {
            device.shutdown().await;
        }
    }

    pub async fn replace_all(
        self: &Arc<Self>,
        devices: &[DeviceConfig],
        backends: &[super::registry::FeatureBackendTemplate],
    ) -> ReconfigurationReport {
        self.replace_all_with_publication(devices, backends, |_| async {})
            .await
    }

    /// Drain retired runtimes before acquiring the caller's publication guard.
    /// Keep that guard through the registry swap, so configuration can publish
    /// atomically without holding its write lock across backend cleanup.
    pub(crate) async fn replace_all_with_publication<F, Fut, Guard>(
        self: &Arc<Self>,
        devices: &[DeviceConfig],
        backends: &[super::registry::FeatureBackendTemplate],
        publication: F,
    ) -> ReconfigurationReport
    where
        F: FnOnce(&ReconfigurationReport) -> Fut,
        Fut: std::future::Future<Output = Guard>,
    {
        let _reconfigure = self.reconfigure.lock().await;
        let old_devices = self.devices.read().await.clone();
        let mut retained = HashMap::new();
        let mut changed = Vec::new();
        for config in devices {
            let fingerprint = serde_json::to_string(config).unwrap_or_default();
            if old_devices
                .get(&config.id)
                .is_some_and(|device| device.source_config.as_deref() == Some(&fingerprint))
            {
                retained.insert(config.id.clone(), old_devices[&config.id].clone());
            } else {
                changed.push(config.clone());
            }
        }

        // Construct replacements outside the live registry. Their lifecycle
        // tasks remain idle until a consumer acquires them, while their event
        // forwarders already target the live registry broadcast channel.
        let staging = Arc::new(Self {
            devices: RwLock::new(HashMap::new()),
            keepalive_guards: RwLock::new(HashMap::new()),
            event_tx: self.event_tx.clone(),
            stopped: self.stopped.clone(),
            reconfigure: Mutex::new(()),
        });
        staging.init(&changed, backends).await;
        let mut replacements = std::mem::take(&mut *staging.devices.write().await);
        let mut report = ReconfigurationReport::default();

        for config in changed {
            let Some(staged) = replacements.remove(&config.id) else {
                continue;
            };
            let Some(previous) = old_devices.get(&config.id) else {
                let mut affected: Vec<_> = staged.cfg.features.keys().cloned().collect();
                affected.sort();
                report.affected_features.insert(config.id.clone(), affected);
                retained.insert(config.id.clone(), staged);
                continue;
            };

            let feature_ids: HashSet<_> = previous
                .cfg
                .features
                .keys()
                .chain(staged.cfg.features.keys())
                .cloned()
                .collect();
            let mut merged_features = staged.features.clone();
            let mut merged_instances = staged.feature_instances.read().await.clone();
            let mut affected = Vec::new();

            for feature_id in feature_ids {
                let previous_fingerprint = previous
                    .cfg
                    .features
                    .get(&feature_id)
                    .and_then(|config| serde_json::to_string(config).ok());
                let staged_fingerprint = staged
                    .cfg
                    .features
                    .get(&feature_id)
                    .and_then(|config| serde_json::to_string(config).ok());
                if previous_fingerprint == staged_fingerprint {
                    staged.shutdown_feature(&feature_id).await;
                    merged_instances.retain(|key, _| !key.starts_with(&format!("{feature_id}:")));
                    for instance in previous.instances_for_feature(&feature_id).await {
                        merged_instances
                            .insert(format!("{feature_id}:{}", instance.instance_id), instance);
                    }
                    match previous.features.get(&feature_id) {
                        Some(connection) => {
                            merged_features.insert(feature_id.clone(), connection.clone());
                        }
                        None => {
                            merged_features.remove(&feature_id);
                        }
                    }
                } else {
                    previous.shutdown_feature(&feature_id).await;
                    affected.push(feature_id);
                }
            }

            affected.sort();
            if !affected.is_empty() {
                report.affected_features.insert(config.id.clone(), affected);
            }
            let merged = Arc::new(ManagedDevice {
                id: config.id.clone(),
                source_config: Some(serde_json::to_string(&config).unwrap_or_default()),
                cfg: staged.cfg.clone(),
                features: merged_features,
                feature_instances: RwLock::new(merged_instances),
            });
            retained.insert(config.id.clone(), merged);
        }

        let next_ids: HashSet<_> = retained.keys().cloned().collect();
        for (id, previous) in &old_devices {
            if !next_ids.contains(id) {
                let mut affected: Vec<_> = previous.cfg.features.keys().cloned().collect();
                affected.sort();
                report.affected_features.insert(id.clone(), affected);
                previous.shutdown().await;
            }
        }

        let mut retained_connections = HashSet::new();
        for device in retained.values() {
            for connection in device.feature_instances.read().await.values() {
                retained_connections.insert(Arc::as_ptr(connection) as usize);
            }
        }
        self.keepalive_guards
            .write()
            .await
            .retain(|_, guard| retained_connections.contains(&(Arc::as_ptr(guard.fc()) as usize)));
        let _publication = publication(&report).await;
        *self.devices.write().await = retained;
        report
    }

    pub async fn get_or_create_feature_instance(
        self: &Arc<Self>,
        device_id: &str,
        feature_id: &str,
        instance_id: &str,
        backends: &[FeatureBackendTemplate],
    ) -> Result<Arc<FeatureConnection>, String> {
        // Instance admission and cleanup share the registry publication boundary.
        // No backend I/O is awaited while this gate is held.
        let _reconfigure = self.reconfigure.lock().await;
        if *self.stopped.borrow() {
            return Err(
                "feature instance creation is not available: device registry is stopped".into(),
            );
        }
        let dev = self
            .get_dev(device_id)
            .await
            .ok_or_else(|| format!("device '{device_id}' not found"))?;
        if let Some(fc) = dev.get_instance(feature_id, instance_id).await {
            if !fc.is_stopped() {
                return Ok(fc);
            }
            dev.remove_instance_if_same(&fc).await;
        }

        let feat_cfg = dev
            .cfg
            .features
            .get(feature_id)
            .ok_or_else(|| {
                format!("feature '{feature_id}' not configured on device '{device_id}'")
            })?
            .clone();
        let fc = FeatureConnection::new_instance(
            device_id.to_string(),
            feature_id.to_string(),
            instance_id.to_string(),
            feat_cfg.backend.clone(),
            feat_cfg.clone(),
        );
        let (fc, inserted) = dev
            .get_or_insert_instance_with(feature_id, instance_id, fc, |fc| {
                if let Some(tpl) = backends.iter()
                    .find(|template| template.backend == feat_cfg.backend && template.feature == feature_id)
                {
                    if instance_id == "default" {
                        spawn_lifecycle(fc.clone(), tpl.connect.clone());
                    } else {
                        let dev_for_cleanup = Arc::downgrade(&dev);
                        let registry_for_cleanup = Arc::downgrade(self);
                        let connection_for_cleanup = Arc::downgrade(fc);
                        spawn_ephemeral_lifecycle(fc.clone(), tpl.connect.clone(), Arc::new(move || {
                            let dev = dev_for_cleanup.clone();
                            let registry = registry_for_cleanup.clone();
                            let connection = connection_for_cleanup.clone();
                            Box::pin(async move {
                                let Some(connection) = connection.upgrade() else { return; };
                                if let Some(registry) = registry.upgrade() {
                                    let _reconfigure = registry.reconfigure.lock().await;
                                    if let Some(dev) = dev.upgrade() {
                                        dev.remove_instance_if_same(&connection).await;
                                    }
                                    // An unchanged feature may now belong to a newly
                                    // published ManagedDevice after a partial reload.
                                    if let Some(current) = registry.get_dev(&connection.device_id).await {
                                        current.remove_instance_if_same(&connection).await;
                                    }
                                } else if let Some(dev) = dev.upgrade() {
                                    dev.remove_instance_if_same(&connection).await;
                                }
                            })
                        }));
                    }
                } else {
                    warn!("[{device_id}:{feature_id}:{instance_id}] no backend '{}' registered, skipping lifecycle", feat_cfg.backend);
                }
            }).await?;
        if !inserted {
            return Ok(fc);
        }

        spawn_feature_event_forwarders(
            self.event_tx.clone(),
            device_id.to_string(),
            feature_id.to_string(),
            fc.clone(),
        );

        Ok(fc)
    }

    /// Initialize registry from config.
    /// Creates ManagedDevice entries with FeatureConnections and starts
    /// background lifecycle tasks for each registered backend.
    pub async fn init(
        self: &Arc<Self>,
        devices: &[DeviceConfig],
        backends: &[super::registry::FeatureBackendTemplate],
    ) {
        for dev_cfg in devices {
            let mut feat_cfgs = FeatureConfigs {
                features: HashMap::new(),
            };
            let params = super::config::BackendParams {
                host: dev_cfg.host.clone(),
                port: Some(dev_cfg.port),
                kvm_port: dev_cfg.kvm_port,
                username: Some(dev_cfg.auth.username.clone()),
                password: Some(dev_cfg.auth.password.clone()),
                broadcast: None,
                vnc_port: None,
                vnc_password: None,
                vnc_security: None,
                vnc_tls: None,
                avct_port: None,
                avct_vm_port: None,
                avct_tls_mode: None,
                avct_virtual_media_privilege: None,
                redfish_insecure_tls: None,
                ..Default::default()
            };
            if let Some(backend) = dev_cfg.feature_backend("video") {
                feat_cfgs.features.insert(
                    "video".into(),
                    FeatureConfig {
                        backend: backend.to_string(),
                        backend_params: backend_params_for(dev_cfg, "video", params.clone()),
                        feature_params: feature_params_for(
                            dev_cfg,
                            "video",
                            FeatureParams {
                                max_fps: Some(dev_cfg.canonical_max_fps()),
                                encoder_backend: Some(dev_cfg.canonical_encoder_backend()),
                                video_device: dev_cfg.canonical_video_device(),
                                video_format: dev_cfg.canonical_video_format(),
                                video_width: dev_cfg.canonical_video_width(),
                                video_height: dev_cfg.canonical_video_height(),
                                video_fps: dev_cfg.canonical_video_fps(),
                                video_quality: dev_cfg.canonical_video_quality(),
                                reconnect: Default::default(),
                            },
                        ),
                        providers: Vec::new(),
                    },
                );
            }
            if let Some(backend) = dev_cfg.feature_backend("hid") {
                feat_cfgs.features.insert(
                    "hid".into(),
                    FeatureConfig {
                        backend: backend.to_string(),
                        backend_params: backend_params_for(dev_cfg, "hid", params.clone()),
                        feature_params: feature_params_for(dev_cfg, "hid", Default::default()),
                        providers: Vec::new(),
                    },
                );
            }
            if let Some(backend) = dev_cfg.feature_backend("msd") {
                feat_cfgs.features.insert(
                    "msd".into(),
                    FeatureConfig {
                        backend: backend.to_string(),
                        backend_params: backend_params_for(dev_cfg, "msd", params.clone()),
                        feature_params: feature_params_for(dev_cfg, "msd", Default::default()),
                        providers: Vec::new(),
                    },
                );
            }
            if let Some(backend) = dev_cfg.feature_backend("power") {
                feat_cfgs.features.insert(
                    "power".into(),
                    FeatureConfig {
                        backend: backend.to_string(),
                        backend_params: management_backend_params(dev_cfg, "power", backend),
                        feature_params: feature_params_for(dev_cfg, "power", Default::default()),
                        providers: Vec::new(),
                    },
                );
            }
            let boot_providers = dev_cfg.feature_providers("boot");
            if !boot_providers.is_empty() {
                feat_cfgs.features.insert(
                    "boot".into(),
                    FeatureConfig {
                        backend: String::new(),
                        backend_params: BackendParams::default(),
                        feature_params: feature_params_for(dev_cfg, "boot", Default::default()),
                        providers: boot_providers
                            .iter()
                            .map(|provider| FeatureProviderConfig {
                                id: provider.id.clone(),
                                backend: provider.backend.clone(),
                                backend_params: boot_provider_params(dev_cfg, provider),
                            })
                            .collect(),
                    },
                );
            } else if let Some(backend) = dev_cfg.feature_backend("boot") {
                feat_cfgs.features.insert(
                    "boot".into(),
                    FeatureConfig {
                        backend_params: management_backend_params(dev_cfg, "boot", backend),
                        backend: backend.to_string(),
                        feature_params: feature_params_for(dev_cfg, "boot", Default::default()),
                        providers: Vec::new(),
                    },
                );
            }
            if let Some(backend) = dev_cfg.feature_backend("identify") {
                feat_cfgs.features.insert(
                    "identify".into(),
                    FeatureConfig {
                        backend_params: management_backend_params(dev_cfg, "identify", backend),
                        backend: backend.to_string(),
                        feature_params: feature_params_for(dev_cfg, "identify", Default::default()),
                        providers: Vec::new(),
                    },
                );
            }
            for feature_id in ["firmware", "health"] {
                if let Some(backend) = dev_cfg.feature_backend(feature_id) {
                    feat_cfgs.features.insert(
                        feature_id.to_string(),
                        FeatureConfig {
                            backend_params: management_backend_params(dev_cfg, feature_id, backend),
                            backend: backend.to_string(),
                            feature_params: feature_params_for(
                                dev_cfg,
                                feature_id,
                                Default::default(),
                            ),
                            providers: Vec::new(),
                        },
                    );
                }
            }

            let mut connections: HashMap<String, Arc<FeatureConnection>> = HashMap::new();

            for (feat_id, feat_cfg) in &feat_cfgs.features {
                if feat_id == "boot" && !feat_cfg.providers.is_empty() {
                    let fc = FeatureConnection::new(
                        dev_cfg.id.clone(),
                        feat_id.clone(),
                        String::new(),
                        feat_cfg.clone(),
                    );
                    spawn_lifecycle(
                        fc.clone(),
                        Arc::new(|fc| crate::bmc::providers::boot::connect(fc)),
                    );
                    spawn_feature_event_forwarders(
                        self.event_tx.clone(),
                        dev_cfg.id.clone(),
                        feat_id.clone(),
                        fc.clone(),
                    );
                    connections.insert(feat_id.clone(), fc);
                    continue;
                }
                let Some(tpl) = backends
                    .iter()
                    .find(|t| t.backend == feat_cfg.backend && t.feature == feat_id.as_str())
                else {
                    warn!(
                        "[{}:{}] no backend '{}' registered, omitting feature",
                        dev_cfg.id, feat_id, feat_cfg.backend
                    );
                    continue;
                };
                let fc = FeatureConnection::new(
                    dev_cfg.id.clone(),
                    feat_id.clone(),
                    feat_cfg.backend.clone(),
                    feat_cfg.clone(),
                );

                // Set up processor runtime for video features at init time.
                if feat_id == "video" {
                    crate::bmc::feature::video::runtime::ensure_runtime(&fc).await;
                }

                spawn_lifecycle(fc.clone(), tpl.connect.clone());

                spawn_feature_event_forwarders(
                    self.event_tx.clone(),
                    dev_cfg.id.clone(),
                    feat_id.clone(),
                    fc.clone(),
                );

                connections.insert(feat_id.clone(), fc);
            }

            let md = Arc::new(
                ManagedDevice::new(dev_cfg.id.clone(), feat_cfgs, connections)
                    .with_source_config(serde_json::to_string(dev_cfg).unwrap_or_default()),
            );
            self.devices.write().await.insert(dev_cfg.id.clone(), md);
        }
        info!(
            "DeviceRegistry initialized with {} devices",
            self.devices.read().await.len()
        );
    }
}
