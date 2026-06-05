/// Backend module hierarchy.
///
/// Single-function backends live in their own submodule (`msd`, `ipmi`).
/// Multi-function backends (e.g. `mahogany`: video capture + HID + MSD)
/// share a `general` submodule.
pub mod msd;
pub mod general;
