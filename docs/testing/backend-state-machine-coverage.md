# Remaining backend state-machine coverage

This inventory extends ADR-0015 with ADR-0016. It covers every source file of the
five remaining protocol backends, the neutral support/model crates, and their
application adapters. `backend_model_boundaries` compares this exact inventory
with disk; adding a source requires review. Inventory coverage is not line or
branch coverage, nor a proof of arbitrary firmware compatibility.

## Decision owners

| Flow | Pure owner | Runtime contract |
| --- | --- | --- |
| VNC and Mahogany connection/pool | connection, negotiation | Atomic lease admission; cancelled creators remove their slot/lock waiter. No automatic replay after disconnect. |
| VNC and Mahogany HID | input | One serialized intent, atomic bounded batches, commit after admission, reset retains keys on cancelled/failed admission. |
| VNC and Mahogany incoming stream | stream | Preserve buffered bytes; fixed partial-record deadline; invalid framing ends session. |
| Mahogany privileges | control | Account privilege cannot be overridden by a control notification; timed wait, explicit revoke. |
| Mahogany virtual media | media, storage | Matching port plus ready peer; unknown status retains possibly active resources; invalid SCSI framing rejected. |
| Redfish actions | task, transaction | Absolute 30-second monitor budget including HTTP; known pending/complete/failure; unknown fails closed; configuration/media readback. |
| Redfish inventory | pagination | Loop/limit rejection; terminal page at limit permitted; malformed Members/nextLink rejected. |
| IPMI | transaction | One wire attempt per request, checked completion code, boot readback; RAII closes sessions on all returns. |
| WOL | transaction | Full datagram dispatch only; device power remains unknown. |

Independent state machines share observations through their runtime owner. Local
queue admission is not a remote HID acknowledgement. IPMI chassis actions return
protocol acceptance, not completion of a host shutdown/reboot. Firmware and sensor
features are read-only and use existing bounded transport/collection paths.

## Files

