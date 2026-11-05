# iDRAC Compatibility and Acceptance Matrix

**Status**: The AVCT backend has protocol unit coverage for APCP, AVCT framing,
direct account login layout/results, display codecs, input encoding, pooling,
and preset registration. A generation is supported operationally only after its
own hardware row is completed.

## Required evidence

| Layer | Required evidence |
|---|---|
| Unit | APCP/AVCT framing, credential redaction, candidate VQ/IDCT and raw display decoding, HID layout, COW retained framebuffer, pool lifecycle, and bounded iDRAC6 DVC15 startup transitions. |
| Integration | iDRAC preset/config validation, feature lifecycle reconnect, config reload/shutdown, and no secret read-back. |
| Hardware | Login, first frame, sustained updates, keyboard, pointer, wheel, reset, disconnect/reconnect, and power/MSD capability checks on a real BMC. |

### Feature data lifecycle matrix

These cases apply to Power, Boot, Identify, MSD status, MSD image catalogs,
Firmware inventory, and Health/Sensor snapshots.
Hardware rows must use two simultaneous browser/API clients where specified.

| Case | Required result | Evidence |
|---|---|---|
| Concurrent refresh | Multiple readers cause one backend load and receive one revision | Rust unit test |
| Refresh/mutation race | A refresh started before a mutation cannot overwrite its result | Rust unit test |
| Rapid commits | Every revision reaches the registry broadcast without `watch` coalescing | Rust unit test |
| Duplicate value | Equal snapshots neither increment revision nor broadcast | Rust unit test |
| Event/response ordering | An event arriving before its HTTP response remains authoritative | Frontend revision test |
| Shared component ownership | Desktop/mobile panels share one request and one feature lease | Frontend lease test |
| WebSocket lag/reconnect | Client performs an authoritative refresh | Frontend lifecycle test |
| Cross-client mutation | Client A mutation updates client B without reopening its panel | Live dual-client test |
| External media change | Another owner is reported without invented mapping; mounts are blocked and all clients recover when the owner releases its transport | Live hardware test |
| Device route change | Late response from the previous BMC is discarded | Frontend lifecycle test |

Live lifecycle acceptance on 2026-08-05 used two isolated browser sessions and
the single aggregator instance on port 8400:

| Case | Result |
|---|---|
| Identify cross-client mutation | Client A selected temporary 15 seconds; client B changed without reopening. Hardware returned to Off and revision advanced. A then selected indefinite On and both clients updated immediately; A restored Off and both clients converged. Final iDRAC8 revision was 5 with no fault. |
| Power ownership | Each open client produced one non-overlapping approximately three-second status chain. Closing both Power panels and waiting 4.5 seconds produced zero further Power requests in either client. No power mutation was sent. |
| Boot slow initial load | Both clients opened Boot while the first slow WS-MAN operation timed out. The shared retry recovered the authoritative default/once snapshot; discovery advanced from revision 1 `discovering` to revision 2 `transport_unavailable`. After the terminal state, both clients produced zero Boot requests during a five-second observation window. |
| MSD cross-client mutation | Client A mounted the managed `gpt-flash-probe.img` as flash. Client B showed the same removable attachment without reopening; the BMC issued 36 reads covering 297 blocks. Client B disconnected it and client A returned to Empty within 2.5 seconds. MSD revision reached 8 with no fault. |
| MSD ownership | While two panels were open, each used the shared one-second, completion-scheduled discovery chain. Closing both panels and waiting 3.5 seconds produced zero further MSD status requests. |
| MSD external owner | A temporary second aggregator mounted `gpt-flash-probe.img` as flash. Both 8400 browser clients changed to the external-occupancy view with no target/image fabrication and no Mount controls. The status endpoint returned `occupancy.state=external` in about 1.2 ms instead of waiting 3/15 seconds. Unmount cleared the mapping; closing the temporary aggregator released its still-live AVMP transport, after which 8400 reacquired the session in about 0.73 seconds and both already-open dialogs automatically restored the Optical/Removable target list. |
| Device route scope | Client A changed from iDRAC8 to iDRAC6. iDRAC6 Identify loaded as Off under its independent epoch 11. After clearing request history, four seconds produced no iDRAC8 feature requests and one current iDRAC6 Identify refresh. |
| WebSocket/page reconnect | Reloading the iDRAC6 console rebuilt the device-scoped socket and resource leases; reopening UID recovered the authoritative Off state through HTTP instead of retaining the pre-reload in-memory snapshot. |

