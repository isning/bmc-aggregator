//! Pure decisions shared by native protocol shells. Time and observations are inputs.
#![no_std]
#![forbid(unsafe_code)]
#![forbid(clippy::wildcard_enum_match_arm)]
extern crate alloc;
pub mod connection;
pub mod input;
pub mod media;
pub mod task;
pub mod transaction;

pub mod control;
pub mod negotiation;
pub mod pagination;
pub mod storage;
pub mod stream;

pub mod boot;
