# ADR-0008: Registry Pattern for Backend Selection

## Status

Accepted

## Context

The system has multiple "backend" selection points where a string ID (from config or
codec metadata) is mapped to a concrete adapter:

| Seam | Adapters | Selection mechanism |
|------|----------|-------------------|
| `FrameEncoder` (HTTP MJPEG) | `SoftwareEncoder`, `WebpEncoder`, `NvencEncoder` | `match` in `probe_encoder()` + `create_encoder()` |
| `VideoStreamEncoder` (WebRTC) | `SoftwareFfmpegEncoder`, `NvencFfmpegEncoder` | Registry (`STREAM_ENCODER_BACKENDS`) |
| Protocol session (`connect_bmc`) | `MahoganySession` | `match` in `connector.rs` |

Each `match`-based selection requires editing a central factory function when adding
a new adapter. This violates the Open-Closed Principle and concentrates knowledge of
all adapters in one place, making the system harder to extend without risk of
introducing bugs in unrelated paths.

The `VideoStreamEncoder` seam already uses a static registry array
(`STREAM_ENCODER_BACKENDS`). Adding a new streaming encoder requires appending one
entry — no existing code changes.

## Decision

Use a **static registry array** for all backend selection seams going forward.

Each registry entry carries:

- `id` — the string identifier (from config or codec metadata)
- `name` — human-readable display name
- Additional seam-specific fields (e.g., `probe` fn for hardware availability,
  `create` fn for adapter construction, `is_hardware` flag)

Selection is a linear scan of the registry (`iter().find(|b| b.id == id)`). For
small N (currently ≤5 entries per seam), this is negligible and avoids the
complexity of a HashMap.

Existing `match`-based factories (`encoder/backend.rs`, `connector.rs`) are
migrated to this pattern as opportunities arise. New seams must use the registry
pattern.

## Consequences

**Positive:**

- Adding a new backend is a single-entry addition to a static array — no match
  arms to edit, no risk of forgetting to update a sibling function
- Each seam's registry is co-located with the trait it satisfies, giving locality
- The registry is discoverable — `STREAM_ENCODER_BACKENDS` lists every available
  `VideoStreamEncoder` adapter in one place
- `EncoderRegistry::discover()` can iterate the registry instead of maintaining a
  parallel list of IDs

**Negative:**

- Linear scan instead of direct dispatch — negligible for small N, but worth
  monitoring if any seam grows past ~20 entries
- `fn` pointers in static arrays cannot capture environment; adapter constructors
  must be self-contained

**Neutral:**

- The protocol seam in `connector.rs` is deferred — its adapter signatures differ
  significantly (`MahoganySession::connect` takes config + encoder, others may
  differ), and there is only one implementation today. Migrate when a second
  protocol adapter appears.