Backend closure acceptance on 2026-08-06:

| Case | Result |
|---|---|
| Mahogany `STORAGE_STATUS` | Protocol `0x89` updates the pooled authoritative attachment/status snapshot and publishes one `status` resource revision. Byte-identical reports are deduplicated. Parser, transition, and duplicate tests pass. Real Mahogany hardware remains unavailable in this lab configuration. |
| Mahogany media serving | The production backend retains a validated ISO handle, binds a storage listener on the KVM connection's local interface, sends `StorageClientConnect (0x99)`, and commits only after both the `0xFE` storage-peer setup and authoritative `StorageStatus (0x89)` confirmation. Disconnect uses `0x9A`, waits for status, preserves the peer on disconnect errors for retry, and rolls failed mounts back. READ(10) is bounds checked and streamed in 4096-byte protocol chunks; MMC opcode, continuation, peer reconnect, privilege, mount, rollback, and unmount tests pass. Real Mahogany hardware interoperability remains pending because this lab has no Mahogany fixture. |
| Mahogany firmware | Protocol `0xC9` validates its u32 little-endian byte length, decodes the UTF-8 version, stores it in the shared pool, and publishes the generic firmware inventory resource. Complete and truncated message tests pass. Real Mahogany hardware remains unavailable in this lab configuration. |
| Redfish firmware | Mock interoperability follows ServiceRoot -> UpdateService -> FirmwareInventory -> member and validates version/updateability/health mapping. iDRAC8 hardware content validation is pending because the test runner could not establish TCP to the fixture on 2026-08-06. |
| Redfish health/sensors | Mock interoperability covers Chassis health, legacy Thermal arrays, inferred units, and standard Sensors collection members. iDRAC8 hardware content validation is pending for the same connectivity reason. |
| HID LED contract | Mahogany `0xD5` is the client-visible Num/Caps/Scroll state source. iDRAC `0x0204` remains a private no-response focus initialization record and is not represented as readable LED state. |

The legacy iDRAC management page and AVMP login expose the global active/busy
fact but not another AVMP session's image name or target mapping. The accepted
contract therefore reports external occupancy and automatic recovery without
fabricating an attachment. It does not claim exact third-party media discovery
or per-target eject. Dell's management `Attached=Detached` setting disconnects
all virtual-media sessions and changes global configuration; it is not exposed
as a precise Disconnect operation.

Do not record addresses, passwords, tokens, certificate keys, unredacted packet
captures, or video/HID payloads. Use a device alias.

## Read-only aggregator probe

After configuring and starting the aggregator, run the read-only probe below.
It uses the credentials already held by the running service; it neither accepts
nor prints BMC credentials, and it does not send power, HID, media, or stream
mutations.

```bash
BMC_AGGREGATOR_URL=http://127.0.0.1:8080 \
  nix develop --command just verify-idrac lab-idrac8
```

Save only the resulting selected JSON fields against the device alias. A
successful probe is evidence of feature registration and status discovery, not
evidence that a video frame or input action worked; complete the applicable
hardware cases below before marking a matrix row supported.

