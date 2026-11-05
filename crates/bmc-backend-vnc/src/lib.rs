//! Isolated RFB/VNC backend.

mod configuration;
mod hid;
mod pool;
mod protocol;
mod video;

pub use configuration::configuration_descriptors;
pub use hid::VncHidBackend;
pub use pool::{get_or_connect, VncConfig, VncFrame, VncLease, VncSession};
pub use video::VncVideoBackend;
