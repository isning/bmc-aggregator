use futures::future::BoxFuture;

use crate::bmc::encoder::backend::SharedEncoder;
use crate::bmc::session::types::DeviceSession;
use crate::config::DeviceConfig;

/// A registered transport template for a protocol backend.
pub struct ConnectionTemplate {
    pub id: &'static str,
    pub name: &'static str,
    /// Feature IDs this transport provides (e.g. ["video", "hid", "msd"])
    pub features: &'static [&'static str],
    pub connect: fn(
        cfg: &DeviceConfig,
        encoder: SharedEncoder,
        progress: &crate::bmc::connection::progress::ConnectProgress,
    ) -> BoxFuture<'static, Result<DeviceSession, anyhow::Error>>,
}

/// Compute the set of all feature IDs available across all registered backends.
pub fn all_registered_features() -> Vec<&'static str> {
    use crate::bmc::connector::PROTOCOL_BACKENDS;
    let mut features: Vec<&str> = Vec::new();
    for backend in PROTOCOL_BACKENDS {
        for ct in backend.connections {
            for f in ct.features {
                if !features.contains(f) {
                    features.push(f);
                }
            }
        }
    }
    features
}