| Alias | Model | iDRAC/Firmware | Native AVCT video/HID | IPMI power | Redfish power | Redfish media | Date | Evidence |
|---|---|---|---|---|---|---|---|---|
| `example-idrac6` | pending | pending | pending | pending | n/a/pending | n/a/pending | pending | redacted result link |
| `example-idrac7` | pending | pending | pending | pending | pending | pending | pending | redacted result link |
| `example-idrac8` | pending | pending | pending | pending | pending | pending | pending | redacted result link |
| `lab-idrac8-20260728` | iDRAC 8 | firmware not recorded | Native AVCT `0x008a` RGB-RLE: control/video channels connected; 58/58 packets decoded, 58 raw frames published, and MJPEG bytes emitted. Visual inspection, HID, and reconnect remain pending. | power status read verified; no action invoked | discovery verified; action not invoked | collection present but standard Insert/Eject absent | 2026-07-28 | redacted local result |
| `idrac8-lab-20260731` | iDRAC 8 | firmware not recorded | Fresh service session reached 1024x768, decoded/published 94 frames with zero decode errors, emitted at least 64 KiB MJPEG, exposed 11 source color-depth profiles, accepted HID reset, and repeated first-frame acceptance after graceful service restart. Native socket-loss reconnect was not induced. | status `on`; no disruptive action invoked | not exercised in this pass | optical plus shared removable floppy/flash discovery passed; no-card vFlash unavailable path passed | 2026-07-31 | local API/trace acceptance |
| `idrac6-lab-20260731` | iDRAC 6 | firmware not recorded | Fresh service session reached 1024x768, decoded/published 87 frames with zero decode errors, emitted at least 64 KiB MJPEG, exposes only the hardware-validated `variant-1-15` profile, accepted HID reset, and repeated first-frame acceptance after graceful service restart. Native socket-loss reconnect is not advertised for this profile. | status `on`; no disruptive action invoked | n/a in this pass | fixed optical discovery passed; no-card vFlash unavailable path passed | 2026-07-31 | local API/trace acceptance |
| `idrac6-dvc15-startup-20260731` | iDRAC 6 | firmware not recorded | The typed startup state reached `ready` after suppressing 23 pre-ready display packets, retained one absolute deadline until readiness, selected only wire family `0x0081`, and published an 800x600 DVC15 frame with zero decode errors and no active fault. An earlier run produced and visually verified a complete 640x480 GRUB MJPEG frame. Explicit no-signal suspend/resume is call-site tested; a physical power-cycle was not performed in this pass. | not exercised | n/a | not exercised | 2026-07-31 | local API trace plus decoded JPEG |
| `idrac8-video-regression-20260731` | iDRAC 8 | firmware not recorded | The unchanged unmanaged startup policy published 1024x768 wire family `0x0081` frames with zero decode errors and no active fault; a decoded MJPEG image visually matched the host login console. | not exercised | not exercised | not exercised | 2026-07-31 | local API trace plus decoded JPEG |
| `idrac6-color-depth-20260804` | iDRAC 6 | firmware not recorded | Cross-family probes confirmed the official 11-entry matrix: Performance 4-7 and Max Video 7/9/12/15/18/21/23. All accepted profiles produced correct 1024x768 frames and zero decode errors. The appliance's `0x0083` payload is byte-identical to DVC15 despite the GRAY7 envelope; `0x0082` Max Video 7 was validated by official-DVC7 versus RGB555 differential rendering. Max Video 4-6 and Performance 9-23 were ignored and recovered to DVC15 rather than exposed as false successes. | not exercised | n/a | not exercised | 2026-08-04 | raw payloads, cross-family probes, official-decoder differential, browser frames, live API trace |
| `idrac8-color-depth-20260804` | iDRAC 8 | firmware not recorded | Raw `0x8204` applied-state checks confirmed 14 profiles: Performance 4-7 and Max Video 4/5/6/7/9/12/15/18/21/23. All produced correct 1024x768 frames with zero decode errors. Performance 9-23 left the previous mode unchanged and is not exposed. Observed standard wire families were `0x0083`, `0x0082`, `0x0081`, and `0x008a` respectively. | not exercised | n/a | not exercised | 2026-08-04 | opposite-state probes, raw status, browser frames, live API trace |
| `workspace-crates-idrac6-20260805` | iDRAC 6 | firmware not recorded | Post-crate-migration service used `idrac6-apcp`, reached DVC startup `ready`, decoded/published 318 1024x768 frames with zero decode errors and no terminal reason, and accepted HID reset. | read-only status connected | n/a | fixed optical discovery and real media lifecycle verified below | 2026-08-05 | live API/status/trace acceptance |
| `workspace-crates-idrac8-20260805` | iDRAC 8 | firmware not recorded | Post-crate-migration service used `idrac8-apcp`, decoded/published 270 1024x768 frames with zero decode errors and no terminal reason, and accepted HID reset. | read-only status connected | not exercised | optical plus shared removable discovery and real media lifecycle verified below | 2026-08-05 | live API/status/trace acceptance |

Legacy AVMP media evidence is tracked separately because it is independent of
Redfish media discovery:

