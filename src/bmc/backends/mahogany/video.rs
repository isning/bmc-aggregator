use std::sync::atomic::Ordering;
use std::sync::Arc;

use bmc_backend_mahogany::{MahoganyVideoBackend, PoolKey, POOL};
use bmc_feature_api::video::VideoSource;
use futures::future::BoxFuture;
use tracing::info;

use super::{params, video_forwarder::forward_raw_frames};
use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::video::runtime;

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let params = params(&fc, fc.config.feature_params.max_fps.unwrap_or(30));
        let key = PoolKey::from_params(&params);
        let lease = POOL
            .get_or_connect(&key, &params)
            .await
            .map_err(|error| format!("mahogany video connect: {error}"))?;
        let entry = lease.entry().clone();
        let frames = entry.raw_frame_tx.subscribe();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(forward_raw_frames(
            frames,
            shutdown_rx,
            runtime::ensure_runtime(&fc).await,
        ));
        entry.request_full_frame().await;
        let disconnected = entry.subscribe_disconnect();
        fc.set_backend_runtime(
            Arc::new(MahoganyVideoBackend::new(lease, shutdown_tx)) as Arc<dyn VideoSource>
        )
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
        info!("[{}] video connected", fc.device_id);
        Ok(())
    })
}
