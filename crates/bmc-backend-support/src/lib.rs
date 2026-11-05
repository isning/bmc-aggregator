//! Protocol-neutral building blocks shared by backend crates.
//!
//! This crate must not depend on the aggregator or on any backend crate.

pub mod endpoint;
pub mod http_digest;
pub mod removable_image;
