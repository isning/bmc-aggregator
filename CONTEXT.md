# CONTEXT.md - BMC Aggregator

## Architecture

### Three-Layer Model

```
Client Layer (HTTP/WS handlers)
    |  routes are owned by web shell or by each Feature
    v
Feature Layer (Feature + FeatureConnection + runtime)
    |  FeatureConnection: lifecycle only (id, state, ref_count, events)
    |  FeatureGuard: RAII ref_count guard
    |  feature runtime: feature-owned state that survives backend reconnects
    |  backend runtime: typed backend adapter state created by connect()
    v
Backend Layer (source/transport adapters)
    |  Mahogany DevicePool deduplicates TCP by
    |  (host, HTTP port, effective KVM port, username, password fingerprint)
    |  Protocol crates own transport; application adapters map config and bridge events
    |  VNC, Redfish, and iDRAC AVCT own their protocol-specific adapters
```

### FeatureConnection

Lifecycle handle only. It owns `id`, `instance_id`, `device_id`, `backend`, `config`, `state`, `ref_count`, `event_tx` / `event_rx`, `wake`, `healthy`, `lifecycle_started`, and `disconnected`.

It does not expose raw `Any` fields. Runtime state is accessed through:

- `set_feature_runtime()` / `feature_runtime()` for feature-owned state such as `Arc<VideoRuntime>`
- `set_backend_runtime()` / `backend_runtime()` for backend adapters such as `Arc<dyn HidBackend>`

Backend runtime is cleared on reconnect/disconnect. Feature runtime survives reconnects.

### FeatureBackendTemplate

`feature/registry.rs` registers `(backend, feature, connect)` entries with optional requirement hooks through `define_backends!`.

`connect(fc)` initializes backend runtime for the requested feature. There is no generic `execute()` seam in the current code; handlers call typed feature traits (`HidBackend`, `MsdBackend`, `PowerBackend`) or `VideoProcessor` / `VideoSource` directly through feature modules.

### Protocol backends

The registry contains `mahogany`, `vnc`, `idrac-avct-console`, `ipmi`, `wol`,
`idrac-legacy-media`, `idrac-wsman`, and `redfish` entries for their supported features. `idrac-avct-console` is the
native Dell/Avocent console backend for video/HID; it shares an AVCT pool only
between its paired adapters and never falls back to VNC. `redfish` is restricted
to discovered power, boot, identify, MSD, firmware and health capabilities. See `docs/adr/0012-generic-protocol-backends.md`.

### Video Feature

`VideoProcessor` owns video control and browser-facing stream orchestration:

- stream mode and encoder backend
- bitrate and constraints
- encoder self-check response
- raw frame fanout
- WebRTC and WebTransport session maps
- MJPEG encode timing and trace data

`VideoSource` is the source-side seam. A source adapter only feeds raw frames and exposes source-side control:

```rust
pub trait VideoSource: Send + Sync {
    async fn request_full_frame(&self) -> Result<bool, String>;
    fn trace_snapshot(&self) -> serde_json::Value;
}
```

Mahogany implements `VideoSource`. It obtains the feature-owned `VideoRuntime`,
subscribes to the pooled raw-frame channel, and publishes frames into its
`VideoProcessor`. The current source adapter itself remains typed backend
runtime, so it is cleared and recreated on reconnect. Encoding policy, codec
selection, bitrate, constraints, and browser sessions do not belong to the
backend.

### DevicePool

`crates/bmc-backend-mahogany/src/pool.rs` owns the module-static Mahogany TCP pool. `DeviceEntry` wraps the TCP writer, framebuffer, raw-frame broadcast channel, storage server, performance counters, and HID control state. Video, HID, and MSD feature adapters share entries through the pool.

### AppState

`AppState` owns the device registry, configuration/coordinator, MSD storage and
download tasks, and optional WebTransport runtime. See `src/web/state.rs`.

There is no `BmcManager`, `SessionManager`, `BmcSession`, or `SessionBundle`.

### Route Registration

`src/web/routes.rs` owns root web shell routes, root auth/setup/system routes, server config routes, static fallback, and device-level endpoints. `bmc/feature/mod.rs::mount_all_feature_routes()` mounts feature-owned runtime routes under `/bmc/:id/api/feature/*`.

BMC-scoped root compatibility URLs are not mounted for auth/setup/system, HTTP stream, ATX discovery, HID event bus, or config. Canonical routes are root web/server/auth/setup/system under `/api/*`, device-level info/events/discovery under `/bmc/:id/api/{info,features,features/status,snapshot,health,ws,devices}`, and feature runtime/config under `/bmc/:id/api/feature/*`.

Feature config routes live under their owning feature modules. Video input capture config, max FPS, and default encoder are canonical under `features.video.feature_params`; old top-level video fields are load-time compatibility only.

### Frontend Config State

Device settings use `deviceConfiguration.ts` and the backend configuration catalog. Console controls retain feature-owned `videoConfig.ts`, `hidConfig.ts`, and `audioConfig.ts` stores. Global settings own their server request state. There is no central `frontend/src/stores/config.ts` multiplexer.

### Feature Status Interface

Every `GET /feature/:id/status` handler returns a base shape from `build_feature_status()`:

- `available`
- `online`
- `state`
- `backend`
- `connection_id`
- `ref_count`

Feature handlers append feature-specific runtime fields when needed.

## Code Structure

```
bmc/feature/
|-- mod.rs                mount_all_feature_routes(), status helpers
|-- config.rs             FeatureConfig, BackendParams, FeatureParams
|-- config_edit.rs        shared feature config patch/save helpers
|-- connection.rs         FeatureConnection, FeatureGuard, spawn_lifecycle
|-- registry.rs           FeatureBackendTemplate, define_backends! macro
|-- managed.rs            ManagedDevice
|-- device_registry.rs    DeviceRegistry
|-- video/
|   |-- mod.rs            VideoSource seam + routes
|   |-- runtime.rs        VideoRuntime around VideoProcessor; source is backend runtime
|   |-- processor.rs      video control, encoding orchestration, browser sessions
|   |-- handler.rs        video HTTP handlers
|   |-- stream_config.rs  VideoProcessor config and bitrate endpoints
|   |-- ice_config.rs     WebRTC ICE config helpers
|   |-- encoding.rs       encoder selection state
|   |-- http_stream.rs    HTTP MJPEG transport
|   |-- mode.rs           mode endpoints
|   |-- webrtc.rs         WebRTC signaling
|   `-- webtransport.rs   WebTransport session handling
|-- hid/                  HID routes and typed HidBackend
|-- msd/                  MSD routes and typed MsdBackend
|-- power/                power routes and typed PowerBackend
|-- boot/                 provider-owned boot selection
|-- identify/             chassis identification
|-- firmware/             firmware inventory
|-- health/               chassis health and telemetry
`-- audio/                placeholder
```

## Removed Architecture

The old `bmc/session/`, `bmc/connection/`, `bmc/traits.rs`, `bmc/connector.rs`, `bmc/manager.rs`, `bmc/mock_session.rs`, and `bmc/webrtc/` modules are not part of the current architecture.


Pure control models: `bmc-idrac-model` owns native iDRAC flows;
`bmc-backend-model` owns the remaining protocol decisions. Runtime crates execute
bounded I/O and publish observations. See ADR-0015/0016 and both executable source
coverage ledgers under `docs/testing/`. Codecs, configuration mapping, metrics and
resource handles stay in backend shells; FeatureConnection stays feature-owned.
