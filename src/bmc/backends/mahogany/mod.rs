pub mod firmware;
pub mod hid;
pub mod msd;
pub mod video;
mod video_forwarder;

use crate::bmc::feature::connection::FeatureConnection;
use bmc_backend_mahogany::ConnParams;

fn params(fc: &FeatureConnection, max_fps: u32) -> ConnParams {
    let bp = &fc.config.backend_params;
    ConnParams {
        id: fc.device_id.clone(),
        host: bp.host.clone(),
        port: bp.port.unwrap_or(443),
        kvm_port: bp.kvm_port,
        username: bp.username.clone().unwrap_or_else(|| "admin".into()),
        password: bp.password.clone().unwrap_or_default(),
        max_fps,
    }
}
