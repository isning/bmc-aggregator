# ADR-0010: Three-Layer Architecture with Feature Layer

**Status**: Accepted
**Date**: 2026-06-06
**Updated**: 2026-06-18
**Architecture doc**: `ARCHITECTURE.md`
**Context doc**: `CONTEXT.md`

## Context

The v1 prototype used a device-level session model:

```
HTTP handler -> BmcManager -> SessionManager -> DeviceSession
```

That model coupled all features on a device to one lifecycle and one session trait family. HID activity could keep video alive, IPMI had to fit a session-shaped interface even though power operations are stateless, and video processing state lived beside transport/session state.

The current project needs independent feature lifecycles, backend transport reuse, and a clear seam between feature control-plane logic and backend source/transport adapters.

## Decision

Adopt a three-layer architecture:

```
Client Layer
  HTTP/WS handlers, web shell routes, root auth/setup/system/server/device routes

Feature Layer
  Feature routes, FeatureConnection lifecycle, typed feature interfaces,
  feature-owned runtime such as VideoProcessor

Backend Workspace Crates
  stable feature API, neutral support, independent protocol/transport crates

Application Backend Adapters
  configuration mapping and lifecycle/event/frame bridging
```

`FeatureConnection` is a lifecycle handle only. It exposes typed runtime accessors:

- `set_backend_runtime()` / `backend_runtime()` / `clear_backend_runtime()`
- `set_feature_runtime()` / `feature_runtime()`

`FeatureBackendTemplate` registers only `(backend, feature, connect)`. The connect function initializes typed runtime:

- `Arc<dyn HidBackend>`
- `Arc<dyn MsdBackend>`
- `Arc<dyn PowerBackend>`
- `Arc<dyn VideoSource>` in backend runtime; `VideoRuntime` stays feature-owned

There is no generic `execute()` seam. Handlers call typed feature interfaces or `VideoProcessor`.

Protocol implementations are isolated in `crates/bmc-backend-*`. These crates do
not depend on one another or on the application crate. `src/bmc/backends/` contains
only thin adapters, and runtime registration consumes explicit feature backend
configuration without cross-backend fallback. Compatibility defaults are
materialized once while normalizing legacy configuration.

Route ownership is explicit:

- Root web/server/auth/setup/system routes use `/api/*`.
- Device-level info/events/discovery use `/bmc/:id/api/{info,features,features/status,snapshot,health,ws,devices}`.
- Feature runtime/config routes use `/bmc/:id/api/feature/*`.

BMC-scoped root compatibility aliases for auth/setup/system, HTTP stream, ATX discovery, HID event bus, and config are not mounted.

## Video Amendment

Video control-plane belongs to `VideoProcessor`, not to the backend.

`VideoProcessor` owns:

- stream mode
- encoder backend
- bitrate
- constraints response
- encoder self-check response
- raw-frame fanout
- last-frame replay
- WebRTC/WebTransport session maps
- trace data

`VideoSource` is intentionally narrow:

```rust
#[async_trait]
pub trait VideoSource: Send + Sync {
    async fn request_full_frame(&self) -> Result<bool, String>;
    fn trace_snapshot(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}
```

This keeps source-side backend operations separate from browser-facing encoding policy.

Persistent video input capture config, max FPS, and default encoder live in `features.video.feature_params`. Top-level video config fields are load-time compatibility for old YAML only.

## Consequences

### Positive

- Feature lifecycle is independent: video, HID, MSD, and power do not share one ref_count.
- Backend transport reuse is still possible through backend-owned pools such as Mahogany `DevicePool`.
- The Feature layer has better locality: handlers call small typed interfaces rather than an operation string dispatcher.
- Video has higher leverage: bitrate, encoder, constraints, and sessions are testable through `VideoProcessor` without a live BMC backend.
- Backend modules no longer need to expose encoding/control-plane methods for video.

### Negative

- Each feature has its own lifecycle task, so a device may have several mostly-idle tasks.
- Runtime accessors still use `Any` internally, but the public seam is now typed accessors rather than public raw fields.
- A new feature still requires adding a route mount and a backend registry entry.

## Current Module Map

| Module | Responsibility |
|--------|----------------|
| `bmc/feature/connection.rs` | `FeatureConnection`, `FeatureGuard`, lifecycle state machine |
| `bmc/feature/registry.rs` | `FeatureBackendTemplate`, `define_backends!` |
| `bmc/feature/device_registry.rs` | Device/feature initialization and feature instances |
| `bmc/feature/video/processor.rs` | Video control-plane and stream session state |
| `bmc/feature/video/runtime.rs` | Feature-owned `VideoProcessor`; current `VideoSource` is backend runtime |
| `bmc/feature/video/mod.rs` | `VideoSource` and video route mount |
| `bmc/feature/hid` | HID routes and `HidBackend` trait |
| `bmc/feature/msd` | MSD routes and `MsdBackend` trait |
| `bmc/feature/power` | Power routes and `PowerBackend` trait |
| `crates/bmc-backend-mahogany/src/pool.rs` | Shared Mahogany TCP connection pool |
| `bmc/backends/mahogany/video.rs` | Mahogany `VideoSource` adapter |
| `bmc/backends/mahogany/hid.rs` | Mahogany `HidBackend` adapter |
| `bmc/backends/mahogany/msd.rs` | Mahogany `MsdBackend` adapter |
| `bmc/backends/ipmi/power.rs` | IPMI `PowerBackend` adapter |
| `bmc/backends/wol.rs` | WOL `PowerBackend` adapter |

## Replaced Architecture

These v1 seams are obsolete:

- `BmcManager`
- `SessionManager`
- `BmcSession` / `BmcInputSession` / `BmcPowerSession` / `BmcMsdSession`
- `PROTOCOL_BACKENDS`
- generic `connect + execute`
- public `FeatureConnection.handle`
- public `FeatureConnection.extension`

## Future Work

- Consider typed wrapper structs for `backend_runtime` and `feature_runtime` if more feature runtime types appear.
