pub mod hid;
pub mod video;

use crate::bmc::feature::config::BackendParams;
use bmc_backend_vnc::VncConfig;

fn config(params: &BackendParams) -> Result<VncConfig, String> {
    if params.vnc_tls.unwrap_or(false) {
        return Err("VNC TLS is not implemented; refusing insecure fallback".into());
    }
    let port = params
        .vnc_port
        .ok_or_else(|| "vnc_port is required for the vnc backend".to_string())?;
    VncConfig::new(
        params.host.clone(),
        port,
        params
            .vnc_password
            .clone()
            .or_else(|| params.password.clone()),
        params.vnc_security.as_deref(),
    )
}
