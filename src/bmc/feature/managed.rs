use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::bmc::feature::config::FeatureConfigs;
use crate::bmc::feature::connection::{FeatureConnection, FeatureGuard};
use crate::bmc::feature::video::runtime::VideoRuntime;

pub struct ManagedDevice {
    pub id: String,
    /// Serialized source configuration used by the registry to retain devices
    /// whose runtime-relevant configuration did not change during a reload.
    pub source_config: Option<String>,
    pub cfg: FeatureConfigs,
    pub features: HashMap<String, Arc<FeatureConnection>>,
    pub feature_instances: RwLock<HashMap<String, Arc<FeatureConnection>>>,
}

impl ManagedDevice {
    pub fn new(
        id: String,
        cfg: FeatureConfigs,
        features: HashMap<String, Arc<FeatureConnection>>,
    ) -> Self {
        let feature_instances = features
            .iter()
            .map(|(feature_id, fc)| (instance_key(feature_id, &fc.instance_id), fc.clone()))
            .collect();
        Self {
            id,
            source_config: None,
            cfg,
            features,
            feature_instances: RwLock::new(feature_instances),
        }
    }

    pub fn with_source_config(mut self, source_config: String) -> Self {
        self.source_config = Some(source_config);
        self
    }

    pub fn acquire(&self, feature_id: &str) -> Result<FeatureGuard, String> {
        let fc = self
            .features
            .get(feature_id)
            .ok_or_else(|| format!("feature '{feature_id}' not found on device '{}'", self.id))?;
        Ok(FeatureGuard::new(fc.clone()))
    }

    pub async fn get_instance(
        &self,
        feature_id: &str,
        instance_id: &str,
    ) -> Option<Arc<FeatureConnection>> {
        self.feature_instances
            .read()
            .await
            .get(&instance_key(feature_id, instance_id))
            .cloned()
    }

    pub async fn insert_instance(
        &self,
        feature_id: &str,
        instance_id: &str,
        fc: Arc<FeatureConnection>,
    ) -> Arc<FeatureConnection> {
        let mut instances = self.feature_instances.write().await;
        instances
            .entry(instance_key(feature_id, instance_id))
            .or_insert_with(|| fc.clone())
            .clone()
    }

    pub(crate) async fn get_or_insert_instance_with(
        &self,
        feature_id: &str,
        instance_id: &str,
        fc: Arc<FeatureConnection>,
        initialize: impl FnOnce(&Arc<FeatureConnection>),
    ) -> Result<(Arc<FeatureConnection>, bool), String> {
        let mut instances = self.feature_instances.write().await;
        let key = instance_key(feature_id, instance_id);
        if let Some(existing) = instances.get(&key) {
            return Ok((existing.clone(), false));
        }
        if instances
            .values()
            .filter(|current| current.id == feature_id)
            .count()
            >= 64
        {
            return Err("feature instance limit reached".into());
        }
        initialize(&fc);
        instances.insert(key, fc.clone());
        Ok((fc, true))
    }

    pub async fn remove_instance(
        &self,
        feature_id: &str,
        instance_id: &str,
    ) -> Option<Arc<FeatureConnection>> {
        self.feature_instances
            .write()
            .await
            .remove(&instance_key(feature_id, instance_id))
    }

    /// A delayed lifecycle callback may only retire the instance it owned.
    pub(crate) async fn remove_instance_if_same(
        &self,
        expected: &Arc<FeatureConnection>,
    ) -> Option<Arc<FeatureConnection>> {
        let key = instance_key(&expected.id, &expected.instance_id);
        let mut instances = self.feature_instances.write().await;
        if instances
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
        {
            instances.remove(&key)
        } else {
            None
        }
    }

    pub async fn instances_for_feature(&self, feature_id: &str) -> Vec<Arc<FeatureConnection>> {
        let prefix = format!("{feature_id}:");
        self.feature_instances
            .read()
            .await
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(_, fc)| fc.clone())
            .collect()
    }

    /// Stop all tasks and feature-owned sessions before dropping a device from
    /// the registry. Existing request guards may outlive this call, so the FC
    /// itself owns the terminal stopped flag.
    pub async fn shutdown(&self) {
        let instances: Vec<_> = self
            .feature_instances
            .read()
            .await
            .values()
            .cloned()
            .collect();

        Self::shutdown_instances(&self.id, instances).await;
    }

    pub async fn shutdown_feature(&self, feature_id: &str) {
        Self::shutdown_instances(&self.id, self.instances_for_feature(feature_id).await).await;
    }

    async fn shutdown_instances(device_id: &str, instances: Vec<Arc<FeatureConnection>>) {
        // Publish the terminal boundary before awaiting any transport cleanup.
        // In particular, active MJPEG/WebRTC consumers must observe `stopped`
        // so their own tasks can release the resources cleanup is waiting for.
        for fc in &instances {
            fc.shutdown();
        }
        for fc in instances {
            if fc.id == "video" {
                if let Some(runtime) = fc.feature_runtime::<Arc<VideoRuntime>>().await {
                    if tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        runtime.processor().close_all_webrtc_connections(),
                    )
                    .await
                    .is_err()
                    {
                        tracing::warn!(
                            %device_id,
                            feature = %fc.id,
                            "timed out closing WebRTC sessions during device shutdown"
                        );
                    }
                }
            }
            fc.clear_backend_runtime().await;
        }
    }
}

fn instance_key(feature_id: &str, instance_id: &str) -> String {
    format!("{feature_id}:{instance_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmc::feature::config::FeatureConfig;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn instance_lookup_waits_for_initialization_before_publication() {
        let device = Arc::new(ManagedDevice::new(
            "device".into(),
            FeatureConfigs {
                features: HashMap::new(),
            },
            HashMap::new(),
        ));
        let fc = FeatureConnection::new_instance(
            "device".into(),
            "video".into(),
            "browser".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let writer = tokio::spawn({
            let device = device.clone();
            async move {
                device
                    .get_or_insert_instance_with("video", "browser", fc, move |fc| {
                        entered.send(()).unwrap();
                        wait.recv_timeout(Duration::from_secs(2)).unwrap();
                        fc.lifecycle_started.store(true, Ordering::SeqCst);
                    })
                    .await
                    .unwrap()
            }
        });
        ready.await.unwrap();
        let mut reader = tokio::spawn({
            let device = device.clone();
            async move { device.get_instance("video", "browser").await.unwrap() }
        });
        let hidden = tokio::time::timeout(Duration::from_millis(30), &mut reader)
            .await
            .is_err();
        release.send(()).unwrap();
        let (inserted, fresh) = writer.await.unwrap();
        let observed = reader.await.unwrap();
        assert!(
            hidden,
            "lookup must not expose an incompletely initialized instance"
        );
        assert!(fresh && observed.lifecycle_started.load(Ordering::SeqCst));
        assert!(Arc::ptr_eq(&inserted, &observed));
    }
}
