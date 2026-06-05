# AGENTS.md — BMC Aggregator Architecture

## Architecture Overview

A Rust (Axum) aggregator that proxies multiple One-KVM BMC devices under path-based routing (`/bmc/<id>/`), providing a unified web UI with video streaming, HID input, MSD, and power control.

```
┌──────────────────────────────────────────────────────────┐
│                     CLI (main.rs)                        │
│  clap parse → Config::load() → BmcManager → axum listen  │
└────────────────────┬─────────────────────────────────────┘
                     │
           ┌─────────┴──────────┐
           ▼                    ▼
┌──────────────────┐  ┌──────────────────────┐
│   WEB LAYER      │  │  BMC DOMAIN LAYER    │
│  web/            │  │  bmc/                │
│  ├── routes.rs   │  │  ├── manager.rs      │
│  └── handlers/   │  │  ├── connector.rs    │
│                   │  │  ├── session/       │
│                   │  │  ├── traits.rs      │
│                   │  │  ├── encoder/       │
│                   │  │  ├── backends/      │
│                   │  │  ├── stream/        │
│                   │  │  └── webrtc/        │
│                   │  └──────────────────────┘
│                   │
│  ┌──────────────────────┐
│  │  CONFIG LAYER        │
│  │  config.rs           │
│  └──────────────────────┘
└──────────────────────────────────────────────────────────┘
```

### Data Flow: HTTP Request → Response

