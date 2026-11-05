# AGENTS.md - BMC Aggregator Architecture

## Architecture Overview

BMC Aggregator is a Rust/Axum aggregator for One-KVM BMC devices. Devices are exposed under `/bmc/<id>/api/...` with feature-owned routes for video, HID, MSD, power, and audio placeholder support.

Current architecture:

```
Client Layer (web/)
  routes.rs + web shell handlers
  mounts feature-owned routes
        |
        v
Feature Layer (bmc/feature/)
  FeatureConnection lifecycle
  typed feature interfaces
  feature runtime such as VideoProcessor
        |
        v
Backend Crates (crates/bmc-backend-*)
  protocol, transport, connection pools, typed feature implementations
        |
        v
Application Adapters (bmc/backends/)
  configuration mapping, lifecycle/event/frame bridging only
```

There is no `BmcManager`, `SessionManager`, `BmcSession`, `SessionBundle`, `connector.rs`, or top-level `bmc/webrtc` module in the current architecture.

## Key Seams

### FeatureConnection

File: `src/bmc/feature/connection.rs`

`FeatureConnection` is a lifecycle handle only. It owns identity, config, state, ref_count, events, wake notification, health flags, and two private runtime slots.

Runtime state must be accessed only through typed accessors:

- `set_backend_runtime()` / `backend_runtime()` / `clear_backend_runtime()`
- `set_feature_runtime()` / `feature_runtime()`

Semantics:

- `backend_runtime` is created by backend `connect(fc)` and cleared on reconnect/disconnect.
- `feature_runtime` is feature-owned state that survives backend reconnects.

Do not reintroduce public `handle`, public `extension`, or a generic `execute()` operation dispatcher.

### Backend Registry

File: `src/bmc/feature/registry.rs`

`FeatureBackendTemplate` registers `(backend, feature, connect)` and optional requirement hooks. `connect(fc)` initializes typed runtime for that feature.

Current registrations live in `src/bmc/feature/mod.rs`:

- `mahogany` -> `video`
- `mahogany` -> `hid`
- `mahogany` -> `msd`
- `mahogany` -> `firmware`
- `vnc` -> `video`
- `vnc` -> `hid`
- `idrac-avct-console` -> `video`
- `idrac-avct-console` -> `hid`
- `idrac-legacy-media` -> `msd`
- `idrac-wsman` -> `boot`
- `ipmi` -> `power`
- `ipmi` -> `boot`
- `ipmi` -> `identify`
- `redfish` -> `power`
- `redfish` -> `msd`
- `redfish` -> `boot`
- `redfish` -> `identify`
- `redfish` -> `firmware`
- `redfish` -> `health`
- `wol` -> `power`

### Feature Status

`build_feature_status()` in `src/bmc/feature/mod.rs` owns the common status base:

- `available`
- `online`
- `state`
- `backend`
- `connection_id`
- `ref_count`

Feature handlers append feature-specific fields from the module that owns them.

## Video Feature

Files:

- `src/bmc/feature/video/mod.rs`
- `src/bmc/feature/video/runtime.rs`
- `src/bmc/feature/video/processor.rs`
- `src/bmc/feature/video/handler.rs`
- `src/bmc/feature/video/http_stream.rs`
- `src/bmc/feature/video/webrtc.rs`
- `src/bmc/feature/video/webtransport.rs`

`VideoProcessor` owns video control-plane and browser-facing stream orchestration:

- stream mode
- encoder backend
- bitrate
- constraints
- encoder self-check
- raw frame fanout
- last-frame replay
- MJPEG encode timing
- WebRTC/WebTransport session maps
- trace snapshot

`VideoSource` is intentionally narrow:

```rust
#[async_trait]
pub trait VideoSource: Send + Sync {
    async fn request_full_frame(&self) -> Result<bool, String>;
    fn trace_snapshot(&self) -> serde_json::Value;
}
```

Backend video adapters must only provide source-side behavior: connect to the BMC, publish raw frames into `VideoRuntime`, request full frames, and expose source trace. Encoder selection, bitrate, constraints, and WebRTC/WebTransport sessions belong to `VideoProcessor`.

Mahogany video adapter: `src/bmc/backends/mahogany/video.rs`.
iDRAC AVCT video adapter: `src/bmc/backends/idrac_avct/video.rs`.

## HID, MSD, Power

Feature-owned typed traits:

- `src/bmc/feature/hid/mod.rs` -> `HidBackend`
- `src/bmc/feature/msd/mod.rs` -> `MsdBackend`
- `src/bmc/feature/power/mod.rs` -> `PowerBackend`

Handlers acquire the feature connection, then read the typed backend adapter from `fc.backend_runtime::<Arc<dyn ...>>()`.

## Route Ownership

`src/web/routes.rs` owns root web shell routes, root auth/setup/system routes, server config routes, static fallback, and device-level endpoints.

Runtime feature routes are mounted by `bmc::feature::mount_all_feature_routes()`:

- `/bmc/:id/api/feature/video/*`
- `/bmc/:id/api/feature/hid/*`
- `/bmc/:id/api/feature/msd/*`
- `/bmc/:id/api/feature/power/*`
- `/bmc/:id/api/feature/audio/*`
- `/bmc/:id/api/feature/boot/*`
- `/bmc/:id/api/feature/identify/*`
- `/bmc/:id/api/feature/firmware/*`
- `/bmc/:id/api/feature/health/*`