- `crates/bmc-backend-ipmi/src/boot.rs` — Boot wire mapping and typed selection; mutation readback uses transaction confirmation.
- `crates/bmc-backend-ipmi/src/client.rs` — IPMI completion/session ownership or Redfish bounded HTTP/task/pagination shell.
- `crates/bmc-backend-ipmi/src/configuration.rs` — Stateless feature schema and configuration mapping.
- `crates/bmc-backend-ipmi/src/identify.rs` — Typed indicator mapping; unknown results are rejected, writes use client acceptance/readback.
- `crates/bmc-backend-ipmi/src/lib.rs` — Public protocol exports and construction.
- `crates/bmc-backend-ipmi/src/power.rs` — Power mapping; IPMI/Redfish client owns command acceptance; transitions do not fabricate observed power.
- `crates/bmc-backend-mahogany/src/auth.rs` — Bounded HTTP Digest exchange during modeled negotiation; validated endpoint authority and redirect rejection.
- `crates/bmc-backend-mahogany/src/configuration.rs` — Stateless feature schema and configuration mapping.
- `crates/bmc-backend-mahogany/src/firmware.rs` — Read-only firmware observation from the shared protocol session; no independent mutation lifecycle.
- `crates/bmc-backend-mahogany/src/framebuffer.rs` — Pixel transformations and buffer ownership, not a lifecycle authority; truncated source tiles are rejected before pixel or publication-state changes; tile copies clip each axis without wrapping right-edge pixels into later rows; every native frame allocation checks a nonzero 64 MiB byte budget before publication.
- `crates/bmc-backend-mahogany/src/hid.rs` — Pure input planning; bounded atomic batch admission before state commit.
- `crates/bmc-backend-mahogany/src/lib.rs` — Public protocol exports and construction.
- `crates/bmc-backend-mahogany/src/msd.rs` — Media resource mapping and runtime ownership; pure setup/attachment/confirmation decisions.
- `crates/bmc-backend-mahogany/src/perf.rs` — Metrics only, no control authority.
- `crates/bmc-backend-mahogany/src/pool.rs` — Resource ownership; pure pool/session, control, negotiation, and stream decisions.
- `crates/bmc-backend-mahogany/src/protocol.rs` — Wire codecs and validated parsing, including complete ordinary BitBlt rectangle payloads before copying, nonzero enhanced-tile dimensions, explicit rejection of unsupported enhanced encodings and atomic rejection of truncated Basic/HLC tile batches; pure negotiation decisions in worker/handshake.
- `crates/bmc-backend-mahogany/src/storage_server.rs` — SCSI codec and file I/O shell; pure setup/command admission, bounded exchanges.
- `crates/bmc-backend-mahogany/src/video.rs` — Feature source delegation; protocol worker owns session and frame observations.
- `crates/bmc-backend-model/src/connection.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-model/src/control.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-model/src/input.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-model/src/lib.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-model/src/media.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-model/src/negotiation.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-model/src/pagination.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-model/src/storage.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-model/src/stream.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-model/src/task.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-model/src/transaction.rs` — Pure, dependency-free state/event decisions and adversarial transition tests.
- `crates/bmc-backend-redfish/src/boot.rs` — Boot wire mapping and typed selection; standard BootOptionEnabled is authoritative, legacy Enabled is used only when absent, and unknown values remain disabled; discovery rejects missing/blank references instead of publishing empty or partial success; mutation readback uses transaction confirmation.
- `crates/bmc-backend-redfish/src/client.rs` — Bounded HTTP/task/pagination shell; rejects non-object GET resources and malformed or foreign-authority member links before exposing any collection result.
- `crates/bmc-backend-redfish/src/configuration.rs` — Stateless feature schema and configuration mapping.
- `crates/bmc-backend-redfish/src/firmware.rs` — Read-only firmware observation; malformed member resources reject the complete inventory; no independent mutation lifecycle.
- `crates/bmc-backend-redfish/src/health.rs` — Read-only inventory/sensor mapping through bounded client traversal; malformed advertised links, resources, arrays and sensor entries reject the entire snapshot instead of publishing a partial inventory.
- `crates/bmc-backend-redfish/src/identify.rs` — Typed indicator mapping; unknown results are rejected, writes use client acceptance/readback.
- `crates/bmc-backend-redfish/src/lib.rs` — Public protocol exports and construction.
- `crates/bmc-backend-redfish/src/msd.rs` — Remote status URLs strip credentials, query and fragment; opaque/non-HTTP URLs are redacted. Media resource mapping and runtime ownership; pure setup/attachment/confirmation decisions.
- `crates/bmc-backend-redfish/src/power.rs` — Power mapping; IPMI/Redfish client owns command acceptance; transitions do not fabricate observed power.
- `crates/bmc-backend-support/src/endpoint.rs` — Stateless host admission and DNS/IPv4/IPv6 authority formatting; rejects embedded URL components.
- `crates/bmc-backend-support/src/http_digest.rs` — Stateless HTTP Digest challenge-list/quoted-field parsing, explicit MD5/auth admission, legacy no-qop calculation and checked header escaping; known-answer regressions.
- `crates/bmc-backend-support/src/lib.rs` — Public protocol exports and construction.
- `crates/bmc-backend-support/src/removable_image.rs` — Neutral removable-image validation and file ownership, no remote lifecycle.
- `crates/bmc-backend-vnc/src/configuration.rs` — Stateless feature schema and configuration mapping.
- `crates/bmc-backend-vnc/src/hid.rs` — Correct USB-to-X11 special-key/keypad mapping (distinct keypad Enter), with known-value regressions; Pure input planning; bounded atomic batch admission before state commit.
- `crates/bmc-backend-vnc/src/lib.rs` — Public protocol exports and construction.
- `crates/bmc-backend-vnc/src/pool.rs` — Resource ownership; pure pool/session, control, negotiation, and stream decisions.
- `crates/bmc-backend-vnc/src/protocol.rs` — VNC wire codecs and complete-message validation borrow rectangle payloads; owned pixel copies occur only after all rectangles arrive; pure negotiation decisions live in worker/handshake.
- `crates/bmc-backend-vnc/src/video.rs` — Feature source delegation; protocol worker owns session and frame observations.
- `crates/bmc-backend-wol/src/configuration.rs` — Stateless feature schema and configuration mapping.
- `crates/bmc-backend-wol/src/lib.rs` — Public protocol exports and construction.
- `src/bmc/backends/ipmi/boot.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/ipmi/identify.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/ipmi/mod.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/ipmi/power.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/mahogany/firmware.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/mahogany/hid.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/mahogany/mod.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/mahogany/msd.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/mahogany/video.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/mahogany/video_forwarder.rs` — Thin application frame bridge; sticky lease shutdown has priority over queued frames and interrupts publication waiting on the shared processor.
- `src/bmc/backends/redfish/boot.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/redfish/firmware.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/redfish/health.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/redfish/identify.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/redfish/mod.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/redfish/msd.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/redfish/power.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/vnc/hid.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/vnc/mod.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.
- `src/bmc/backends/vnc/video.rs` — Thin application frame bridge; sticky lease shutdown has priority over queued frames and interrupts publication waiting on the shared processor.
- `src/bmc/backends/wol.rs` — Thin application adapter: configuration, typed runtime, and observation bridging.

## Verification and limits

Run `nix develop --command just ci-rust`. Both model crates
are checked without default features; workspace Clippy, model purity checks,
production bridge checks, both source inventories, and all workspace tests are
required. Model tests exercise terminal, out-of-order, unknown, and deadline
observations; mock HTTP/socket tests cover runtime bridges and cancellation.

Validation on 2026-11-03: the complete Rust quality stage passed with **847 tests
passed, 0 failed, 7 hardware-dependent tests ignored across 21 targets**. This
includes 21 neutral-model tests, 46 iDRAC-model tests, both purity/inventory suites,
and all protocol/application regressions. Script syntax and ShellCheck also
passed. Logs: `.local/quality/backend-migration-quality.log` and
`.local/quality/backend-migration-scripts.log`.

No physical devices are available in this run. Devices may omit fields, expose
vendor extensions, or finish after the local timeout; these outcomes return an
error/unknown state and must be queried afresh. No mutation is automatically
replayed. A cancelled IPMI spawn_blocking caller cannot stop an in-flight blocking
UDP request; its owned session still closes when the bounded closure returns.
Readback is a point-in-time observation, not a guarantee against another client
changing the BMC afterward. Mahogany storage status lacks operation IDs, so an
arbitrarily delayed reply with the same port cannot be proved fresh.


Current validation uses `just ci`; see [Testing and validation](README.md) for
commands and interpretation limits. Behavioral tests cover stale listener-port
observations, DNS/IPv6 resolution, standard identify encoding, redirect confinement,
conservative reset capabilities and typed mutation uncertainty. The native-worker
AST phase guard remains mandatory.

- `crates/bmc-backend-model/src/boot.rs` — Pure classification of known, disabled and unknown Redfish boot overrides.

Power wire mapping also has shared-contract regressions: every serialized
`PowerAction` resolves through the shared canonical/compatibility parser; a mock
Redfish server verifies each advertised action's exact `ResetType` and rejects
unknown commands without dispatch. IPMI mapping checks distinguish hard reset
(3) from power cycle (2) and reject unsupported graceful restart before opening
a session. These are stateless wire mappings; protocol acceptance remains in the
existing transaction model and is not proof of a completed host transition.