| Alias | Generation | Media result | Date |
|---|---:|---|---|
| `lab-idrac6-media` | 6 | Optical map accepted and the BMC issued 13 read requests covering 26 blocks. Floppy map also succeeded through the request-specific VDiskInfo refresh and advanced the session totals to 15 requests covering 106 blocks. Both release paths completed and the session remained healthy after the late status update. | 2026-07-30 |
| `lab-idrac8-media` | 8 | Initial discovery returned two disabled type-zero placeholders. The BMC accepted `1 floppy + 1 CD`, omitted `0x8241`, and converged through `VDiskInfo` to idle type `2 + 1`; `0x8410` was not used as completion. Floppy map succeeded, followed by 14 BMC reads covering 98 blocks. Fire-and-forget release returned immediately and the session remained healthy and unmapped after late traffic. | 2026-07-30 |
| `lab-idrac6-dual-slot` | 6 | Optical and floppy were mounted simultaneously in one AVMP session. Per-drive routing recorded optical 26 reads/52 blocks and floppy 15 reads/106 blocks. A transient BMC rejection immediately after an older session ended surfaced as `mount_failed` without disturbing optical; a clean retry completed normally. | 2026-07-30 |
| `lab-idrac8-dual-slot` | 8 | Optical and floppy were mounted simultaneously. Per-drive routing recorded optical 12 reads/24 blocks and floppy 14 reads/98 blocks. Unmounting optical retained the floppy attachment and lifecycle guard; remount restored both slots and both capabilities. | 2026-07-30 |
| `lab-idrac6-flash` | 6 | A managed FAT image was mapped through `VDiskRequest2` as generic `flash` media on the type-1 removable target. The BMC issued 15 reads covering 106 blocks. The API exposes one `removable(floppy, flash)` slot because both modes share this target. | 2026-07-30 |
| `lab-idrac6-removable-switch` | 6 | Regression reproduced a delayed `ServerImageInfo(0x8500)` incorrectly terminating AVMP before map. Mount refresh was separated from persistent-storage probing, delayed `0x8500/0x8530` records became normal state updates, and the non-official release `ClientStatus(7)` was removed. A 1.44 MiB image then mounted as floppy and served 13 reads/104 blocks; Disconnect recycled the final iDRAC6 media session, immediate remount as flash served 13 reads/104 blocks, and final Disconnect left zero attachments/mappings with no fault. | 2026-08-02 |
| `lab-idrac6-late-vdiskinfo-map` | 6 | The API reproduced a map race after a requested refresh returned type `2 + 1`: an unsolicited type-2-only `VDiskInfo` arrived before backend dispatch and the second status lookup rejected flash without sending its map request. The backend now consumes the drive snapshot verified for that operation, matching the official JAR's `getVDiskInfoAll` -> select remote id -> map ordering. After replacing the sole 8400 test instance, `probe.img` mounted as flash and the BMC issued 13 reads covering 104 blocks; status reported the `removable` flash attachment with no fault. | 2026-08-03 |
| `lab-idrac6-hidden-sibling-target` | 6 | With optical already mounted, capability refresh returned only the mapped type-2 entry and hid the still-usable idle type-1 sibling. The backend retained only topology previously advertised in the same authenticated AVMP session and continued to require BMC `DeviceStatus` success. Optical plus flash mounted simultaneously (`74 reads / 148 blocks` and initially `20 / 160`, then `24 / 192` after remount). While optical remained mounted, removable switching `flash -> floppy -> flash` succeeded and status ended with `fault=null`. | 2026-08-03 |
| `lab-idrac8-flash` | 8 | The exact native `SetDeviceCount(1 floppy, 1 CD, 1 flash)` request was accepted through `DeviceStatus=0`; refreshed discovery exposed type `2 + 1`. The type-1 target accepted a generic `flash` map and issued 14 reads covering 98 blocks. Optical remained independently usable and issued 12 reads/24 blocks. The same `removable` slot was then released, remapped as floppy, and issued 49 reads/385 blocks. | 2026-07-30 |
| `lab-idrac8-selection-order` | 8 | Provisioning all three semantic counts on first activation made both Optical -> Flash and Flash -> Optical succeed. In each direction both target IDs remained distinct; optical produced 12 reads/24 blocks and flash produced at least 14 reads/98 blocks. All mappings were released afterward. | 2026-07-30 |
| `lab-idrac6-raw-capability` | 6 | The opt-in capability-one probe completed APCP and raw AVMP account login, then discovered one virtual-media drive. | 2026-07-31 |
| `lab-idrac6-raw-map-api` | 6 | The raw probe mapped optical media, served 2 BMC-read blocks, and sent release. A separate API-level mount served 26 requests/52 blocks and API unmount left zero attachments and mappings. Consecutive short-lived probes temporarily observed no idle drive until firmware reclaimed the previous session; a clean retry succeeded. | 2026-07-31 |
| `lab-idrac8-api-dual-slot` | 8 | API discovery exposed fixed `optical` plus one shared `removable(floppy, flash)` slot and no dynamic-target creation. Simultaneous optical and floppy mounts produced 12 requests/24 blocks and 37 requests/289 blocks respectively. Removing optical retained the removable mapping; both final unmounts left zero attachments and mappings. | 2026-07-31 |
| `lab-idrac8-gpt-flash` | 8 | A 1 MiB GPT-partitioned raw image passed shared layout validation, mapped as `flash`, and served 33 BMC requests covering 218 blocks. Unmount left zero attachments and mappings. | 2026-07-31 |
| `lab-idrac6-fixed-targets` | 6 | Live API discovery exposed one fixed optical slot and no dynamic-target management. This firmware has no second independently addressable compatible drive, so dynamic create/delete is correctly unavailable rather than untested. | 2026-07-31 |
| `lab-idrac8-fixed-targets` | 8 | Live API discovery exposed fixed optical plus one shared removable slot and no dynamic-target management. The physical layout cannot substantiate multiple logical targets of one media type, so create/delete is correctly unavailable. | 2026-07-31 |
| `workspace-crates-idrac6-media` | 6 | After moving the complete protocol implementation into `bmc-backend-idrac-legacy-media`, API discovery exposed optical, `idrac6-media-probe.iso` mounted, and the BMC issued 26 reads covering 52 blocks. Disconnect returned success; authoritative refresh ended with zero attachments, idle occupancy, `mapped=false`, and no fault. | 2026-08-05 |
| `workspace-crates-idrac8-media` | 8 | After crate isolation, API discovery exposed optical plus shared removable. `idrac8-floppy-probe.img` mounted as floppy on removable and the BMC issued 14 reads covering 98 blocks. Disconnect returned success; authoritative refresh ended with zero attachments, idle occupancy, `mapped=false`, and no fault. | 2026-08-05 |
| `lab-idrac8-msd-lifecycle-guard` | 8 | A real floppy mapping served 28 BMC reads covering 217 blocks. While occupied, repeat `set_image`, repeat `connect`, two concurrent mount requests, and target deletion all returned HTTP 409 `target_mounted` before backend replacement. Final unmount left zero attachments and mappings. | 2026-07-31 |
| `lab-idrac6-msd-lifecycle-guard` | 6 | A real optical mapping was accepted and published as the sole attachment. Two concurrent replacement requests both returned HTTP 409 before backend dispatch. The host did not issue a media read during the two-second observation window. Final unmount left zero attachments and mappings. | 2026-07-31 |

