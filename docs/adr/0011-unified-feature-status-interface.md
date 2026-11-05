# ADR-0011: Unified Feature Status Interface

**Status**: Accepted
**Date**: 2026-06-18

Every `GET /feature/:id/status` endpoint returns a common base shape derived from `FeatureConnection` state. Feature handlers may append feature-specific runtime fields from `VideoProcessor` or typed backend traits.

## Context

Each feature used to return a different status shape. The frontend needed a uniform base contract for availability, connection state, and backend identity, while still keeping fields such as video stream state, MSD drive metadata, or power status.

## Decision

`build_feature_status()` in `bmc/feature/mod.rs` owns the common base:

- `available`
- `online`
- `state`
- `backend`
- `connection_id`
- `ref_count`

The status path does not call a generic backend `execute("status")` hook. The current Feature layer has no generic execute seam.

Feature handlers append data from the owner of that data:

- Video appends stream/instance fields and reads processor-owned state where needed.
- HID appends instance status only.
- MSD merges filesystem/drive/image metadata and, when connected, typed `MsdBackend` status.
- Power reads typed `PowerBackend` status when available.
- Audio remains a placeholder fallback.

## Consequences

- The frontend can rely on the common base for all features.
- The interface keeps locality: feature-specific fields stay in the feature handler or processor that owns them.
- Video status does not require a broad backend-owned video interface; video control and stream state remain in `VideoProcessor`.
- Missing FC fallback remains simple: `{available: false, online: false}`.
