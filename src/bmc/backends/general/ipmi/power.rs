use std::sync::Arc;

use futures::future::BoxFuture;

use crate::bmc::backends::general::ipmi::IpmiSession;
use crate::bmc::connection::progress::ConnectProgress;
use crate::bmc::encoder::backend::SharedEncoder;
use crate::bmc::session::types::DeviceSession;
use crate::config::DeviceConfig;

pub fn ipmi_power_connect(
    cfg: &DeviceConfig,
    _encoder: SharedEncoder,
    _progress: &ConnectProgress,
) -> BoxFuture<'static, Result<DeviceSession, anyhow::Error>> {
    let host = cfg.ipmi_host.clone().unwrap_or_else(|| cfg.host.clone());
    let port = cfg.ipmi_port.unwrap_or(623);
    let username = cfg.auth.username.clone();
    let password = cfg.auth.password.clone();
    Box::pin(async move {
        // IPMI is stateless — each operation opens its own session.
        // The IpmiSession is just a factory that stores credentials.
        let session = IpmiSession::new(host, port, username, password);
        Ok(DeviceSession {
            base: None,
            input: None,
            power: Some(Arc::new(session) as Arc<dyn crate::bmc::traits::BmcPowerSession>),
            msd: None,
        })
    })
}