The `flash` result above is client-served removable media. Dell VFlash SD-card
partition administration and AVMP ServerImage storage are separate persistent
BMC-storage capabilities; neither is implied by a successful flash mapping.

### ServerImage probe evidence

| Alias | Generation | Result | Date |
|---|---:|---|---|
| `lab-idrac6-server-image-read` | 6 | Read-only `ServerImageInfo` returned `MAS001`, `MAS002`, `MAS003`, `MAS004`, `MAS005`, `MAS021`, and `MAS022` at indexes 1 through 7. All have type 4, format 0, flags 3, and store 1. Their raw size fields are 71680, 29298, 107421, 117187, 419840, 307200, and 327680. `ServerImageSpace` returned total/free 0/0 for official store 16 and entry store 1. | 2026-07-31 |
| `lab-idrac8-server-image-read` | 8 | Read-only `ServerImageInfo` returned `MAS001`, `MAS002`, `MAS005`, `MAS021`, `MAS022`, `MAS024`, and `MAS025`. Indexes 1 through 6 use store 1; `MAS025` uses index 49/store 4. All have type 4, format 0, and flags 3. Raw size fields are 102400, 30720, 1024000, 614400, 512000, 20480, and 122880. `ServerImageSpace` returned total/free 0/0 for stores 16, 1, and 4. | 2026-07-31 |
| `lab-idrac8-server-image-create` | 8 | A guarded nonempty-inventory override sent `ServerImageCreate` for unused index 7. Firmware returned extension 0/status 1 and rejected it. A post-request inventory read returned the same seven entries, confirming no partial creation or BMC-state change. Create support is therefore not advertised. | 2026-07-31 |

ServerImage read-only interoperability is verified for both generations.
Create/upload/delete remains unsupported. The available iDRAC8 is outside the
official empty-inventory precondition and explicitly rejected the hardened
nonempty-inventory create request without changing its inventory.
The protocol evidence does not establish a byte multiplier for the raw size
field, so the matrix does not convert it to bytes or MiB. Zero space responses
mean neither firmware advertises writable capacity through this AVMP API.

