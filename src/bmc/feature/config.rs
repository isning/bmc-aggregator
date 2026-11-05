use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReconnectConfig {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_base_delay")]
    pub base_delay_s: u32,
    #[serde(default = "default_max_delay")]
    pub max_delay_s: u32,
}

fn default_max_attempts() -> u32 {
    0
}
fn default_base_delay() -> u32 {
    1
}
fn default_max_delay() -> u32 {
    30
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            max_attempts: 0,
            base_delay_s: 1,
            max_delay_s: 30,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FeatureParams {
    #[serde(default)]
    pub max_fps: Option<u32>,
    #[serde(default)]
    pub encoder_backend: Option<String>,
    #[serde(default)]
    pub video_device: Option<String>,
    #[serde(default)]
    pub video_format: Option<String>,
    #[serde(default)]
    pub video_width: Option<u32>,
    #[serde(default)]
    pub video_height: Option<u32>,
    #[serde(default)]
    pub video_fps: Option<u32>,
    #[serde(default)]
    pub video_quality: Option<u32>,
    #[serde(default)]
    pub reconnect: ReconnectConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BackendParams {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub kvm_port: Option<u16>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub broadcast: Option<String>,
    #[serde(default)]
    pub vnc_port: Option<u16>,
    #[serde(default)]
    pub vnc_password: Option<String>,
    #[serde(default)]
    pub vnc_security: Option<String>,
    #[serde(default)]
    pub vnc_tls: Option<bool>,
    /// Dell/Avocent APCP/AVCT console port.  This is intentionally separate
    /// from the generic VNC port and defaults to iDRAC's documented 5900.
    #[serde(default)]
    pub avct_port: Option<u16>,
    /// Dell/Avocent AVMP virtual-media port. This stays distinct from the
    /// console AVCT port because the appliance commonly exposes VM on 2068.
    #[serde(default)]
    pub avct_vm_port: Option<u16>,
    /// Narrow TLS policy selector for the OEM AVCT backend.  The only
    /// currently supported value is `idrac-legacy-rsa`.
    #[serde(default)]
    pub avct_tls_mode: Option<String>,
    /// Request the AVCT virtual-media privilege bit used by Dell's direct
    /// console client. This affects only the AVCT login packet.
    #[serde(default)]
    pub avct_virtual_media_privilege: Option<bool>,
    /// Source-backed launcher setting `CAPABILITY_RECONNECT`.  Absence uses
    /// the official non-ENABLE branch (the 53-byte initial APCP request).
    #[serde(default)]
    pub avct_reconnect_capability: Option<bool>,
    /// Source-backed launcher setting `RECONNECT_VERSION`.  It is consulted
    /// only when `avct_reconnect_capability` is true.
    #[serde(default)]
    pub avct_reconnect_version: Option<i16>,
    /// APCP selector passed to the official `ProtocolAPCP` constructor.  The
    /// official iDRAC8 KVM client default is 5.
    #[serde(default)]
    pub avct_apcp_selector: Option<u32>,
    /// Source-backed reconnect preference written by the extended `0x0100`
    /// APCP writer. It is not a video or VNC setting.
    #[serde(default)]
    pub avct_reconnect_preference: Option<u16>,
    /// Explicit OEM generation profile for protocols whose official iDRAC6
    /// and iDRAC8 wire formats differ. iDRAC7 remains unsupported until an
    /// official client artifact establishes its distinct branch.
    #[serde(default)]
    pub idrac_generation: Option<u8>,
    /// Optional AVMP heartbeat override for source-profiled native media.
    /// iDRAC6 has a fixed, always-enabled 10-second idle heartbeat in the
    /// official Java VM client; only `10` is accepted when set for that
    /// profile. A positive value enables AVMP `0x0400` for other profiles.
    #[serde(default)]
    pub avmp_heartbeat_interval_s: Option<u32>,
    /// Native iDRAC8 VM launcher reconnect method.  Only 0, 1 and 2 are
    /// source-confirmed; it selects the APCP setup selector, not a retry
    /// count. iDRAC6 ignores this setting because its official client has no
    /// reconnect scheduler.
    #[serde(default)]
    pub avmp_reconnect_method: Option<u8>,
    #[serde(default)]
    pub redfish_insecure_tls: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeatureConfig {
    pub backend: String,
    #[serde(default)]
    pub backend_params: BackendParams,
    #[serde(default)]
    pub feature_params: FeatureParams,
    #[serde(default)]
    pub providers: Vec<FeatureProviderConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeatureProviderConfig {
    pub id: String,
    pub backend: String,
    pub backend_params: BackendParams,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeatureConfigs {
    #[serde(flatten)]
    pub features: HashMap<String, FeatureConfig>,
}
