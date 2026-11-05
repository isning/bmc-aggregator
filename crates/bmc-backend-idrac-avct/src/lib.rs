//! Isolated Dell/Avocent AVCT console backend.

mod apcp;
mod avct;
mod block_video;
mod configuration;
mod decoder;
mod display;
mod dvc;
mod hid;
mod input;
mod login;
mod pool;
mod video;

pub use configuration::configuration_descriptors;
pub use decoder::DecodedFrame;
pub use hid::IdracAvctHidBackend;
pub use pool::{
    get_or_connect, AvctLease, AvctSession, AvctTerminalReason, AvctVideoState, IdracAvctConfig,
    IdracAvctOptions,
};
pub use video::IdracAvctVideoBackend;
