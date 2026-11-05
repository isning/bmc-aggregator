# ADR-0009: Per-Feature Connection State Management

**Status**: Accepted

## Context

The original device-level connection model grouped transport and feature state too tightly. That made HID, MSD, and video share one lifecycle even when they should disconnect independently.

## Decision

Each feature now has its own `FeatureConnection`.

`FeatureConnection` owns:

- identity
- feature config
- lifecycle state
- ref count
- event bus
- two runtime slots:
  - `backend_runtime` for typed backend adapter state
  - `feature_runtime` for feature-owned state such as `VideoRuntime`

`FeatureGuard` increments the ref count on acquire and decrements it on drop.

Lifecycle behavior:

- `backend_runtime` is cleared on reconnect and disconnect.
- `feature_runtime` survives backend reconnects.
- one feature disconnect does not force other features on the same device to disconnect.

Video uses this model to keep `VideoProcessor` alive while the Mahogany source adapter reconnects or refreshes.

## Consequences

- HID, MSD, video, and power can evolve independently.
- Backend adapters can be typed and narrow.
- Feature-specific runtime stays where it is used, instead of being mixed into transport state.
