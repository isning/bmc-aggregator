# ADR-0012: Generic Protocol Backends and Native iDRAC AVCT

**Status**: Accepted and implemented
**Date**: 2026-07-28
**Related record**: [iDRAC protocol evidence](../devices/idrac-protocol-reverse-engineering.md)

## Context

iDRAC exposes independent capability transports. Its Java remote console is the
private Dell/Avocent AVCT protocol, while VNC/RFB is an optional standard service
and IPMI/Redfish are separate management transports. A single all-purpose
`idrac` backend would couple unrelated failure modes, credentials, and lifecycles.

The architecture selects adapters per `(backend, feature)` via
`FeatureBackendTemplate`. Backends remain narrow typed implementations of
`VideoSource`, `HidBackend`, `MsdBackend`, or `PowerBackend`.

## Decision

| Backend | Features | Status | Rule |
|---|---|---|---|
| `idrac-avct-console` | video, hid | Implemented | Native APCP/TLS/AVCT console, shared only by its video/HID adapters. |
| `idrac-legacy-media` | msd | Implemented | Native AVMP VM session, explicitly configured because it provides local ISO bytes. |
| `vnc` | video, hid | Implemented | Generic standard RFB, explicit configuration only. |
| `ipmi` / `wol` | power | Implemented | Generic RMCP+ / magic-packet power paths. |
| `redfish` | power, boot, msd | Implemented | Shared ServiceRoot/action discovery; no hard-coded Dell paths. |

The native Dell backends are operationally supported for the recorded iDRAC6
and iDRAC8 profiles after official-client reverse engineering, bounded protocol
tests, and hardware interoperability acceptance. iDRAC7 remains deferred until
an official artifact and generation-specific hardware row exist.

`protocol: idrac` is a profile that defaults video/HID to
`idrac-avct-console` and power to `ipmi`. It does not enable MSD by
assumption, and it never falls back from AVCT to VNC.

The AVCT implementation lives in `crates/bmc-backend-idrac-avct`. It owns APCP,
legacy TLS compatibility, login, console packet framing, display decode, HID,
and a shared pool. Application adapters in `src/bmc/backends/idrac_avct/`
publish raw frames through `VideoRuntime`; encoder
policy and browser sessions remain owned by `VideoProcessor`.

## Consequences

- Generic VNC and Redfish remain reusable without Dell protocol details.
- A video/HID AVCT pair shares one connection only when endpoint and credential
  fingerprint match; the last lease closes it.
- AVCT disconnect signals both FeatureConnection lifecycles immediately.
- Legacy TLS/certificate acceptance is limited to the AVCT connector and must
  not be reused by generic VNC, Redfish, or HTTP clients.
- iDRAC generation support remains evidence-based. Registration means the
  protocol implementation exists; it is not a claim of hardware certification.

## Alternatives rejected

### A monolithic `idrac` backend

Rejected because it mixes console, power, and media into one runtime despite
their independent capabilities and failure domains.

### Treating AVCT as VNC

Rejected because AVCT is not RFB and requires its own APCP, login, display, and
input protocol. VNC remains useful only as an explicit generic backend.

### Separate iDRAC6/iDRAC7/iDRAC8 console backends

Rejected because the observed console family shares AVCT. Generation-specific
differences belong in evidence-driven compatibility handling, not duplicated
backend trees.

### Silent protocol fallback

Rejected because falling back to a different endpoint, credential scheme, or
security policy changes the operator-visible contract.
