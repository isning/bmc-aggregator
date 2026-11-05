# iDRAC state-machine coverage and failure contract

This inventory covers every Rust source file in the three native iDRAC crates,
their application adapters, and the pure model. The architecture test compares
this list to the filesystem: adding or moving a file requires updating this
inventory. This is a review boundary, not a claim of line or branch coverage.

## Control flows

AVCT uses pure models for shared pool acquisition/release, cancelled connection
creation, session epochs, bounded native reconnect, bootstrap and baseline
request deduplication, DVC startup/fallback/profile confirmation, HID intent, and
bounded display-command assembly. Unknown or unimplemented control messages,
malformed records, unsupported video and decoder failures terminate the session;
only classified transport failures permit native recovery. APCP remote close is
terminal. Queued input is tagged with its accepting transport epoch and cannot
replay on a recovered socket. Consumer key press/release is one queue item.

AVMP uses pure models for session recovery, silence/attempt budgets, incremental
framing, drive refresh and command admission, map/release/delete confirmation,
and ServerImage create/chunk/finalize/readback sequencing. A pending refresh owns
the response stream. Main polling and acknowledgement waiters share one live-event
interpreter: media reads, device failures, heartbeat and configuration observations
are processed immediately. Only operation-owned discovery observations enter a
bounded 32-record inbox; unknown messages or overflow terminate the epoch.
Read cancellation retains framing progress; partial frames
have an absolute ten-second deadline. Failed operations after output terminate
the epoch, preventing later operations from consuming their delayed replies.
Successful unmount requires the matching release acknowledgement; reads remain
serviceable until it arrives. Independent remote errors can invalidate a mapping
earlier. Unsolicited release duplicates cannot remove a newer mapping. An image
is staged for early reads before map acknowledgement, but is not published as
attached until confirmation. Read admission rejects empty/out-of-image requests
before output, and transfer chunks obey both the BMC limit and the outer packet
limit including its headers. Drive-refresh deadlines belong to the model and
late count acknowledgements/topology convergence cannot extend or complete them.
Recovery clears old topology/capability/configuration/storage observations before
publishing a restored session. Shutdown preempts queued commands, pending I/O,
and reconnect waits, then allows at most 500 ms for best-effort protocol close.

WS-MAN uses pure models for HTTP connection reuse, authentication, all three
pagination flows, boot selection confirmation, inventory-state classification,
and inspect/set/commit/readback remediation. A connection returns to the cache
only after its complete bounded response body. Errors and cancellation discard
it. Discovery, selection and remediation serialize their publications. Selection
is published only after active one-time state and the exact requested entry are
read back and activation is checked again after inventory enumeration; a setting
consumed by the host during readback cannot be published as a current choice.
A fresh discovery cycle also clears the previous selection while queries run.
Unavailable readback clears certainty. Inventory remediation never
mutates an unknown initial state or reports accepted writes/jobs as confirmation.

## Failure semantics

Timeout means an unconfirmed remote result. Mutation effects are not blindly
replayed. Unknown input has an explicit unsupported/failure policy; recognized
informational messages cannot confirm a pending operation. Absolute deadlines,
byte/page/fragment/attempt limits and terminal states prevent partial progress
from extending work without bound. Logical time and observations enter the
model as event data; runtime code owns sockets, TLS, tasks, locks, files, codecs,
frame buffers, and feature-store publication.

The codecs and stateless configuration/capability mapping functions remain in
protocol crates. Their byte/pixel transformations are not all moved into the
no_std model. State-dependent command-assembly admission is model-owned, with
metadata validation and pixel decoding performed by the codec shell. This does
not change the application FeatureConnection lifecycle or migrate other backends.

## Evidence and limits

Unit tests exercise state/event matrices, malformed wire values, all optional
inventory facts, terminal invariants, stale epochs, deadlines and resource limits.
Mock-peer tests also hold map/release acknowledgements until image reads have
completed, exercise oversized remote read limits and zero/out-of-image reads,
and change one-time activation between boot entry readback and confirmation.
Mock-peer tests exercise actual queue output, partial-read cancellation, release
acknowledgement, shutdown during stalled I/O, cancelled pool creators, HTTP body
cancellation, connection-close tokens, and existing protocol handshakes/frames.
Run `nix develop --command just ci-rust` for the full gate.

Validation on 2026-11-03: the Rust workspace gate passed 814 tests with zero
failures and seven explicit hardware-dependent ignores across 19 test targets.
This includes 46 pure-model tests and five model-boundary/inventory tests.
Final AVCT epoch-confirmation changes also passed all 91 AVCT tests. Formatting,
model compilation without default features, Clippy with the repository's existing
three compatibility exceptions, script syntax, and ShellCheck passed. Logs are
in `.local/quality/idrac-correctness-quality.log` for the current Rust gate,
`idrac-correctness-tests.log` for focused protocol regression, and
`idrac-expanded-scripts.log` for the unchanged scripts. This correctness follow-up
adds six model tests and three mock-peer scenarios without adding source files.

