//! Stable contracts implemented by protocol backend crates.
//!
//! This crate contains no lifecycle, HTTP, configuration persistence, or
//! protocol implementation. Backend crates depend on it; the aggregator
//! adapts implementations into its feature connection lifecycle.

pub mod boot;
pub mod configuration;
pub mod firmware;
pub mod health;
pub mod hid;
pub mod identify;
pub mod msd;
pub mod power;
pub mod requirements;
pub mod video;

pub mod mutation;
