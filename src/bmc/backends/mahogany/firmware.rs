use std::sync::atomic::Ordering;
use std::sync::Arc;

use bmc_backend_mahogany::{MahoganyFirmwareFeature, PoolKey, POOL};
use bmc_feature_api::firmware::FirmwareBackend;
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
            .map_err(|error| format!("mahogany firmware connect: {error}"))?;
        let entry = lease.entry().clone();
        let disconnected = entry.subscribe_disconnect();
        let mut changed = entry.subscribe_firmware();
        fc.set_backend_runtime(
            Arc::new(MahoganyFirmwareFeature::new(lease)) as Arc<dyn FirmwareBackend>
        )
        .await;

        let event_fc = fc.clone();
        let event_generation = fc.backend_generation();
        tokio::spawn(async move {
            while event_fc
                .wait_for_backend_update(event_generation, &mut changed)
                .await
            {
                crate::bmc::feature::firmware::handler::publish_backend_inventory(
                    &event_fc,
                    event_generation,
                    "backend_event",
                )
                .await;
            }
        });
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