```
HTTP Request
  → routes.rs (axum Router, path-based dispatch)
    → handlers/*.rs (extract State, call BmcManager)
      → manager.rs (BmcManager facade)
        → session/manager.rs (StreamOrchestrator)
          → ManagedDevice.acquire() + wait_for_session()
            → PROTOCOL_BACKENDS → mahogany/ (BMC TCP connection)
```
HTTP Request
  → routes.rs (axum Router, path-based dispatch)
    → handlers/*.rs (extract State, call BmcManager)
      → manager.rs (BmcManager facade)
        → session/manager.rs (SessionManager)
          → PROTOCOL_BACKENDS → mahogany/ (BMC TCP connection)
```

### Data Flow: Video Frame → Browser

```
BMC Device (TCP framebuffer stream)
  → backends/general/mahogany/framebuffer.rs (raw RGBA)
    → FRAME_ENCODER_BACKENDS → encoder/backend.rs [HTTP MJPEG]
      → web/http_stream.rs (HTTP chunked response)
    OR
    → STREAM_ENCODER_BACKENDS → stream/encoder.rs [WebRTC]
      → stream/pipeline.rs (broadcast channel)
        → webrtc/session.rs (WebRTC track)
```

---

## Module Reference

### `src/main.rs`
**Entry point.** Parses CLI args (`--config`), loads config, creates `BmcManager`, builds Axum app, starts listener.

### `src/config.rs`
**Config layer.** All serde structs: `Config`, `ServerConfig`, `DeviceConfig`, `AuthConfig`, `MsdDeviceConfig`. YAML-based with `${ENV_VAR}` substitution support. Saving writes back to file (config persistence).

### `src/web/`

| Module | Responsibility |
|--------|---------------|
| `routes.rs` | Axum `Router` with ~60 routes. Path-based under `/bmc/:id/`. Auth routes are stubs (pending real implementation). |
| `handlers/` | HTTP handlers split by domain. Each file owns one functional area. |
| `handlers/device.rs` | Device info, health, stream status/codecs/mode, snapshot, encoder self-check |
| `handlers/stream.rs` | Stream start/stop/constraints/bitrate |
| `handlers/config.rs` | Config get/patch for stream, HID, video, ATX, MSD, auth, audio, web, generic |
| `handlers/msd.rs` | MSD status, connect/disconnect, set_image, images CRUD, drive, upload/download |
| `handlers/webrtc.rs` | WebRTC offer, ICE candidates, close, ICE servers, pipeline encoder |
| `handlers/auth.rs` | Auth check, login, logout, change-password — **stubs**, always return success |
| `handlers/atx.rs` | ATX power control, power status |
| `handlers/hid.rs` | HID device status |
| `handlers/misc.rs` | System info, extensions, setup status, fallback; event WebSocket subscribes to `ConnectionEvent` from `ManagedDevice` and forwards as `connection.state_changed` / `connection.health` to frontend |
| `http_stream.rs` | MJPEG streaming endpoint — reads from `BmcSession::subscribe_framebuffer()`, encodes with `FrameEncoder`, writes HTTP chunked response |

### `src/bmc/` — Domain Layer

#### `manager.rs`
**BmcManager — facade for all BMC operations.** Owned by `AppState`. Methods delegate to `SessionManager`, handles cross-cutting concerns (WebRTC session lifecycle, pipeline management, keepalive). Key types:

- `DeviceState` — per-device runtime state: `session`, `ipmi`, `encoder`, `pipeline`, `webrtc_sessions`, `encoder_error`
- `SessionBundle` — bundles `BmcSession` + `BmcInputSession` + `BmcMsdSession` into one Arc

#### `connector.rs`
**Connection lifecycle.** Backend selection via `PROTOCOL_BACKENDS` registry array. `connect_bmc()` iterates the registry to find the protocol adapter and create a `DeviceState`. `connect_task()` is a state machine: `WAITING → CONNECTING → CONNECTED → IDLE → DISCONNECT → WAITING`. Background tokio task per device.

#### `session/`
- `managed.rs` — **Core connection state management**:
  - `ConnectionState` enum: `Disconnected`, `Connecting`, `Connected`, `Disconnecting`
  - `ConnectionEvent` enum: `StateChanged(state)`, `Health { online, fps, width, height }` — published via `broadcast::Sender` on `ManagedDevice`
  - `ManagedDevice`: per-device state holder (`cfg`, `encoder`, `state`, `ref_count`/`video_refs` atomics, `idle_since`, `idle_timeout`, `wake_tx`, `event_tx`)
  - `SessionGuard` (unified RAII guard, replacing old `InUseGuard` + `KeepAliveGuard`): increments `ref_count` (and `video_refs` if `has_video`) on creation, decrements on drop, wakes `connect_task`
  - `ref_count` replaces separate `in_use` + `keep_alive` atomics — any consumer that wants to prevent idle disconnect increments it
  - Methods: `acquire(has_video)`, `wait_for_session(timeout)`, `subscribe()` (connection events), `publish(event)` (called by `connect_task`)
- `manager.rs` — `StreamOrchestrator` facade: no more `ConnectionManager`, delegates to `ManagedDevice.acquire()` + `wait_for_session()` directly
- `registry.rs` — `DeviceRegistry`: HashMap of `ManagedDevice`, init/inject/query, + `keepalive_guards` HashMap for WebRTC/MSD persistent holds, + `subscribe_connection_events()`
- `encoder_manager.rs` — `EncoderManager`: single authority for `FrameEncoder` switching (HTTP MJPEG backend)
- `pipeline_manager.rs` — `PipelineManager`: create/switch/stop streaming video pipelines (`VideoStreamEncoder`), no encoder-switching logic

#### `traits.rs`
**4 focused traits** defining the session contract:

| Trait | Methods | Owner |
|-------|---------|-------|
| `BmcSession` | `subscribe_framebuffer()`, `info()`, `close()`, `fps()`, `set_pipeline()` | All sessions |
| `BmcInputSession` | `send_keyboard()`, `send_mouse()` | HID-capable backends |
| `BmcPowerSession` | `power_control()`, `power_status()` | IPMI backends |
| `BmcMsdSession` | `msd_status()`, `msd_connect()`, `msd_disconnect()`, `msd_set_image()` | MSD-capable backends |

Key types: `DeviceInfo`, `PowerAction`, `MsdStatusInfo`.

#### `error.rs`
`SessionError` enum (thiserror). Wraps `EncoderError`, I/O errors, protocol errors, timeouts.

#### `encoder/` — Single-Frame Video Encoding (HTTP MJPEG)
**Used by HTTP MJPEG streaming.** Takes raw RGBA, produces one encoded frame. Stateless.

| File | Content |
|------|---------|
| `error.rs` | `EncoderError` enum |
| `backend.rs` | `FrameEncoder` trait + `FRAME_ENCODER_BACKENDS` registry (software, webp) + `SharedEncoder` |
| `codec.rs` | `Codec` enum — single source for ALL codec variants (H264, H265, VP8, VP9, AV1, Mjpeg, WebP) |
| `jpeg.rs` | JPEG encoder: `turbojpeg` BGRA path → `image` crate fallback |
| `webp.rs` | WebP encoder via `image` crate |
| `registry.rs` | `EncoderRegistry::discover()` iterates `FRAME_ENCODER_BACKENDS`; `codecs()` lists stream codec metadata |

Key types:
- `trait FrameEncoder` — `fn encode(&self, pixels: &[u8], width: u16, height: u16, quality: u8) -> Result<Vec<u8>>`
- `enum Codec` — unified across both encoder systems
- `type SharedEncoder = Arc<RwLock<Box<dyn FrameEncoder>>>` — runtime-swappable

#### `backends/` — Protocol Backend Implementations

```
backends/
├── general/                    ← Multi-function backends
│   ├── mahogany/               ← KVM over TCP (video + HID + MSD)
│   │   ├── mod.rs              → pub use MahoganySession
│   │   ├── connection.rs       ← TCP connect, HTTP digest auth, main KVM loop
│   │   ├── protocol.rs         ← KVM message constants + parse_payload
│   │   ├── framebuffer.rs      ← Framebuffer blit, resize, format conversion
│   │   ├── hid.rs              ← HID scancode → KVM protocol encoding
│   │   └── msd.rs              ← M2.JAR MSD protocol over KVM TCP (protocol only)
│   └── ipmi/                   ← IPMI power + sensor (RMCP+)
│       ├── mod.rs
│       ├── power.rs            ← Power control (on/off/cycle), chassis status
│       └── sensor.rs           ← (future: temperature, fan, voltage)
│
└── msd/                        ← Single-function backends
    ├── mod.rs
    └── storage_server.rs       ← SCSI-over-TCP storage server (MahoganyStorageServer)
```

**`general/mahogany/`** — The primary KVM capture backend. Connects to One-KVM devices over TCP using the Mahogany protocol. Implements `BmcSession` (video), `BmcInputSession` (HID), and `BmcMsdSession` (MSD over KVM TCP). Connection is a single TCP socket with multiplexed message types (framebuffer updates, HID events, MSD protocol). The SCSI storage server was extracted into `backends/msd/storage_server.rs`.

**`general/ipmi/`** — Standalone IPMI RMCP+ backend. Opens a new IPMI session per operation (stateless). Implements `BmcPowerSession`. Independent of any video capture protocol. Currently provides power control; designed to also host sensor monitoring.

**`msd/`** — Houses the SCSI-over-TCP storage server (`MahoganyStorageServer`), extracted from `general/mahogany/msd.rs`. The MSD protocol encode/decode remains in `general/mahogany/msd.rs`.

#### `stream/` — Streaming Video Pipeline (for WebRTC)
**Used by WebRTC streaming.** Stateful encoder pipeline with broadcast distribution.

| File | Content |
|------|---------|
| `encoder.rs` | `VideoStreamEncoder` trait + `STREAM_ENCODER_BACKENDS` registry (`SoftwareFfmpegEncoder`, `NvencFfmpegEncoder`) + `FfmpegCore` (shared encode state) |
| `pipeline.rs` | `SharedVideoPipeline` — `broadcast::Sender<EncodedFrame>` + mutex-guarded encoder + `push_frame()` / `subscribe()` |

Key distinction from `encoder/`:
| | `encoder/` (FrameEncoder) | `stream/` (VideoStreamEncoder) |
|---|---|---|
| Use | HTTP MJPEG | WebRTC |
| State | Stateless (`&self`) | Stateful (`&mut self`) |
| Codecs | Mjpeg, WebP | H264, H265, VP8, VP9, AV1 |
| Output | Single `Vec<u8>` | Multiple `Vec<Vec<u8>>` (packets) |
| Backend selection | `FRAME_ENCODER_BACKENDS` registry | `STREAM_ENCODER_BACKENDS` registry |

#### `webrtc/` — WebRTC Signaling

| File | Content |
|------|---------|
| `config.rs` | `WebRtcConfig` (ICE servers, STUN) |
| `signaling.rs` | `WebRtcOfferRequest`, `WebRtcOfferResponse`, `IceCandidate` |
| `session.rs` | `WebRtcSession` — SDP offer/answer, ICE, track management, close (orchestration inlined into `SessionManager`) |
| `video_track.rs` | WebRTC video track — reads from `SharedVideoPipeline` broadcast |

---

## Key Decisions (ADRs)

### ADR-1: Event-driven Encoder Switching via SharedEncoder
- **Status**: Accepted
- **Context**: Encoder backend must switch at runtime (MJPEG → WebP) without restarting the stream or reconnecting to BMC.
- **Decision**: `type SharedEncoder = Arc<RwLock<Box<dyn FrameEncoder>>>`. Encode task reads the RwLock per frame. `set_encoder_runtime()` replaces the inner encoder.
- **Tradeoff**: Per-frame RwLock read is negligible overhead vs encode time. Simpler than channel-based hot-swap.

### ADR-2: Two Separate Encoder Traits (FrameEncoder vs VideoStreamEncoder)
- **Status**: Accepted
- **Context**: HTTP MJPEG needs stateless single-frame encoding. WebRTC needs stateful multi-packet streaming.
- **Decision**: Keep them separate. `encoder/backend.rs` for HTTP, `stream/encoder.rs` for WebRTC. Each has its own registry array.
- **Tradeoff**: Duplicated codec concepts but each trait stays focused.

### ADR-3: Session Trait Split (4 traits, not 1 monolith)
- **Status**: Accepted
- **Context**: Different backends support different capabilities (mahogany does HID+MSD+video, IPMI does power only).
- **Decision**: `BmcSession` (core) + `BmcInputSession` + `BmcPowerSession` + `BmcMsdSession`. Each has default no-op methods where sensible. `SessionBundle` bundles them.
- **Tradeoff**: More traits to manage, but avoids fat trait with `unimplemented!()` stubs.

### ADR-4: Lazy Connection with RAII Guards (Unified SessionGuard)
- **Status**: Accepted
- **Context**: Don't drain BMC device resources. Connect on demand, disconnect after idle timeout.
- **Decision**: Single `SessionGuard` type for all uses (transient and persistent). `connect_task` waits in WAITING state until `ref_count > 0`, then transitions through CONNECTING → CONNECTED → IDLE → DISCONNECT. State transitions are published as `ConnectionEvent` for frontend/backend consumers.
- **Tradeoff**: Slight latency on first access (connect + handshake). Exponential backoff retry on failure.

### ADR-5: Mahogany MSD over KVM TCP (not separate connection)
- **Status**: Accepted
- **Context**: M2.JAR protocol defines MSD messages (0x89/0x99/0x9A) flowing over the same KVM TCP connection.
- **Decision**: MSD protocol state machine lives in `general/mahogany/msd.rs`. SCSI storage server was extracted to `backends/msd/storage_server.rs`.
- **Tradeoff**: Single TCP socket simplifies deployment but couples MSD lifecycle to KVM connection.

### ADR-6: IPMI as Stateless Session (open-close per operation)
- **Status**: Accepted
- **Context**: IPMI RMCP+ sessions are lightweight but have timeout.
- **Decision**: Each power operation opens a new IPMI session, performs the action, closes. No persistent connection.
- **Tradeoff**: Slight latency per operation (RMCP+ handshake). Resilient to network interruptions.

### ADR-7: `backends/general/` for Multi-Function Backends
- **Status**: Accepted
- **Context**: Some backends provide multiple capabilities (mahogany: video+HID+MSD; IPMI: power+sensor).
- **Decision**: `general/` hosts backends implementing ≥2 traits. Single-function backends live in their own directory under `backends/`.
- **Tradeoff**: — 

### ADR-8: Registry Pattern for Backend Selection
- **Status**: Accepted
- **Context**: The system has multiple backend selection points mapping string IDs to concrete adapters. Match-based factories require editing central functions when adding adapters, violating Open-Closed.
- **Decision**: Use static registry arrays for all backend seams. Each entry carries `id`, `name`, and seam-specific function pointers (`create`, `probe`). Selection is `iter().find(|b| b.id == id)`.
- **Seams migrated**: `FRAME_ENCODER_BACKENDS` (HTTP JPEG), `STREAM_ENCODER_BACKENDS` (WebRTC), `PROTOCOL_BACKENDS` (connect_bmc).
- **Tradeoff**: Linear scan (negligible for ≤5 entries). `fn` pointers cannot capture environment.

---

## Development Guide

### Adding a New Device Backend
1. Create `backends/general/<name>/` (multi-function) or `backends/<name>/` (single-function)
2. Implement relevant trait(s) from `traits.rs`
3. Add a `ProtocolBackend` entry to `PROTOCOL_BACKENDS` in `connector.rs`
4. Add any new config fields to `config.rs::DeviceConfig`
5. Register routes if new API endpoints needed

### Adding a New Frame Encoder (HTTP MJPEG)
1. Create `encoder/<name>.rs`
2. Implement `FrameEncoder` trait
3. Add entry to `FRAME_ENCODER_BACKENDS` in `encoder/backend.rs`
4. Add availability probe logic

### Adding a New Stream Encoder (WebRTC)
1. Implement `VideoStreamEncoder` trait (can share `FfmpegCore` or be independent)
2. Add entry to `STREAM_ENCODER_BACKENDS` in `stream/encoder.rs`

### Adding a New Codec
1. Add variant to `Codec` enum in `encoder/codec.rs` (single source)
2. Add entry to `EncoderRegistry::codecs()` in `encoder/registry.rs` (metadata)
3. Add encoder name/pixel format/options in the relevant adapter's config function

### Development Environment (Nix + direnv)

This project uses a Nix flake for reproducible dev environments. The `.envrc` at project root enables automatic shell activation when you `cd` into the directory (requires `direnv`):

```bash
direnv allow                   # Activate dev shell (first time only)
```

Or enter the shell manually:

```bash
nix develop                    # Enter dev shell with all dependencies
```

The flake (`flake.nix`) provides `ffmpeg.dev`, `clang` (for `libclang` / bindgen), `pkg-config`, `nasm`, and `glibc.dev`. All required environment variables (`LIBCLANG_PATH`, `LD_LIBRARY_PATH`, `C_INCLUDE_PATH`, `PKG_CONFIG_PATH`) are set automatically.

### Build & Test

Run these commands inside the dev shell (`nix develop` or via direnv):

```bash
cargo build                    # Debug build
cargo build --release          # Release build
cargo test                     # All tests (unit + integration + e2e)
cargo clippy                   # Lint
```

### Config File
```yaml
server:
  listen: "0.0.0.0:8000"
  frontend_dir: "./frontend"
devices:
  - id: server1
    name: "Server 1"
    protocol: mahogany
    host: 192.168.1.100
    port: 443
    kvm_port: 17990              # HP iLO 4 KVM port
    ipmi_host: 192.168.1.100     # defaults to host
    ipmi_port: 623               # defaults to 623
    auth:
      username: admin
      password: "${BMC_PASS}"    # env var substitution
    max_fps: 30
    video_backend: mahogany
    encoder_backend: software    # software | nvenc (stream encoders)
    audio_backend: ~
```

### Pending Work
- **Auth stubs**: `handlers/auth.rs` returns hardcoded success. Routes are registered but not removed (listed in `routes.rs` lines 67-70). Implement real auth before removing stubs.
