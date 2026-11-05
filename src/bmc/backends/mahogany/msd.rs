use std::sync::atomic::Ordering;
use std::sync::Arc;

use bmc_backend_mahogany::{MahoganyMsdFeature, PoolKey, POOL};
use bmc_feature_api::msd::MsdBackend;
use futures::future::BoxFuture;

use super::params;
use crate::bmc::feature::connection::FeatureConnection;

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let params = params(&fc, 30);
        let key = PoolKey::from_params(&params);
        let lease = POOL
            .get_or_connect(&key, &params)
            .await
            .map_err(|error| format!("mahogany msd connect: {error}"))?;
        let entry = lease.entry().clone();
        let disconnected = entry.subscribe_disconnect();
        let status_changed = entry.subscribe_msd_status();
        fc.set_backend_runtime(Arc::new(MahoganyMsdFeature::new(lease)) as Arc<dyn MsdBackend>)
            .await;
        tokio::spawn(forward_status_updates(
            fc.clone(),
            fc.backend_generation(),
            status_changed,
        ));
        let fc2 = fc.clone();
        let backend_generation = fc.backend_generation();
        tokio::spawn(async move {
            let Some(_current) = fc2
                .wait_for_backend_disconnect(backend_generation, disconnected)
                .await
            else {
                return;
            };
            POOL.disconnect(&key, &entry);
            fc2.disconnected.store(true, Ordering::SeqCst);
            fc2.wake.notify_one();
        });
        Ok(())
    })
}

async fn forward_status_updates(
    fc: Arc<FeatureConnection>,
    generation: u64,
    mut changed: tokio::sync::watch::Receiver<u64>,
) {
    while fc.wait_for_backend_update(generation, &mut changed).await {
        crate::bmc::feature::msd::handler::publish_backend_status(&fc, generation, "backend_event")
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shared_pool_status_observer_retires_on_backend_replacement_or_stop() {
        for stop in [true, false] {
            let fc = FeatureConnection::new(
                "device".into(),
                "msd".into(),
                "test".into(),
                crate::bmc::feature::config::FeatureConfig {
                    backend: "test".into(),
                    backend_params: Default::default(),
                    feature_params: Default::default(),
                    providers: vec![],
                },
            );
            let (sender, changes) = tokio::sync::watch::channel(0u64);
            let observer = forward_status_updates(fc.clone(), fc.backend_generation(), changes);
            tokio::pin!(observer);
            assert!(futures::poll!(&mut observer).is_pending());
            if stop {
                fc.shutdown();
            } else {
                fc.clear_backend_runtime().await;
            }
            tokio::time::timeout(std::time::Duration::from_secs(1), observer)
                .await
                .unwrap();
            assert_eq!(sender.receiver_count(), 0);
        }
    }
}
