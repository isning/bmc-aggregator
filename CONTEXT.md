# CONTEXT.md — BMC Aggregator

## Design Decisions

### StreamOrchestrator
The module that holds the 3 sub-managers (DeviceRegistry, EncoderManager, PipelineManager) and provides only cross-sub-manager coordination. Its interface is 4 methods: `configure_stream`, `webrtc_offer`, `webrtc_candidate`, `webrtc_close`. Single-sub-manager operations (encoder swap, pipeline stop, mode read, power control) access sub-managers directly — StreamOrchestrator is not a pass-through facade.

### Sub-manager
One of 3 lifecycle-owning modules behind StreamOrchestrator: DeviceRegistry (device init, session lookup, keepalive guard storage), EncoderManager (FrameEncoder probe/create/swap), PipelineManager (VideoStreamEncoder pipeline create/stop/mode). Connection management is now handled directly by `ManagedDevice.acquire()` + `SessionGuard` — no separate ConnectionManager.

### Connection State Management
Unified into `ManagedDevice` + `connector.rs` state machine:
- `ManagedDevice.ref_count` (single atomic, replaces old `in_use` + `keep_alive`) prevents idle disconnect
- `SessionGuard` RAII guard increments/decrements `ref_count`
- `connect_task` publishes `ConnectionEvent` (StateChanged, Health) on each transition
- Frontend WebSocket subscribes to `ConnectionEvent` and receives real-time `connection.state_changed` / `connection.health` events
- No polling needed for connection state awareness

### SessionManager
*Deprecated.* Previously a pass-through facade over the 4 sub-managers with 15 methods (13 one-liner delegations). Eliminated in favor of StreamOrchestrator + direct sub-manager access.

### Feature Registry (future)
Each protocol backend declares its capabilities at registration time via a feature list, rather than hardcoding feature presence in handlers. The frontend discovers features per device via `GET /bmc/:id/features`.

```rust
ProtocolBackend {
    id: "mahogany",
    features: &[
        Feature { id: "hid",    config_route: "/feature/hid/config" },
        Feature { id: "msd",    config_route: "/feature/msd/config" },
        Feature { id: "power",  config_route: "/feature/power/config" },
        Feature { id: "video",  config_route: "/feature/video/config" },
    ],
}
```

Capability config endpoints live under `/bmc/:id/feature/{id}/config` and return backend-specific data by checking the active session's trait availability (`BmcInputSession`, `BmcPowerSession`, etc.). Currently the 6 endpoints under `/bmc/:id/api/config/` are hardcoded and ignore `bmc_id` — this is a legacy of having only one protocol backend (mahogany).

### ATX → Power (future)
Rename `atx` routes and module to `power` to match the domain trait `BmcPowerSession` and enum `PowerAction`. `/bmc/:id/api/atx/power` → `/bmc/:id/feature/power/control`, `/bmc/:id/api/atx/status` → `/bmc/:id/feature/power/status`. Frontend updates required in tandem.

### stream_start / stream_stop (implemented)
- `stream_start`: wakes the device connection via `dev.acquire(has_video=true)` + `dev.wait_for_session(30s)`. The transient `SessionGuard` drops when the handler returns — the actual stream consumer (HTTP MJPEG subscriber or WebRTC session) holds its own guard.
- `stream_stop`: tears down WebRTC pipeline via `pipeline.stop_pipeline(id)`. No keepalive manipulation — connection idles out naturally when all guards release.
- `stream_bitrate`: intentionally kept as no-op with TODO. Remove when real flow control is implemented.
- Persistent holds (WebRTC, MSD) store `SessionGuard` in `DeviceRegistry.keepalive_guards`.
