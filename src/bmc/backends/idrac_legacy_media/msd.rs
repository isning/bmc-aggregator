//! Application lifecycle adapter for Dell legacy Virtual Media.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use bmc_backend_idrac_legacy_media::{IdracLegacyMediaBackend, IdracLegacyMediaConfig};
use bmc_feature_api::msd::{MsdBackend, MsdTargetStore};
use futures::future::BoxFuture;
use tracing::info;

use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::fault::{FeatureFault, FeatureFaultDomain};

fn mark_disconnected(
    fc: &Arc<FeatureConnection>,
    generation: u64,
    record_fault: bool,
) -> tokio::task::JoinHandle<()> {
    let fc = fc.clone();
    tokio::spawn(async move {
        let Ok(_current) = fc.backend_effect_guard(generation).await else {
            return;
        };
        fc.healthy.store(false, Ordering::SeqCst);
        fc.disconnected.store(true, Ordering::SeqCst);
        if record_fault {
            fc.faults
                .record(FeatureFault::feature(
                    FeatureFaultDomain::Transport,
                    "legacy_media_session_ended",
                    "The iDRAC legacy virtual-media session ended",
                    true,
                ))
                .await;
        }
        fc.wake.notify_waiters();
    })
}

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let params = &fc.config.backend_params;
        if params
            .avct_tls_mode
            .as_deref()
            .unwrap_or("idrac-legacy-rsa")
            != "idrac-legacy-rsa"
        {
            return Err("iDRAC virtual-media requires avct_tls_mode=idrac-legacy-rsa".into());
        }
        let config = IdracLegacyMediaConfig::new(
            params.host.clone(),
            params.avct_vm_port.or(params.avct_port).unwrap_or(5900),
            params.username.clone().unwrap_or_default(),
            params.password.clone().unwrap_or_default(),
            params.idrac_generation.ok_or_else(|| {
                "iDRAC virtual-media requires explicit idrac_generation: 6 or 8".to_string()
            })?,
            params.avmp_heartbeat_interval_s,
            params.avmp_reconnect_method,
        )?;
        let target_registry = crate::bmc::feature::msd::feature_runtime(&fc)
            .await
            .targets
            .clone();
        let targets: Arc<dyn MsdTargetStore> = Arc::new(target_registry);
        let generation = fc.backend_generation();
        let disconnected = Arc::downgrade(&fc);
        let recycled = Arc::downgrade(&fc);
        let backend = IdracLegacyMediaBackend::connect(
            config,
            targets,
            Arc::new(move || {
                if let Some(fc) = disconnected.upgrade() {
                    drop(mark_disconnected(&fc, generation, true));
                }
            }),
            Arc::new(move || {
                if let Some(fc) = recycled.upgrade() {
                    drop(mark_disconnected(&fc, generation, false));
                }
            }),
        )
        .await?;
        fc.healthy.store(true, Ordering::SeqCst);
        fc.set_backend_runtime(backend as Arc<dyn MsdBackend>).await;
        info!("[{}] iDRAC legacy virtual-media connected", fc.device_id);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_media_callbacks_cannot_disconnect_or_fault_a_replacement() {
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
        fc.set_backend_runtime(Arc::new(())).await;
        let old = fc.backend_generation();
        fc.clear_backend_runtime().await;
        fc.set_backend_runtime(Arc::new(())).await;
        fc.healthy.store(true, Ordering::SeqCst);
        mark_disconnected(&fc, old, true).await.unwrap();
        assert!(fc.healthy.load(Ordering::SeqCst));
        assert!(!fc.disconnected.load(Ordering::SeqCst));
        assert!(fc.faults.active().await.is_none());
        mark_disconnected(&fc, fc.backend_generation(), true)
            .await
            .unwrap();
        assert!(fc.disconnected.load(Ordering::SeqCst));
        assert_eq!(
            fc.faults.active().await.unwrap().code,
            "legacy_media_session_ended"
        );
    }
}
