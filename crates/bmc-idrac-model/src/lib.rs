//! Deterministic iDRAC decision models. Runtime work is represented as effects.
#![no_std]
#![forbid(unsafe_code)]
#![forbid(clippy::wildcard_enum_match_arm)]

extern crate alloc;
// Declare finite phase sets once; matrix tests automatically include added states.
macro_rules! finite_states {
    ($(#[$attribute:meta])* $visibility:vis enum $name:ident { $($(#[$variant_attribute:meta])* $variant:ident),* $(,)? }) => {
        $(#[$attribute])* $visibility enum $name { $($(#[$variant_attribute])* $variant),* }
        #[cfg(test)]
        impl $name { const ALL: &'static [Self] = &[$(Self::$variant),*]; }
    };
}

pub mod assembly;
pub mod avct;
pub mod framing;
pub mod hid;
pub mod management;
pub mod media;
pub mod operation;
pub mod pool;
pub mod recovery;
pub mod session;
pub mod storage;
pub mod wsman;

#[must_use]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transition<S, E> {
    pub state: S,
    pub effect: E,
}
