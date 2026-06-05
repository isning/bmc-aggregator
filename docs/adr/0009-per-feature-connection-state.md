# ADR-0009: Per-Feature Connection State Management

## Status

Accepted

## Context

The original design used a monolithic `SessionBundle` + `DeviceState` per device. A single
`connect_task` managed the entire connection lifecycle, and a single `SessionGuard` kept the
whole connection alive. This created two problems:

1. **Coarse granularity**: HID or MSD operations kept the video stream alive, and vice
   versa. A user opening the HID panel would trigger the full KVM TCP handshake even if
   they never looked at the video.

2. **Rigid feature mapping**: All sessions (`BmcSession`, `BmcInputSession`, `BmcMsdSession`,
   `BmcPowerSession`) were bundled together. Adding a new feature required touching every
   backend, and a backend that only provided power (IPMI) had to return stubs for video/HID/MSD.

3. **Single event bus**: Connection events (state changes, health, progress) were published
   on a device-level broadcast channel. Frontend components couldn't subscribe to just the
   features they cared about.

Separately, different backends provide different feature sets on different transports:
- Mahogany KVM TCP provides video + HID + MSD over one socket
- IPMI provides power control over RMCP+ (stateless, no persistent connection)
- Future backends might provide video-only (VNC), or power-only (WoL), or audio-only

## Decision

Replace the monolithic session/state model with a **per-feature connection state** model.

### Concepts

- **Feature** — a single capability identified by a string ID (`"video"`, `"hid"`,
  `"msd"`, `"power"`). Each feature has its own `FeatureConnection` holding:
  - `ref_count` — atomic count of active users (incremented by `FeatureGuard`)
  - `state` — `ConnectionState` (Disconnected / Connecting / Connected / Disconnecting)
  - `event_tx` — broadcast channel for per-feature connection events
  - `transport` — weak reference to the owning `TransportState`

- **Transport** — a network connection that provides one or more features.
  Mahogany KVM TCP is one transport providing `["video", "hid", "msd"]`.
  IPMI RMCP+ would be a separate transport providing `["power"]`.

- **TransportState** — runtime state per transport instance:
  - `session` — the live `DeviceSession` (set when connected, cleared on disconnect)
  - `connected` — watch channel signalling connection state
  - `wake_tx` — watch channel for waking the `connect_task`
  - `ref_count` — aggregate across all features on this transport

- **FeatureGuard** — RAII guard returned by `acquire(feature_id)`. Increments the
  feature's `ref_count` (and by extension keeps its transport alive). On drop,
  decrements `ref_count` and wakes the transport's `connect_task`.

### Architecture

```
ManagedDevice
├── features: HashMap<String, Arc<FeatureConnection>>   # per-feature state
└── transports: Vec<Arc<TransportState>>                 # per-transport instances

DeviceRegistry::init(configs)
  → for each device config:
    1. Look up ProtocolBackend by config.protocol
    2. For each ConnectionTemplate in backend:
       a. Create TransportAdapter wrapping the template's connect fn
       b. Create TransportState
       c. For each feature in template:
          - Create FeatureConnection with Weak ref to TransportState
    3. Create ManagedDevice with features + transports
    4. Spawn connect_task per transport (background loop)

connect_task(ts, features, cfg, encoder)
  loop {
    WAITING:   sleep until any feature's ref_count > 0
    CONNECTING: call transport.connect(), report stages via ConnectProgress → ConnectionEvent
    CONNECTED:  health-check loop; disconnect when all features' ref_counts are 0 + idle timeout
    DISCONNECT: clear session, publish events, close sessions
  }
```

### Protocol Backend Registration

Backends declare their transports and features via `ConnectionTemplate`:

```rust
pub struct ConnectionTemplate {
    pub id: &'static str,          // transport identifier
    pub features: &[&'static str], // feature IDs this transport provides
    pub connect: fn(...) -> BoxFuture<...>,  // connect function
}
```

Registered in `PROTOCOL_BACKENDS`:

```rust
pub(crate) static PROTOCOL_BACKENDS: &[ProtocolBackend] = &[
    ProtocolBackend {
        id: "mahogany",
        connections: &[
            ConnectionTemplate { id: "kvm",   features: &["video", "hid", "msd"],  ... },
            ConnectionTemplate { id: "power", features: &["power"],                 ... },
        ],
    },
];
```

### Handler API

```rust
// Old (monolithic):
let (guard, bundle) = orchestrator.acquire_session(id, has_video).await?;
bundle.base.info().await;

// New (per-feature):
let guard = orchestrator.acquire(id, "video").await?;
let session = orchestrator.wait_for(id, "video", timeout).await?;
session.base.unwrap().info().await;
```

## Consequences

**Positive:**

- **Independent lifecycle**: HID operations don't keep the video stream alive. Each
  feature's ref_count independently controls its transport. When all features on a
  transport are released, the transport disconnects after a configurable idle timeout.

- **Composable backends**: A backend can provide any subset of features. Adding a
  power-only backend (IPMI) requires no stubs for video/HID/MSD.

- **Per-feature events**: Frontend components subscribe to connection events for
  only the features they display. Progress events (connection stages) are reported
  per-feature.

- **Multiple transports per device**: A device can have a KVM TCP transport (video+hid+msd)
  and a separate IPMI transport (power), each with independent connection state.

- **Feature discovery at runtime**: Frontend queries `features_for_protocol(protocol)` to
  learn which features are available, instead of hardcoding.

**Negative:**

- **More types**: `FeatureConnection`, `FeatureGuard`, `TransportState`, `TransportAdapter`,
  `DeviceSession` (replacing `SessionBundle`). More boilerplate in the connection layer.

- **Lifetime complexity**: `FeatureConnection` holds a `Weak<TransportState>` to break
  the reference cycle. `TransportState` owns the session. `ManagedDevice` holds both.

- **API surface growth per feature**: Instead of one `wait_for_session(id, timeout)`,
  callers now specify which feature: `wait_for(id, "video", timeout)`.

**Neutral:**

- The `connect_task` state machine is essentially the same as the old
  per-device connect task, just parameterized by transport instead of device.
- `ConnectionEvent` replaces the old `ConnectionEvent` with per-feature routing
  (`StateChanged { feature, state }` instead of just `StateChanged(state)`).
