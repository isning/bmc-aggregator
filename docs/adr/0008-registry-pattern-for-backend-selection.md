# ADR-0008: Registry Pattern for Backend Selection

**Status**: Accepted

## Context

The codebase has several seams where a string ID selects a concrete adapter:

| Seam | Registry |
|------|----------|
| HTTP frame encoding | `FRAME_ENCODER_BACKENDS` |
| WebRTC stream encoding | `STREAM_ENCODER_BACKENDS` |
| Feature backend connect | `FeatureBackendTemplate` entries via `define_backends!` |

Central `match` factories make adapter addition shallow and force unrelated code to change.

## Decision

Use static registry arrays for selection seams.

Each entry carries the data that seam needs:

- `id`
- `name`
- seam-specific construction or probe logic

Selection is a linear scan. For the current number of adapters, this keeps the seam simple and local.

Current feature registry lives in `src/bmc/feature/mod.rs` and uses:

```rust
define_backends!(
    "mahogany" => "video" => mahogany_video,
    "mahogany" => "hid" => mahogany_hid,
    "mahogany" => "msd" => mahogany_msd,
    "ipmi" => "power" => ipmi_power,
    "wol" => "power" => wol_power,
)
```

## Consequences

- Adapter addition stays local to the seam.
- Registry discovery is straightforward for frontend capability listings.
- The registry pattern does not require a global service locator.

## Protocol backend extension policy

For a standards-based transport, prefer one protocol-named backend shared by
all vendors that implement the relevant capability. ADR-0012 proposes `vnc`
for video/HID and `redfish` for discovered power/MSD capabilities; these are
not current registry entries until their implementation and tests land.

A vendor-specific backend is justified only for a demonstrated OEM protocol or
extension that cannot be expressed through the standard backend. It must be
registered per feature, not used as a device-wide service locator, and its
configuration must not make unsupported feature controls appear live.
