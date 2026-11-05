//! Dell iDRAC 6/7/8 Avocent (AVCT) console backend.
//!
//! AVCT is intentionally isolated from the generic RFB/VNC backend.  Its
//! legacy TLS policy, APCP preface and proprietary display/HID packets are
//! Dell/Avocent-specific and must never weaken shared HTTP or VNC transports.

pub mod hid;
pub mod video;

use crate::bmc::feature::config::BackendParams;
use bmc_backend_idrac_avct::{IdracAvctConfig, IdracAvctOptions};

fn config(params: &BackendParams) -> Result<IdracAvctConfig, String> {
    if params
        .avct_tls_mode
        .as_deref()
        .unwrap_or("idrac-legacy-rsa")
        != "idrac-legacy-rsa"
    {
        return Err("iDRAC AVCT requires avct_tls_mode=idrac-legacy-rsa".into());
    }
    IdracAvctConfig::new(IdracAvctOptions {
        host: params.host.clone(),
        port: params.avct_port.unwrap_or(5900),
        username: params.username.clone().unwrap_or_default(),
        password: params.password.clone().unwrap_or_default(),
        generation: params.idrac_generation.unwrap_or(8),
        virtual_media_privilege: params.avct_virtual_media_privilege.unwrap_or(false),
        apcp_selector: params.avct_apcp_selector.unwrap_or(5),
        reconnect_capability: params.avct_reconnect_capability.unwrap_or(false),
        reconnect_version: params.avct_reconnect_version.unwrap_or(-1),
        reconnect_preference: params.avct_reconnect_preference.unwrap_or(0),
    })
}