The conditional BMC-storage implementation is complete but **awaiting vFlash
hardware acceptance**. Mock-transport tests cover nonzero-space capability
publication, empty-inventory create, two 32 KiB-oriented data chunks,
zero-length finalize, refreshed inventory, confirmed delete, and nonzero status
failure. A future card-equipped device must verify list/upload/delete and
capacity changes before this row can be marked hardware-supported. ServerImage
select/map remains unimplemented and is not advertised.

### Test-path limitations found

- Aggregator multipart upload and URL import now stream into same-filesystem
  staging files with a per-device configurable ceiling that defaults to 64 GiB. They no longer retain a
  complete image in memory. A 3 MiB regression covers payloads above Axum's
  former 2 MiB default; boundary tests verify that the first excess byte maps
  to HTTP 413 and the unified non-retryable `upload_too_large` storage fault.
  MD5, SHA-1, SHA-256, and SHA-512 are calculated for user comparison.

### Transfer acceptance matrix

| Case | Evidence | Result |
|---|---|---|
| Configured upload ceiling | Device `features.msd.feature_params.max_image_bytes=8`; nine-byte multipart payload | HTTP 413, storage-domain `upload_too_large`, operation `upload_image`, non-retryable |
| Axum legacy boundary | 3 MiB multipart payload | Passed above the former 2 MiB extractor limit |
| Large-file storage width | Sparse 4 GiB + 4096-byte staged image | Atomic publish preserved the exact `u64` size without allocating image-sized memory |
| Resumable browser upload | Create, PATCH six bytes, recreate router, PATCH remaining five bytes | Completed `hello world`; persisted offset was authoritative |
| Offset conflict | PATCH with stale/nonmatching offset | HTTP 409 `upload_offset_mismatch`; no append |
| Concurrent PATCH | Two simultaneous requests at offset zero | Per-session lock admitted one append; the other returned HTTP 409 |
| Interrupted PATCH rollback | Body failed after three uncommitted bytes, then retried at offset zero | Staged tail was truncated and final content contained only the successful retry |
| Storage write failure | Synthetic open/write/flush failure mapping | HTTP 507 `upload_write_failed`, retryable; filling the host filesystem was deliberately not performed |
| Cancellation cleanup | Partial PATCH followed by DELETE | Session became cancelled and `.part` data was removed |
| Generic BMC store completion | Resumable upload to mock BMC-owned location | Typed `MsdMediaStore` received the staged file; later catalog list recovered feature-owned digests |
| Digest correctness | `hello world` | MD5, SHA-1, SHA-256, and SHA-512 matched published known vectors |
| URL transient resume | Source interrupted after five bytes | Second request used `Range: bytes=5-` and matching `If-Range`; final bytes were `helloworld` |
| URL runtime-loss resume | Four failures, in-memory task map cleared, persisted checkpoint resumed | Status returned `paused` at offset five and a later Range request completed the image |
| Full regression | Rust workspace / architecture / integration / frontend lifecycle / WebRTC | `cargo test --workspace` passed across the application and all isolated backend crates (4 hardware-gated tests ignored); architecture 63/63 and `--features test-utils --test integration` 103/103 passed. The latest binary build and `cargo check` passed. |

URL checkpoints are stored mode `0600`, checkpoint every 8 MiB and on every
interruption, and are removed on completion or cancellation. Browser upload
sessions persist their offset sidecars and the frontend retains session IDs in
local storage. Abandoned active sessions and URL checkpoints expire after seven
days; terminal upload records expire after 24 hours. Tests simulate router
recreation and complete runtime task loss.

### Evidence-gated exclusions

The remaining AVCT sharing/preemption/chat, OTP launcher policy, auxiliary
display controls, AVMP smart-card, writable mapping, and native-buggy alternate
TOC paths do not have sufficient official product semantics and remain
explicitly unsupported. They are not silently ignored and must return a
structured fault if encountered. This is the accepted safe boundary, not an
unfinished guessed implementation. iDRAC7 and auth remain separately deferred.

- After the availability implementation, both no-card lab devices returned no
  usable `bmc` location and one unavailable location with reason
  `media_not_present` plus localization key `msd.idracVflashUnavailableHint`.
  This negative capability/UI path is hardware-verified on iDRAC6 and iDRAC8.

### HID and power follow-up evidence

