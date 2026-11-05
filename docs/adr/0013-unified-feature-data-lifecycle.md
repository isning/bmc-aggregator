# ADR-0013: Unified Feature Data Lifecycle

**Status**: Accepted
**Date**: 2026-08-05

## Context

Power, boot, identify, and virtual-media views previously fetched and polled
their own copies of BMC state. Desktop and mobile variants could issue the same
request concurrently, a mutation only updated its initiating view, and a
refresh started before a mutation could publish stale data afterward.

`FeatureConnection` remains the owner of transport connectivity. It is not a
data cache and must not gain a generic operation dispatcher.

## Decision

Each feature owns typed, reconnect-stable `FeatureResource<T>` values in its
`feature_runtime`. A resource provides:

- one atomic `{revision, value}` snapshot;
- one operation mutex shared by refreshes and mutations;
- refresh single-flight when another operation changes the revision;
- equality deduplication;
- revisioned `feature.data_changed` publication after a committed change.

Every `FeatureConnection` also receives a process-local, strictly monotonic
numeric data epoch. HTTP snapshots and data events carry `{epoch, revision}`.
Revisions are comparable only inside one epoch; a newer epoch replaces the
complete local snapshot, while delayed events from a retired/lower epoch are
discarded. WebSocket reconnect clears the frontend's epoch acceptance state
before the mandatory HTTP resync, so a server process restart may safely begin
its epoch counter again.

Connection state continues to use a Tokio `watch` channel because only its
latest value matters. Revisioned feature data uses a bounded `broadcast`
channel so rapid mutations are not silently coalesced. If a browser receiver
lags, the server emits `feature.data_resync_required`; the browser performs an
authoritative HTTP refresh.

Current resources are:

| Feature | Resource |
|---|---|
| Power | `status` |
| Boot | `status` |
| Identify | `status` |
| MSD | `status`, `images.aggregator`, `images.bmc` |

MSD `status` is a single typed snapshot of capabilities, attachments, and
backend status. Target topology and attachment state therefore cannot update
independently. Catalogs remain separate because storage locations have
independent consistency and availability.

MSD also publishes a generic `occupancy` value with `idle`, `managed`, and
`external` states. `external` means the BMC has authoritatively rejected the
current protocol session because another client owns virtual media. It is not
represented as a synthetic attachment: legacy iDRAC AVMP discloses neither the
other session's target nor its image identity. `details_available=false`
preserves that protocol boundary. An active lifecycle fault terminates feature
acquisition immediately, so a known busy/error state is never held until the
ordinary connection timeout. Fault-change events update occupancy immediately;
the normal HTTP refresh remains authoritative and clears the state after the
external owner releases its transport.

The frontend creates one module-level typed resource per server resource.
Components acquire reference-counted subscriptions; the WebSocket feature
lease is the union of all owners. Polling uses a non-overlapping timeout after
the previous request settles. Events and HTTP responses are accepted only when
their epoch/revision is authoritative. Device changes and final-subscriber
release invalidate both the request generation and any in-flight completion.
Device scope changes proactively rebind the single global WebSocket, and
WebSocket reconnect always causes an HTTP refresh.

Boot discovery polls only while the initial snapshot is absent or discovery is
`discovering`; both an initial transport timeout and a slow successful probe
therefore recover without leaving a blank panel. Any terminal discovery state
stops that poll. MSD keeps its active discovery poll only while a subscriber
owns the panel, because external AVMP/Redfish attachment state has no portable
push channel.

## Mutation Contract

Typed handlers keep their existing operation methods. On success they read or
construct the authoritative feature snapshot, commit it, and return the same
revision and caller-provided `mutation_id`. An event may reach the browser
before the HTTP response; revision comparison makes this ordering harmless.

Power is eventually convergent. Its initial command/readback is serialized,
then a bounded server-side refresh task observes delayed state changes. The
task stops when the expected state is reached, the feature is replaced, or its
backend runtime changes.

Registry reconfiguration is serialized and staged. Changed devices are built
outside the live map, replaced devices are stopped, and the complete device map
is swapped atomically; readers never observe a partial registry. Shutdown first
marks all feature connections stopped, then closes WebRTC within a five-second
bound, and finally clears backend runtimes. Stopped connections also terminate
both state/fault and data-event forwarders, preventing a late old backend event
from entering the new device epoch.

## Consequences

- Multiple browser sessions observe successful changes without private UI
  refresh code.
- External BMC changes are discovered by one centrally owned active refresh
  policy and broadcast only when the typed value differs.
- A controller-wide external media owner blocks mount controls without
  inventing a target. Exact third-party image discovery and per-target eject
  remain unavailable when the BMC protocol does not expose them.
- Event loss is recoverable because HTTP snapshots are authoritative.
- Per-resource operations are serialized for correctness. MSD target
  operations additionally retain the backend-wide lifecycle mutex because
  current AVMP/Redfish adapters do not promise concurrent target mutation.
- A successful mutation is immediately visible to every subscribed client;
  changes made outside the aggregator converge through the one active backend
  refresh policy and then use the same revision broadcast path.
