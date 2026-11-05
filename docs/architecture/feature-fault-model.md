# Unified Feature Fault Model

## Objective

Every BMC feature exposes one sanitized, structured fault contract across:

- Feature lifecycle and backend connection;
- Feature runtime operations;
- HTTP responses, status endpoints, WebSocket events, and trace snapshots;
- Browser UI retry/terminal-failure behavior.

The model applies to video, HID, MSD, power, and future audio. It excludes
root server/auth/setup errors, which have their own ownership boundaries.

## Ownership

```text
FeatureConnection
  owns FeatureFaultStore for the feature instance
  owns lifecycle transitions and retry wake-up

Feature backend/runtime
  maps backend/protocol errors to FeatureFault drafts
  never constructs HTTP or UI responses

Feature handler
  serializes FeatureFault snapshots and maps disposition to HTTP status

Feature-specific runtime
  may maintain scoped faults (e.g. video per browser transport,
  MSD per download task), but publishes them through FeatureFaultStore
```

`FeatureConnection` is the only owner of the active feature fault. This keeps
one connection instance's fault independent from every other instance and
avoids a global manager or generic operation dispatcher.

## Core types

```rust
enum FeatureFaultDomain {
    Lifecycle, Transport, Protocol, Operation, Storage,
    Decode, Encoder, BrowserTransport,
}

enum FeatureFaultScope {
    Feature,
    Operation { operation: &'static str },
    Child { kind: &'static str, id: String },
}

struct FeatureFault {
    domain: FeatureFaultDomain,
    code: &'static str,
    message: String,
    retryable: bool,
    scope: FeatureFaultScope,
    occurred_at_ms: u64,
    count: u64,
}
```

`code` is a stable, public identifier. `message` is operator-readable and
sanitized. It must not contain credentials, token/session material, raw
video/HID data, SDP/ICE, remote image URLs with credentials, or raw backend
responses. Backend-only detail remains in structured tracing, also redacted.

## State and retry

```text
Disconnected -> Connecting -> Connected
      ^             |            |
      |             v            v
      +--- Retryable fault <--- transport loss
                    |
                    +--- Terminal fault (explicit retry/config edit only)
```

- A successful backend connect clears the active feature-scoped fault.
- A retryable lifecycle fault follows the configured reconnect backoff.
- A non-retryable fault preserves status and is not cleared by the retry API.
- An operation/child fault never disconnects a healthy feature unless the
  backend explicitly reports transport loss.
- `POST /feature/<feature>/retry` clears retryable feature faults and wakes only
  that FeatureConnection. It never retries terminal faults automatically.
- `reset` remains feature-specific; it must not silently claim to repair a
  connection fault.

## Standard public response

All feature runtime failures use this JSON envelope:

```json
{
  "success": false,
  "fault": {
    "domain": "protocol",
    "code": "apcp_rejected",
    "message": "The BMC closed the AVCT setup connection",
    "retryable": true,
    "scope": "feature"
  }
}
```

HTTP status is derived mechanically:

| Fault class | HTTP status |
|---|---|
| invalid user request | 400 |
| capability unavailable / terminal config | 409 / 501 |
| device absent | 404 |
| retryable source/backend failure | 502 |
| connection not yet established | 503 |
| bounded operation timeout | 504 |

Feature status always includes `fault: FeatureFault | null`. Trace includes a
bounded history: 16 feature faults and 8 faults per child scope.

## Domains and initial code registry

| Feature | Domain | Stable codes |
|---|---|---|
| All | lifecycle | `connect_timeout`, `backend_unavailable`, `feature_reconfigured`, `source_disconnected` |
| Video | protocol/decoder/encoder/browser_transport | `apcp_rejected`, `tls_handshake_failed`, `login_denied`, `session_in_use`, `frame_decode_failed`, `encoder_unavailable`, `encode_failed`, `first_frame_timeout`, `webrtc_negotiation_failed`, `webtransport_write_failed` |
| HID | transport/operation | `hid_backend_disconnected`, `hid_write_failed`, `unsupported_hid_operation` |
| Power | protocol/operation | `power_status_failed`, `power_action_failed`, `unsupported_power_action` |
| MSD | storage/protocol/operation | `image_not_found`, `download_failed`, `media_insert_failed`, `media_eject_failed`, `storage_unavailable` |
| Audio | lifecycle | `feature_unavailable` |

The registry is closed over public codes; a backend cannot surface arbitrary
new public error strings without a reviewed mapping.

## Feature-specific scope

### Video

`VideoProcessor` does not own a parallel public fault model. It reports
`Decode`, `Encoder`, and `BrowserTransport` child faults into the associated
connection store. Browser transport faults use `Child { kind: "webrtc" |
"webtransport" | "mjpeg", id }`, so one browser does not poison another.

### HID

Each keyboard/mouse/consumer request reports an operation-scoped fault only
when the backend send fails. A successful subsequent input clears the active
operation fault; it does not clear a lifecycle fault.

### Power

Status polling records a retryable `power_status_failed`; control operations
record `power_action_failed` with the requested action omitted from any
untrusted backend message. Unsupported actions are terminal capability faults.

### MSD

Download failures are child-scoped by download ID. Insert/eject failures are
operation-scoped. Managed-image path validation remains a 400 request error,
not a backend fault.

### Audio

The current stub exposes one terminal `feature_unavailable` fault until an
actual backend is registered.

## API and event contract

- `GET /bmc/:id/api/features/status` returns the active fault for every
  feature instance.
- `POST /bmc/:id/api/feature/<feature>/retry` is mounted by each feature module
  through a shared helper, preserving route ownership.
- Runtime handlers return `FeatureFaultResponse`; they never directly format
  backend errors.
- Device WebSocket emits `feature.fault_changed` with feature,
  instance, and the public fault snapshot.
- Frontend feature stores subscribe to status/event updates and show a retry
  action only when `retryable` is true.

## Migration plan

1. Add `feature/fault.rs`, attach `FeatureFaultStore` to `FeatureConnection`,
   and add lifecycle fault recording in `spawn_lifecycle`.
2. Update common feature status and the shared fault response/status mapper.
3. Migrate video to the feature store and retain only per-transport scope in
   `VideoProcessor`.
4. Migrate HID handlers and backend send boundaries.
5. Migrate power status/actions and IPMI/Redfish maps.
6. Migrate MSD operations/download tasks and Redfish media maps.
7. Convert audio stub and add fault event/frontend store support.
8. Remove `last_connect_error`, raw backend `*_error` status fields, and unstructured
   handler `(StatusCode, String)` returns.

## Acceptance criteria

- Every feature status has `fault: null | FeatureFault`.
- No runtime handler returns an unstructured backend error string.
- A source failure ends loading UI rather than leaving a spinner indefinitely.
- A child-scoped video/MSD failure does not mark the feature offline.
- A successful retry clears only the relevant retryable fault.
- Contract tests assert no public fault text contains a configured credential,
  session token, or raw protocol payload.
