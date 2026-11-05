//! Isolated Dell/Avocent legacy Virtual Media protocol implementation.

pub mod apcp;
mod configuration;
mod msd;
pub mod protocol;
pub mod session;

pub use configuration::configuration_descriptors;
pub use msd::IdracLegacyMediaBackend;
pub use protocol::{AvmpGenerationProfile, ServerImageSpace};
pub use session::{
    dynamic_target_capacity, IdracLegacyMediaConfig, LegacyMediaSession, MediaStatus,
};
