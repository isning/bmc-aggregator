# Generic VNC Backend

**Status**: Implemented for `video` and `hid`. It supports the deliberately
narrow RFB baseline documented below. Redfish and virtual media are separate
feature backends and are selected explicitly when the target advertises those
protocols. See [ADR-0012](../adr/0012-generic-protocol-backends.md).

## Purpose

The `vnc` backend is a generic Remote Framebuffer (RFB) client for the
video and HID features. It is not an iDRAC web-console scraper and it is not
an encoder, WebRTC, or MJPEG backend.

```text
vnc -> video: VideoSource -> VideoRuntime -> VideoProcessor
vnc -> hid:   HidBackend  -> RFB KeyEvent / PointerEvent
```

Use it with an explicitly enabled standard RFB endpoint. Native iDRAC console
support uses AVCT instead; interoperability with particular BMC generations
requires device acceptance testing.

## Configuration contract

The following schema is accepted:

```yaml
features:
  video:
    backend: vnc
    backend_params:
      host: 192.0.2.10
      vnc_port: 5901
      vnc_password: ${BMC_VNC_PASSWORD}
      vnc_security: vnc-auth
      vnc_tls: false
  hid:
    backend: vnc
    backend_params:
      host: 192.0.2.10
      vnc_port: 5901
      vnc_password: ${BMC_VNC_PASSWORD}
```

Rules:

- `vnc_password` overrides the device password; when omitted, the device
  password is used. It is never returned by read APIs.
- Port values are explicit. A UI may suggest a vendor default but the backend
  never assumes one.
- `vnc-auth` is the normal target mode. `none` requires an explicit opt-in and
  is unsuitable for production management networks.
- `vnc_tls: true` currently fails closed because TLS transport is not yet
  implemented; it never downgrades to plaintext.

## Implemented protocol support

Initial support is deliberately narrow and testable:

| Area | Target behavior |
|---|---|
| Version negotiation | RFB 3.3, 3.7, and 3.8. |
| Authentication | Standard VNC password challenge-response; no secret logging. |
| Pixel format | Request 32-bit true-colour and normalize output to RGBA. |
| Frame updates | Initial full frame plus incremental Raw updates and DesktopSize. |
| Input | RFB KeyEvent and PointerEvent; USB HID usages map to X11 keysyms. |
| Reset | Attempt to release tracked keys and buttons on reset; network loss can prevent remote delivery. |

Hextile, ZRLE, Tight, cursor pseudo-encodings, and vendor TLS variants are
added only after captured-device evidence and bounded parser tests. Raw is the
initial interoperability baseline, not a claim that every production server
will be performant enough without additional encodings.

## Lifecycle and resource rules

- Video and HID share a `VncPool` lease keyed by host, port, security mode, and
  a password fingerprint.
- The pool key and trace data never contain a plaintext password.
- Reader/writer tasks have a shutdown signal, bounded queues, and bounded
  writes. Lease release signals shutdown; the spawned session task has no
  retained join handle, so release does not await task completion.
- Mouse motion may be coalesced under pressure; key and button edges may not
  be silently discarded.
- Framebuffer dimensions, rectangle counts, byte lengths, integer products,
  and decoded work are bounded before allocation or copy.

## Diagnostics

`VideoSource::trace_snapshot()` exposes only
non-sensitive facts: RFB version, selected security mode, resolution,
frame/byte counts, lag, reconnect count, and the last redacted error.

An operator should collect the following when a connection fails:

1. VNC Server enabled state, port, and license/capability status on the BMC.
2. Whether a standard VNC client can reach the same endpoint from the
   aggregator network.
3. The configured TLS/security mode, without sharing passwords or session
   tokens.
4. A redacted handshake/error trace and BMC model/firmware version.

See [iDRAC device guidance](../devices/idrac.md) and
[transport security](../security/bmc-transports.md).
