use std::net::Ipv4Addr;
use std::sync::Arc;

use bmc_backend_wol::WolBackend;
use futures::future::BoxFuture;
use tracing::info;

use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::power::PowerBackend;

pub fn connect(fc: Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let broadcast = fc
            .config
            .backend_params
            .broadcast
            .as_deref()
            .unwrap_or("255.255.255.255")
            .parse::<Ipv4Addr>()
            .map_err(|e| format!("invalid WOL broadcast address: {e}"))?;
        let backend = Arc::new(WolBackend::new(broadcast)) as Arc<dyn PowerBackend>;
        fc.set_backend_runtime(backend).await;
        info!("[{}] wol power backend ready", fc.device_id);
        Ok(())
    })
}
