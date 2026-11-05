//! IPMI RMCP+ feature backend implementations.

mod boot;
mod client;
mod configuration;
mod identify;
mod power;

pub use boot::IpmiBootBackend;
pub use configuration::configuration_descriptors;
pub use identify::IpmiIdentifyBackend;
pub use power::IpmiPowerBackend;

#[derive(Clone, Debug)]
pub struct IpmiConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}
