# ADR-0015: Pure State Machines for iDRAC Control Decisions

**Status**: Accepted; expanded native iDRAC control coverage implemented
**Date**: 2026-11-03

## Decision

Represent migrated iDRAC control flows as explicit finite state machines in
`crates/bmc-idrac-model`. A transition consumes owned state and a typed event and
returns `Transition<State, Effect>`. The implementation can mutate its own local
owned value, but callers cannot mutate machine fields or pass mutable external
context into public model operations.

The model crate is dependency-free, `no_std`, and forbids unsafe code. It has no
sockets, tasks, locks, shared atomics, randomness, wall clocks, callbacks, or
logging. Time is a logical `Duration` supplied by the runtime. Runtime adapters
execute effects and feed observations/completions back; they do not duplicate
the migrated phase enums or update model fields directly.

This contract is mandatory for iDRAC control-flow changes and their extensions.
Other protocol backends follow the separate neutral model in
[ADR-0016](0016-pure-backend-state-machines.md). The initial pilot covered
the six flows below; the expansion adds the runtime ownership and operation
models described next.

## Initial coverage

The three native iDRAC backends execute these production models:

- AVCT DVC startup: bootstrap, initial-chain drain, full refresh, decoded-chain
  confirmation, no-signal suspension/resume, deadline, and failure.
- AVCT DVC profile switching: queued/write acknowledgement, target-chain drain,
  refresh/decode confirmation, one fallback budget, completion/cancellation,
  stale operation IDs, and confirmed active-profile observations.
- AVMP native reconnect: silence interval, bounded connection attempts, retry
  delay, absolute attempt deadline, completion, and failure.
- AVMP drive refresh: discovery, device-count acknowledgement, observed topology
  convergence, final verification, failure, unsolicited responses, and closure.
- WS-MAN authentication: Basic response, at most one Digest retry, success,
  rejection, and terminal response handling.
- WS-MAN enumeration: all three source/configuration/BIOS page traversals share
  the same bounded model and reject missing/empty cursors.

## Expanded coverage

Pure models now also govern shared-console pool acquisition/release and failed
creator cleanup, session transport epochs, bounded/cancellable AVCT recovery,
bootstrap/baseline deduplication, iDRAC8 colour readback, HID intent and reset,
frame-command assembly budgets, incremental AVMP framing, refresh command
admission, mount/release/delete confirmation, ServerImage upload sequencing,
HTTP connection reuse, exact boot-selection readback, and inventory remediation.

The runtime executes bounded I/O and commits model-approved effects. Codec
buffers, byte parsing, TLS, raw media file I/O, resource handles and feature-store
publication remain runtime-owned. Stateless configuration/capability mapping
functions remain in the backend crates; pixel transformations are not all moved
into the pure crate. Application FeatureConnection lifecycle is unchanged.

See [the complete source inventory and failure contract](../testing/idrac-state-machine-coverage.md)
for the reviewed files, live model bridges, regression evidence and protocol
limits. The inventory is executable: new or moved iDRAC source files require an
explicit inventory update. This is not a proof of device compatibility or full
line/branch coverage.

## Event and effect rules

- Match typed events and phases explicitly. The model denies Clippy's wildcard
  enum match arms. Unknown wire values are validated/classified by the protocol
  parser; modeled display events specify suppress, decode, refresh, or reject.
- A no-change result is permitted only for an explicitly handled duplicate,
  unsolicited, stale, or wrong-phase input. Unhandled events must not disappear
  into a generic success branch.
- Keep socket-write completion distinct from device confirmation. A selected
  DVC profile is committed only after the expected reset-marked chain has been
  decoded, or a separately source-confirmed profile observation arrives.
- Stop processing terminal work. In particular a failed DVC startup cannot be
  revived by a late no-signal notification. A new session creates a new model.
- DVC operation generations never wrap and alias an older request. A stale write,
  cancellation, or completion lookup cannot advance a newer operation.
- Startup/target/fallback deadlines are absolute within their phase budget.
  Partial progress or repeated fallback requests must not extend them.
- Native reconnect waits strictly beyond the official silence interval, then
  bounds the whole attempt phase including in-flight I/O. Late success after its
  deadline is a failure, not a confirmed recovered session.
- WS-MAN may reuse an enumeration cursor across pages; cursor equality is not
  evidence of a loop. The page budget provides the termination bound. The last
  allowed page may finish; a seventeenth request must never be sent.
- Independent state machines share observations through their runtime owner.
  They are not independent authorities for the same device fact. The existing
  session event loop orders AVCT startup and profile-switch effects.

External devices can execute a command and lose the acknowledgement. A model
therefore describes local knowledge, not guaranteed remote truth. Callers must
retain unknown/stale observations and existing authoritative readbacks rather
than infer success from a timeout or a local write.

## Enforcement and validation

`tests/idrac_model_boundaries.rs` parses model sources with `syn` and rejects
runtime imports/paths, foreign calls, global state, async decisions, public
mutable interfaces, injected callbacks, hidden source modules, and unreviewed
macros. Negative fixtures verify the guard rejects prohibited constructions.
The tests also require all native iDRAC backends to consume the model crate,
check production bridges for recovery/confirmation/input/assembly, and reject
reintroduced phase enums for migrated flows. The quality gate compiles the model
with no default features before linting and executing the workspace tests.

The only local declaration macro generates phase enums and their test fixtures
from the same variant list. State/event matrix tests cover deterministic handling
and terminal invariants; sequence tests cover source-confirmed transitions,
unknown packets, absolute deadlines, stale IDs, page exhaustion, and retries.
Existing protocol/mock-peer tests still execute the real adapters and I/O shell.

Run `nix develop --command just ci-rust`. No hardware test
is enabled automatically. Pure-model tests do not establish device compatibility
or prove mathematical purity for arbitrary future Rust code. Compiler isolation,
AST checks, linting, invariants, protocol tests, and review provide complementary
constraints; none alone proves that an external protocol was modeled completely.

## Library choice

The implementation uses explicit Rust enums and exhaustive matches. `statig` was evaluated
as a possible hierarchical-state implementation; its mutable context and async
handlers would still require these purity restrictions. It is not a prerequisite
for a finite state machine or for this enforcement contract. Adopt it later only
if it simplifies a demonstrated hierarchy while preserving the same isolated
model interface and tests.