| Alias | Result | Date |
|---|---|---|
| `lab-idrac6-hid-matrix` | USB HID usage key events visibly produced `test` at the host login prompt; Shift+A visibly produced uppercase `A`. Control and Alt press/release edges, absolute and relative pointer writes, left/middle/right press/release, both wheel directions, and reset all completed through the native AVCT backend. Pointer semantics were accepted by the transport but were not visually observable on the text console. | 2026-07-31 |
| `lab-idrac6-sparse-mjpeg-input` | A deterministic GRUB repro showed every HID WebSocket event reaching AVCT TLS write/flush within about 1 ms while native browser MJPEG remained exactly one decoded frame behind. The HTTP source decoded and encoded one fresh frame per key. Replacing native `<img>` multipart consumption with a bounded Content-Length `ReadableStream` parser made isolated H and I presses visible in their own 200 ms screenshots without a following key/frame. The server now emits conventional self-contained multipart parts; parser tests cover immediate final-part delivery, arbitrary network fragmentation, boundary-like JPEG bytes, malformed headers, and the complete Fetch-to-Blob publication path before stream completion. | 2026-08-01 |
| `lab-idrac8-hid-follow-up` | The same keyboard writes completed and AVCT outbound counters advanced, but the host login screen did not visibly echo input. Device-side console state remains uncertain, so keyboard/pointer acceptance is not claimed. | 2026-07-31 |
| `lab-idrac6-ipmi-acpi-soft` | The backend sent the standard IPMI Chassis Control `AcpiSoft` operation for `graceful_shutdown`; this BMC rejected the command, the API returned retryable `control_failed`, and power remained `on`. No reset action was invoked. | 2026-07-31 |
| `lab-idrac6-boot-identify` | Differential `ipmitool lanplus` testing identified the missing `Set Session Privilege Level` step. After aligning the shared client, Boot Parameter 5 read succeeded and one-second Chassis Identify completed. | 2026-07-31 |
| `lab-idrac8-boot-identify` | Differential `ipmitool lanplus` testing identified the missing `Set Session Privilege Level` step. After aligning the shared client, Boot Parameter 5 read succeeded and one-second Chassis Identify completed. | 2026-07-31 |

### Concrete firmware boot-entry discovery

| Alias | Generation | Discovery result | Selection result |
|---|---:|---|---|
| `lab-idrac6-boot-entry-discovery` | 6 | After Lifecycle Controller 1.7.5.4 and inventory collection, WS-MAN returned ten enabled real sources: three IPL, three BCV, and four UEFI entries. `DCIM_BootConfigSetting` publishes `OneTime`; all ten Dell-defined source families are selectable. Browser-visible ids are opaque SHA-256 values. | `ChangeBootOrderByInstanceID`, selector `OneTime`, accepted and restart-safely recovered all three paths without a host reboot: UEFI fixed disk (`OneTimeUefiBootSeqDev`, `efi=true`), IPL hard drive C (`OneTimeBootSeqDev`, `efi=false`), and BCV SATA disk (`OneTimeHddSeqDev`, `efi=false`). The original UEFI fixed-disk selection was restored after validation; all final status responses had `fault=null`. |
| `lab-idrac8-boot-entry-discovery` | 8 | Redfish was disabled on this fixture. Dell WS-MAN returned five real firmware entries through `DCIM_BootSourceSetting`; the generic Redfish adapter was not involved. | Selecting the current local system disk through `ChangeBootOrderByInstanceID` returned success without a host reboot. After reconnect, BootConfig recovered the selected one-time entry; entry count remained five and `fault=null`. |

The device-level requirements API and UI are implemented for this discovery
precondition. Before the Lifecycle Controller update, the tested iDRAC6 state
mapped to `firmware_inventory_collection/unavailable`; after the update and
inventory collection it maps to `satisfied`. The iDRAC8 state also maps to
`satisfied`. Unit tests verify disabled, pending, enabled,
unlicensed, and unknown states, generation-specific SetAttribute(s) and
CreateConfigJob bodies, exact return values, and the absence of
`RebootJobType`. HTTP contract tests cover devices with no providers and
unknown devices.

