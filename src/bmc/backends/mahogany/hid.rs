use std::sync::atomic::Ordering;
use std::sync::Arc;

use bmc_backend_mahogany::{MahoganyHidBackend, PoolKey, POOL};
use bmc_feature_api::hid::HidBackend;
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
            .map_err(|error| format!("mahogany hid connect: {error}"))?;
        let entry = lease.entry().clone();
        let disconnected = entry.subscribe_disconnect();
        let control = entry.clone();
        tokio::spawn(async move {
            let _ = control
                .wait_for_keyboard_control(std::time::Duration::from_secs(5))
                .await;
        });
        fc.set_backend_runtime(Arc::new(MahoganyHidBackend::new(lease)) as Arc<dyn HidBackend>)
            .await;
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
