# iDRAC 6/7/8 Device Profile

**Status**: `idrac-avct-console` and `idrac-legacy-media` are implemented and
hardware-verified on the recorded iDRAC6 and iDRAC8 lab devices. Support is
generation/profile and firmware evidence based; iDRAC7 remains deferred, and
the outstanding hardware cases remain visible in the compatibility matrix.
The native backends use the BMC Web-account credentials directly; they do not launch Java,
consume JNLP, depend on a Web-console ticket, or fall back to VNC. The
byte-level evidence record is
[`idrac-protocol-reverse-engineering.md`](idrac-protocol-reverse-engineering.md).

## Backend selection

An iDRAC is a profile composed from feature-specific backends:

| Capability | Default backend | Boundary |
|---|---|---|
| Remote-console video | `idrac-avct-console` | Native APCP/TLS/AVCT backend; first-frame, sustained decode, color-depth discovery, and graceful service restart are hardware-verified on the recorded iDRAC6/8 devices. |
| Keyboard and mouse | `idrac-avct-console` | Native AVCT HID backend; reset is hardware-verified, while the full keyboard/pointer/wheel and forced socket-loss matrix is tracked separately. |
| Power | `ipmi` | Default RMCP+ path; use `redfish` explicitly when the service is verified. |
| Boot selection | `ipmi` | Generic typed System Boot Options adapter; controls are unavailable when the IPMI account lacks Configure privilege. |
| UID identify light | `ipmi` | Generic typed Chassis Identify adapter with read-only state fallback when writes are denied. |
| Standard virtual media | `redfish` | Explicit opt-in after the BMC advertises standard VirtualMedia actions. |
| Standard RFB video/HID | `vnc` | Separate generic backend, selected only by explicit configuration. |
| Legacy Dell virtual media | `idrac-legacy-media` | Native generation-profiled AVMP backend; optical, floppy, flash, simultaneous compatible mappings, BMC reads, and release are hardware-verified on the recorded iDRAC6/8 devices. |

`protocol: idrac` supplies AVCT video/HID and IPMI power defaults. It does not
silently replace an explicit backend choice, and it does not add MSD merely
because a device identifies as iDRAC.

## Configuration

```yaml
devices:
  - id: rack-a-idrac
    name: Rack A iDRAC
    protocol: idrac
    host: 192.0.2.10
    port: 443
    auth:
      username: ${IDRAC_USERNAME}
      password: ${IDRAC_PASSWORD}
    features:
      video:
        backend: idrac-avct-console
        backend_params:
          avct_port: 5900
          avct_tls_mode: idrac-legacy-rsa
          avct_virtual_media_privilege: false
      hid:
        backend: idrac-avct-console
        backend_params:
          avct_port: 5900
          avct_tls_mode: idrac-legacy-rsa
      power:
        backend: ipmi
        backend_params:
          port: 623
```

`avct_port` defaults to `5900`; keep it explicit if the appliance is deployed
on a non-default port. `idrac-legacy-rsa` is the only AVCT TLS mode. Its
self-signed/legacy-cipher compatibility is scoped to this OEM socket and does
not weaken VNC, Redfish, or the web server.

The backend selects APCP and account-login forms from the official launch
profile and negotiated protocol pair. Leave the optional `avct_reconnect_*`
and `avct_apcp_selector` values unset unless they came from an official
launcher for the same appliance generation; they are not safe trial-and-error
settings.

Legacy virtual media additionally requires an explicit `idrac_generation: 6`
or `idrac_generation: 8` in the MSD backend parameters. This is required
because the supplied official iDRAC6 Java and iDRAC8 native VM clients use
different APCP setup and discovery records; iDRAC7 remains unsupported pending
an official client artifact.

To use standard Redfish VirtualMedia, add `msd.backend: redfish` only after
discovery confirms InsertMedia/EjectMedia actions. The BMC fetches the supplied
HTTPS image URL, so that URL must be reachable from the BMC management network.

## Remote color depth

`BEEF 0x0403` is an active-console-only DVC control and is deliberately not
persisted in YAML. The official encoder proves four raw control bytes and
`0x8204` reports three corresponding raw status bytes. It does *not* prove
the product semantics of the boolean fields. Consequently the UI must expose
only named, source-confirmed controls and keep all other combinations disabled
until byte-level fixtures and the official UI call path establish their
meaning. `0x8204` is applied state, not a request acknowledgement or HID
authorization message; only a newer record matching the requested raw fields
confirms that the appliance applied a change.

The recorded iDRAC6 firmware is started in the validated `variant-1-15`
(DVC15, wire `0x0081`) profile. Cross-family hardware acceptance confirms its
official eleven-entry matrix: Performance 4/5/6/7, and Max Video
7/9/12/15/18/21/23. Performance uses a refreshed `0x0083` envelope carrying
the iDRAC6 RGB555/DVC15 payload dialect. Max Video 7 uses `0x0082`, whose
payload also requires the iDRAC6 RGB555 dialect; the remaining Max Video modes
use ordinary DVC15 (`0x0081`). Max Video 4/5/6 and Performance 9 through 23 are
ignored by the appliance when requested from the opposite family and are not
exposed.

