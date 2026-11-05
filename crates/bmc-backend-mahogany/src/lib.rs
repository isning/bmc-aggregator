//! Isolated Mahogany/One-KVM backend.

mod auth;
mod configuration;
mod firmware;
mod framebuffer;
mod hid;
mod msd;
mod perf;
mod pool;
mod protocol;
mod storage_server;
mod video;

pub use configuration::configuration_descriptors;
pub use firmware::MahoganyFirmwareFeature;
pub use hid::MahoganyHidBackend;
pub use msd::MahoganyMsdFeature;
pub use pool::{ConnParams, DeviceEntry, MahoganyFrame, PoolKey, PoolLease, POOL};
pub use video::MahoganyVideoBackend;