No physical device compatibility run is automatic. The protocol provides no
unique operation identifier for several AVMP acknowledgements and no applied-input
acknowledgement for HID. Models track observed evidence and intended input; they
cannot distinguish every arbitrarily replayed matching acknowledgement or prove
that firmware telling us success actually performed an action. A new firmware
message is rejected until supported. This is bounded handling of supported and
unsupported observations, not a proof that all future device behavior is known.

## Reviewed source inventory

- `crates/bmc-backend-idrac-avct/src/apcp.rs` — wire codec, parser, or transport primitive.
- `crates/bmc-backend-idrac-avct/src/avct.rs` — wire codec, parser, or transport primitive.
- `crates/bmc-backend-idrac-avct/src/block_video.rs` — wire codec, parser, or transport primitive.
- `crates/bmc-backend-idrac-avct/src/configuration.rs` — configuration/public surface.
- `crates/bmc-backend-idrac-avct/src/decoder.rs` — wire codec and pixel transformation; repeated RLE patterns reuse an unchanged-prefix color lookup within each decode call, bounding history scans instead of rescanning the full canvas per command. Deterministic pixel-visit and reference-equivalence regressions cover this cache; no control decisions move into the codec.
- `crates/bmc-backend-idrac-avct/src/display.rs` — wire codec, parser, or transport primitive.
- `crates/bmc-backend-idrac-avct/src/dvc.rs` — wire codec, parser, or transport primitive.
- `crates/bmc-backend-idrac-avct/src/hid.rs` — runtime owner and typed feature implementation; Digest retry uses the shared checked challenge parser/builder and rejects unsupported authentication parameters.
- `crates/bmc-backend-idrac-avct/src/input.rs` — wire codec, parser, or transport primitive.
- `crates/bmc-backend-idrac-avct/src/lib.rs` — configuration/public surface.
- `crates/bmc-backend-idrac-avct/src/login.rs` — wire codec, parser, or transport primitive.
- `crates/bmc-backend-idrac-avct/src/pool.rs` — Transport/pool and protocol worker resource ownership; pure negotiation, recovery, HID and video models; validated IPv6 authorities for initial and redirected TCP connections.
- `crates/bmc-backend-idrac-avct/src/video.rs` — runtime owner and typed feature implementation; Digest retry uses the shared checked challenge parser/builder and rejects unsupported authentication parameters.
- `crates/bmc-backend-idrac-legacy-media/src/apcp.rs` — wire codec, parser, or transport primitive.
- `crates/bmc-backend-idrac-legacy-media/src/configuration.rs` — configuration/public surface.
- `crates/bmc-backend-idrac-legacy-media/src/lib.rs` — configuration/public surface.
- `crates/bmc-backend-idrac-legacy-media/src/msd.rs` — runtime owner and typed feature implementation; Digest retry uses the shared checked challenge parser/builder and rejects unsupported authentication parameters.
- `crates/bmc-backend-idrac-legacy-media/src/protocol.rs` — wire codec, parser, or transport primitive.
- `crates/bmc-backend-idrac-legacy-media/src/session.rs` — Media session and file/socket ownership; pure framing, recovery and storage decisions; validated IPv6 authorities for initial and redirected TCP connections.
- `crates/bmc-backend-idrac-wsman/src/configuration.rs` — configuration/public surface.
- `crates/bmc-backend-idrac-wsman/src/lib.rs` — Validated host admission and consistent IPv6 TCP/HTTP/SOAP authority; runtime owner and typed feature implementation; Digest retry uses the shared checked challenge parser/builder and rejects unsupported authentication parameters.
- `crates/bmc-idrac-model/src/assembly.rs` — pure decision model.
- `crates/bmc-idrac-model/src/avct.rs` — pure decision model.
- `crates/bmc-idrac-model/src/framing.rs` — pure decision model.
- `crates/bmc-idrac-model/src/hid.rs` — pure decision model.
- `crates/bmc-idrac-model/src/lib.rs` — pure decision model.
- `crates/bmc-idrac-model/src/management.rs` — pure decision model.
- `crates/bmc-idrac-model/src/media.rs` — pure decision model.
- `crates/bmc-idrac-model/src/operation.rs` — pure decision model.
- `crates/bmc-idrac-model/src/pool.rs` — pure decision model.
- `crates/bmc-idrac-model/src/recovery.rs` — pure decision model.
- `crates/bmc-idrac-model/src/session.rs` — pure decision model.
- `crates/bmc-idrac-model/src/storage.rs` — pure decision model.
- `crates/bmc-idrac-model/src/wsman.rs` — pure decision model.
- `src/bmc/backends/idrac_avct/hid.rs` — application lifecycle/event/frame bridge.
- `src/bmc/backends/idrac_avct/mod.rs` — application lifecycle/event/frame bridge.
- `src/bmc/backends/idrac_avct/video.rs` — application lifecycle/event/frame bridge; one forwarding task publishes the initial source observation, filters queued/retained frames against current signal, and stops on lease shutdown.
- `src/bmc/backends/idrac_legacy_media/mod.rs` — application lifecycle/event/frame bridge.
- `src/bmc/backends/idrac_legacy_media/msd.rs` — application lifecycle/event/frame bridge.
- `src/bmc/backends/idrac_wsman/mod.rs` — application lifecycle/event/frame bridge.