The recorded iDRAC8 accepts fourteen combinations. In addition to the eleven
official-slider entries, its `0x8204` status confirms Max Video 4/5/6. It does
not accept Performance 9 through 23. The UI keeps preference and target depth
as separate controls, but each preference lists only depths confirmed by that
backend. iDRAC8 profile changes are committed only after a newer `0x8204`
status matches both requested raw fields; a control write alone is not success.

iDRAC6 startup is an explicit bounded state machine. Before it is ready, the
backend acknowledges but does not decode or publish a persisted non-DVC15
family. After selecting DVC15 it drains the first command/payload chain; the
source-confirmed envelope bit 1 is used only as that command-reader boundary.
At the boundary the backend sends exactly one `ScreenRefresh (0x0301)`, then
waits for a DVC15 packet whose source-confirmed bit 0 resets the retained
cursor. The refreshed chain is decoded transactionally and no intermediate
framebuffer is published before its bit-1 boundary. A single absolute ten
second deadline covers ready notification, bootstrap, drain, refresh, and the
refreshed chain. Expiry or a family change surfaces the non-retryable protocol
fault `idrac6_dvc15_negotiation_failed`; it cannot remain indefinitely in
`waiting for first frame`.

An iDRAC6 runtime profile change uses the same boundary discipline. Writing
`0x0403` does not immediately send a refresh and does not complete the API
request. The backend first acknowledges and discards the reset-marked
transition chain, sends one `0x0301` at that chain's bit-1 boundary, and then
transactionally decodes the next reset-marked chain. Only the completed
refreshed chain commits the active profile. A decode failure performs the same
two-stage transition back to `variant-1-15` before returning the operation
error; a failed recovery terminates the source rather than leaving its state
ambiguous.

An explicit AVCT `video_stopped` record is not treated as failed negotiation.
It moves startup to `awaiting_signal`, removes the deadline, clears the decoder,
bootstrap state, and retained frame, and publishes `NoSignal` immediately. The
official client does not require another `0x0084` video-ready record: its DVC
handler clears the stopped marker, publishes the running state, and forwards
that same first DVC record to the decoder. The backend mirrors that data-driven
recovery for the negotiated DVC15 stream, so a valid frame becomes visible
immediately. `0x0084` remains the normal initial-start notification. Lack of
traffic is never guessed to mean a recovered signal.

The shared AVCT session also retains its latest decoded GStreamer buffer by
reference. If HID creates the shared session before the video feature, a late
video subscriber receives that retained frame. `video_stopped` and native
reconnect clear it, so a stale console image is never replayed as a live
signal.

## Operator checklist

1. Record model, iDRAC generation, firmware, licence, and the AVCT port.
2. Confirm the BMC account has virtual-console privilege.
3. Keep Web-account credentials in environment variables or a secret manager;
   never put them in a ticket, trace, commit, or browser-visible config read.
4. Independently verify IPMI/Redfish power and Redfish media capabilities.
5. Run the generation-specific video, HID, reconnect, and cleanup checks in
   [the compatibility matrix](../testing/idrac-compatibility.md).

## Troubleshooting

- An AVCT login or first-frame failure is isolated to video/HID; it does not
  imply that IPMI or Redfish is unavailable.
- Do not enable generic VNC as an implicit workaround. If a separate RFB
  service is intentionally used, configure `vnc` explicitly.
- A native AVCT disconnect transitions both feature lifecycles immediately;
  the normal reconnect policy then creates a fresh shared session.
- Capture only redacted outcomes (generation, firmware, TLS result, packet
  counters, resolution, and error class). Never capture credentials, session
  material, HID payloads, or display payloads.

See [the native protocol evidence](idrac-protocol-reverse-engineering.md),
[Redfish backend](../backends/redfish.md), and
[transport security](../security/bmc-transports.md).

Legacy Dell virtual media is deliberately opt-in: it exposes locally managed
ISO images through the aggregator rather than asking the BMC to fetch a URL.
It uses the iDRAC KVM endpoint (normally `5900`). `avct_vm_port` is an
explicit override, kept separate from `avct_port` for appliances that expose
VM on another endpoint:

```yaml
features:
  msd:
    backend: idrac-legacy-media
    backend_params:
      avct_vm_port: 5900
      avct_tls_mode: idrac-legacy-rsa
```

The native AVMP layout currently recorded in the compatibility evidence is the
iDRAC8 VM library profile. The supplied iDRAC6 Java VM client differs already
at `GetVDiskInfo(0x0200)` header extension (`0x0000` versus iDRAC8 native
`0xffff`), so virtual media must not be advertised as one generation-neutral
AVMP implementation. iDRAC7 remains unsupported until an official client
artifact establishes its profile.
