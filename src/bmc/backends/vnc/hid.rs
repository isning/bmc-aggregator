use std::sync::atomic::Ordering;
use std::sync::Arc;

use bmc_backend_vnc::{get_or_connect, VncHidBackend};
use bmc_feature_api::hid::HidBackend;
use tracing::info;

use super::config;
use crate::bmc::feature::connection::FeatureConnection;

pub async fn connect(fc: Arc<FeatureConnection>) -> Result<(), String> {
    let lease = get_or_connect(&config(&fc.config.backend_params)?).await?;
    let session = lease.session().clone();
    fc.set_backend_runtime(Arc::new(VncHidBackend::new(lease)) as Arc<dyn HidBackend>)
        .await;
    let fc_for_disconnect = fc.clone();
    let disconnect = session.disconnected_rx();
    let backend_generation = fc.backend_generation();
    tokio::spawn(async move {
        if let Some(_current) = fc_for_disconnect
            .wait_for_backend_disconnect(backend_generation, disconnect)
            .await
        {
            fc_for_disconnect.disconnected.store(true, Ordering::SeqCst);
            fc_for_disconnect.wake.notify_one();
            info!(
                "[{}] VNC HID backend disconnected",
                fc_for_disconnect.device_id
            );
        }
    });
    info!("[{}] VNC HID connected", fc.device_id);
    Ok(())
}
