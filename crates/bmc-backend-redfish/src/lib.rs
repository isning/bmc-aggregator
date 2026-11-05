//! Vendor-neutral Redfish backend implementations.

mod boot;
mod client;
mod configuration;
mod firmware;
mod health;
mod identify;
mod msd;
mod power;

pub use client::RedfishClient;
pub use configuration::configuration_descriptors;
pub use firmware::RedfishFirmwareBackend;
pub use health::RedfishHealthBackend;
pub use identify::RedfishIdentifyBackend;
pub use msd::RedfishMsdBackend;
pub use power::RedfishPowerBackend;

#[derive(Clone, Debug)]
pub struct RedfishConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub insecure_tls: bool,
}
pub use boot::RedfishBootBackend;