Live iDRAC6 remediation was explicitly authorized and exercised on 2026-08-02.
`SetAttribute` returned `ReturnValue=0` and `Set PendingValue`. The firmware
schema advertises `ScheduledStartTime` and `RebootIfRequired`, but the official
2.92 provider binary does not read method inputs; supplying those schema fields
reproducibly returned `CMPI_RC_ERR_INVALID_PARAMETER`. An empty-input
`CreateConfigJob` returned `4096`, and lifecycle job `JID_001785647340`
completed with `LC001`. The official `lcsetconfig` binary was then traced to
OEM IPMI write `0x30/0xA3 0x11 0x01`, paired with read
`0x30/0xA3 0x10 0x00 0x00`. Direct execution returned success, but the read
state remained zero after two graceful shutdown/power-on POST cycles. Because
the same LC reports `Licensed=No`, the backend now treats this machine as
unavailable instead of repeatedly offering a remediation which its firmware
silently fails to apply.

After Lifecycle Controller 1.7.5.4 was installed and inventory collection
completed on 2026-08-03, the same device began publishing the ten boot sources
and `OneTime` BootConfigSetting recorded above. Protocol tests cover exact
method/action/selector/source XML, escaping, `IsNext=3`, Dell non-zero return
faults, enabled IPL/BCV/UEFI source-family selectability, transient capability retention, and raw
InstanceID non-disclosure. Hardware also returned a temporary
`OneTime:<original UEFI InstanceID>` BootSourceSetting after selection; the
adapter now uses that exact embedded id for restart-safe selection readback and
filters the mirror from inventory.

Earlier read-only UI acceptance verified the confirmation surface and layout.
After the Lifecycle Controller update, iDRAC6 boot inventory is available.
The iDRAC8 validation changed only its one-time next-boot selection to the
already selected local system disk; it did not reboot the host.

These rows are discovery evidence, not a claim that every iDRAC firmware exposes
the same inventory. Standard BootOptions devices use `UefiTarget` only when a
real `UefiDevicePath` is published. Dell OEM BootSources devices require a
writable device registry, exact source mapping, accepted Dell method result,
and independent next-state readback before entries are marked selectable.
Boot status returns category-level selection immediately with
`entry_discovery=discovering`; the bounded background probe later publishes the
terminal discovery state. This prevents Redfish and WS-MAN fallback latency
from serially blocking the feature status endpoint.
The scoped legacy connector caps negotiation at TLS 1.2, allows the measured
slow management handshake, uses preemptive Basic with Digest fallback, and
fully closes each Hyper connection before opening the next legacy request.

Forced native socket-loss remains pending. The unprivileged test process was
not permitted to destroy the established TCP sockets, and the failed attempt
did not interrupt either console session. Graceful service restart evidence is
not a substitute for this case.

## Native AVCT acceptance cases

- Connect using only the Web-account credentials and AVCT port; no JNLP, ticket,
  or VNC server configuration.
- Render an initial frame and incremental updates at the negotiated resolution.
- On iDRAC6, start once from each persisted DVC7/DVC7_GRAY state and verify the
  trace reaches `dvc.startup_phase=ready`, publishes only DVC15, and reports no
  decode or protocol fault. Verify a silent negotiation produces
  `idrac6_dvc15_negotiation_failed` instead of an unbounded first-frame wait.
- Establish the shared AVCT pool through HID first, subscribe video afterward,
  and verify the retained first frame is replayed without waiting for a new
  display update. Then verify `video_stopped` does not replay that stale frame,
  and that the first following DVC15 record restores video without waiting for
  another `0x0084` notification or a frontend refresh.
- Verify keyboard press/release, modifiers, consumer usage, pointer movement,
  buttons, wheel, and reset do not leave stuck input.
- With sparse screen updates, verify each key's decoded MJPEG frame becomes
  visible without waiting for another key or source frame.
- Drop one AVCT socket and verify both feature lifecycles disconnect, then
  reconnect without duplicate console sessions or leaked tasks.
- Verify the scoped legacy TLS path does not affect generic VNC/Redfish policy.
- Verify a raw or unknown display mode fails safely; unknown modes must never be
  decoded as a guessed image format.

## Power and media acceptance cases

- Read power status and exercise only explicitly chosen, permitted actions.
- For Redfish, discover ServiceRoot, Systems, Reset AllowableValues, and
  VirtualMedia actions; do not assume Dell member identifiers.
- For media, verify BMC reachability of the image URL, completion/failure, and
  eject cleanup. If Insert/Eject is absent, report the capability unavailable.

## Exit criteria

The shared AVCT implementation may be selected for iDRAC 6/7/8, but each
generation/firmware remains **pending operational support** until its hardware
row passes. Standard `vnc`, `ipmi`, and `redfish` remain independent,
capability-driven backends; a result for one does not imply a result for AVCT.
