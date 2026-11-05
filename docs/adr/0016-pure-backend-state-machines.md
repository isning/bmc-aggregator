# ADR-0016: Pure state decisions for the remaining backends

**Status**: Accepted and implemented
**Date**: 2026-11-03

## Decision

Extend the contract of ADR-0015 to Mahogany, VNC, Redfish, IPMI and WOL.
`crates/bmc-backend-model` is the protocol-neutral decision crate. Native iDRAC
models remain in `bmc-idrac-model`; unrelated protocols do not depend on iDRAC.
Both crates are dependency-free, `no_std`, and forbid unsafe code and wildcard
enum match arms. Public transitions consume owned values, return next state and
explicit actions/results, and accept time/observations as arguments. Private
helpers may mutate owned local data; no public external mutable context, async,
I/O, injected callbacks, ambient clocks, shared cells/atomics or global state.

The orthogonal models own pool admission, session terminal states, ordered
negotiation, intended HID state, partial-record budgets, keyboard ownership,
media setup and attachment uncertainty, SCSI command admission, task monitoring,
pagination and transaction outcomes. Their runtime shells own sockets, timers,
locks, queues, file handles, HTTP clients, binary/pixel transformations, metrics,
and publication. The application FeatureConnection lifecycle stays feature-owned.

This is an enforced decision boundary, not a requirement to wrap stateless codecs,
configuration descriptors or read-only field mapping in artificial phase enums.
Read-only health and firmware features share bounded transport and traversal
models. Do not introduce another lifecycle enum or authoritative mutable phase
flag in a protocol worker for a migrated flow. Extend the pure model instead.

## Important behavior

- Pool acquisition and final release are atomic under resource locks. A cancelled
  VNC creator removes its own connecting slot and publishes sticky completion;
  a cancelled Mahogany waiter releases its lock bookkeeping.
- HID plans are serialized against one owned model. Each complete wire batch
  occupies one queue entry. The shell commits intent after queue admission with
  no intervening await. Reset failure/cancellation retains keys for retry;
  consumer press/release and mode-plus-pointer batches cannot be half-admitted.
  Mouse movement remains lossy under backpressure; dropped plans do not advance
  local intent or wait behind stale pointer traffic.
  Admission still does not prove that a guest applied the input.
- Handshakes and partial records have absolute budgets. Unknown framing ends
  the connection; Mahogany no longer guesses framing by skipping bad bytes.
  VNC 3.7 None authentication correctly omits the 3.8 security-result read, and
  32-bit 3.3 security types cannot alias a supported 8-bit value by truncation.
- Mahogany keyboard privilege and observed ownership are separate facts. Remote
  ownership cannot grant a missing account privilege. Storage setup requires
  matching remote port and ready peer. Unknown/disconnect-error status preserves
  possibly active resources. Cancelled mounts issue best-effort release, without
  blocking cleanup on a full queue; no success is published.
- Redfish task monitoring has one 30-second budget including requests, response
  bodies and Retry-After sleeps. Unknown states/statuses fail closed. A task
  monitor 204 is an explicit completion observation; a successful HTTP response
  with no known task result is not automatically success. Pagination allows the
  final page at its limit and rejects loops and malformed links/members.
- Redfish boot/indicator writes and media mount/eject require readback through
  the transaction model. Missing LED/Inserted fields are unknown, not Off or
  empty. IPMI boot and identify writes also require readback.
- IPMI uses one wire attempt, so timeouts cannot blindly replay chassis changes.
  Completion codes are typed model observations. Session RAII closes on errors
  and early returns. Privilege denial updates a typed capability without parsing
  error strings. Temporary identify accepts intervals 1–255; indefinite identify uses the separate force-on byte.
- IPMI chassis success means protocol acceptance, not host transition completion.
  WOL confirms a complete datagram dispatch only and keeps device power unknown.

## Enforcement and evidence

The shared AST visitor in `tests/support/pure_model.rs` checks both pure crates.
`backend_model_boundaries` checks production model bridges and the complete source
inventory in `docs/testing/backend-state-machine-coverage.md`. The existing iDRAC
boundary tests and inventory remain mandatory. The Rust quality stage checks both
models with no default features, workspace Clippy and all workspace tests.

Model and mock runtime tests cover unknown/terminal events, cancellation cleanup,
readback mismatches, full task deadlines, pagination limits, input reset retry,
coordinate overflow and framed storage exchanges. These are executable invariants,
not formal verification or hardware compatibility certification. Vendor behavior
that cannot be established from observations returns error/unknown. Blocking IPMI
work may outlive its async caller; its owned, bounded session still closes. Another
client can change remote state after readback. See the coverage ledger for limits.