Feature config routes are owned by the feature modules. BMC-scoped legacy config URLs (`/bmc/:id/api/config*`) and BMC-scoped root compatibility aliases for auth/setup/system, HTTP stream, ATX discovery, and HID event bus are not mounted.

Canonical route ownership:

- Root web/server/auth/setup/system: `/api/*`
- Device-level info/events/discovery: `/bmc/:id/api/{info,features,features/status,snapshot,health,ws,devices}`
- Feature runtime/config: `/bmc/:id/api/feature/*`

Video input capture config, max FPS, and default encoder live under `features.video.feature_params`. Top-level video config fields may be read for legacy YAML migration only; do not add new callers to them.

Frontend config state is feature-owned by `frontend/src/stores/*Config.ts`. Do not reintroduce a central `frontend/src/stores/config.ts` multiplexer.

## Backend Layer

Stable contracts live in `crates/bmc-feature-api`; protocol-neutral helpers live
in `crates/bmc-backend-support`. Protocol implementations live in independent
workspace crates:

- `bmc-backend-mahogany`, `bmc-backend-vnc`
- `bmc-backend-idrac-avct`, `bmc-backend-idrac-legacy-media`, `bmc-backend-idrac-wsman`
- `bmc-backend-ipmi`, `bmc-backend-redfish`, `bmc-backend-wol`

Application adapters are grouped directly by protocol under `src/bmc/backends/`.
Use `backends::<protocol>::<feature>` module paths, including `ipmi::power`,
`ipmi::boot`, and `ipmi::identify`.

Protocol backend crates must not depend on one another or on `bmc-aggregator`.
They may depend on `bmc-feature-api` and the neutral support crate. Files under
`src/bmc/backends/` are thin application adapters only: they map configuration,
attach backend trait objects to `FeatureConnection`, and bridge backend events or
frames into feature-owned runtime. Do not put wire parsing, sockets, HTTP clients,
TLS, connection pools, or feature trait implementations there.

### iDRAC Pure State Machines

When changing iDRAC connection/session/pool lifecycle, HID, video negotiation or
assembly, media/storage operations, or WS-MAN control flows, read
`docs/adr/0015-pure-idrac-state-machines.md`. Keep state decisions in the
`bmc-idrac-model` crate and consume its state/event-to-state/effect transitions
from backend runtimes. Do not reintroduce mutable phase bookkeeping for these
flows in protocol workers. The model must remain dependency-free and `no_std`;
no I/O, async handlers, ambient clocks, randomness, global state, or external
mutable context is allowed. Time and completion observations are explicit inputs.
Unknown, stale, duplicate, timeout, and terminal events need explicit policies.
Run the pure-model and `idrac_model_boundaries` tests with the Rust quality gate.

The three native iDRAC backend crates may depend on `bmc-idrac-model`. Keep the
complete file inventory in `docs/testing/idrac-state-machine-coverage.md` current.
Unknown remote results must not become success; unimplemented messages must have
an explicit rejection policy. Keep transport epochs on queued input, retain
framing progress across read cancellation, and discard incomplete HTTP exchanges.
Other backends use the protocol-neutral model contract below. The application
FeatureConnection lifecycle is not migrated.
Wire/pixel transformations and resource ownership remain in runtime code.

### Pure State Machines for Other Backends

Mahogany, VNC, Redfish, IPMI and WOL control decisions belong in
`crates/bmc-backend-model`. Read `docs/adr/0016-pure-backend-state-machines.md`
before changing their lifecycle, HID, storage, polling, pagination or operation
confirmation. Apply the same dependency-free, `no_std`, owned state/event purity
contract as the iDRAC model. Keep I/O/resource ownership, codecs, metrics and
stateless field/configuration mapping in runtime crates. Do not add parallel
phase enums or authoritative phase flags to workers.

Keep `docs/testing/backend-state-machine-coverage.md` complete and run
`backend_model_boundaries` with the Rust quality gate. Both model crates use the
shared AST purity checker. Queue admission is not remote input confirmation;
IPMI chassis acceptance is not a completed power transition; WOL dispatch is
not observed device power. Unknown remote fields and task states must never
become success. Keep mutations single-attempt unless a pure model explicitly
permits safe replay; use readback for configuration/media confirmation.

Legacy configuration inference is confined to `DeviceConfig::normalize_feature_config()`.
Runtime registration consumes only explicit `features.<id>.backend` selections and
must not fall back from one backend to another.

## Development Guide

### Add a New Feature Backend

1. Implement the feature's stable trait in a new or existing independent backend crate.
2. Add a thin application adapter under `src/bmc/backends/` that maps configuration and installs the typed runtime.
3. Add one explicit entry to `define_backends!` in `src/bmc/feature/mod.rs`.
4. Add config fields only if the existing `backend_params` / `feature_params` tree is insufficient.

### Add a Video Control

Put the control in `VideoProcessor` first. Only add a method to `VideoSource` when the operation must be executed by the BMC/source adapter, such as requesting a full frame.

### Build And Test

Use the Nix dev shell:

```bash
direnv allow
# or
nix develop
```

Use `just` for repository tasks; do not introduce shell scripts or a separate
`check-quality` entrypoint. `just ci` is the complete local CI command; remote
CI should invoke the same recipes.

Common verification:

```bash
just fmt
just ci
# Or a specific group:
just ci-rust
just ci-frontend
just ci-tools
```

If the existing `target/` has a stale native CMake cache, use:

```bash
CARGO_TARGET_DIR=/tmp/bmc-aggregator-target-clean cargo check
```

## Pending Work

- Auth handlers in `src/web/handlers/auth.rs` are still compatibility stubs.
