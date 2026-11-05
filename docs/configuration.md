# Configuration Reference and Protocol Backends

The schema is defined by `src/config.rs`. Feature configuration lives under
`devices[].features.<feature>` and separates `backend`, `backend_params`,
and `feature_params`.

## Minimal startup configuration

Save this as the local, ignored `config.yaml` at the repository root:

```yaml
server:
  listen: "127.0.0.1:8080"
  frontend_dir: ./frontend/dist
  data_dir: ./data
devices: []
```

Build with `nix develop --command just build`, then run
`./target/release/bmc-aggregator --config config.yaml` from the same directory.
Open `http://127.0.0.1:8080`; add devices through the settings UI or YAML.
Relative paths resolve from the process working directory. `listen` and
`frontend_dir` are required; `data_dir` defaults to `./data`.

The HTTP listener rejects non-loopback bindings unless `allow_insecure_http`
is explicitly enabled. Application authentication is a dummy compatibility
flow, so login does not secure a deployment. Protect access externally before
exposing the service. WebTransport is disabled by default and needs its own
certificate/listener configuration; see the video transport implementation in
`src/bmc/feature/video/webtransport.rs`.

Use environment placeholders for all BMC credentials:

```yaml
auth:
  username: ${BMC_USERNAME}
  password: ${BMC_PASSWORD}
```

The loader rejects missing environment variables. Do not place real credentials
in source-controlled YAML.

## iDRAC preset

`protocol: idrac` materializes the native AVCT console for video/HID and IPMI
for power/identify, plus IPMI and WS-MAN Boot providers. The AVCT backend uses the same BMC account as `auth`; it has no VNC
password, JNLP URL, or session-ticket field.

The following is a device configuration fragment for the iDRAC8 profile; supply
`id`, `name`, `host`, `port`, and `auth` in the enclosing device entry. Use the
source-confirmed generation for the actual appliance.

```yaml
protocol: idrac
features:
  video:
    backend: idrac-avct-console
    backend_params:
      idrac_generation: 8
      avct_port: 5900
      avct_tls_mode: idrac-legacy-rsa
      avct_virtual_media_privilege: false
  hid:
    backend: idrac-avct-console
    backend_params:
      idrac_generation: 8
      avct_port: 5900
      avct_tls_mode: idrac-legacy-rsa
  power:
    backend: ipmi
    backend_params:
      port: 623
  boot:
    providers:
      - id: ipmi
        backend: ipmi
      - id: wsman
        backend: idrac-wsman
        backend_params:
          idrac_generation: 8
  msd:
    backend: idrac-legacy-media
    backend_params:
      idrac_generation: 8
    feature_params:
      # Per-image upload and URL-import limit. Defaults to 64 GiB.
      max_image_bytes: 68719476736
```

`avct_port` must be an integer from 1 to 65535. `avct_tls_mode` currently
accepts only `idrac-legacy-rsa`; there is no downgrade or fallback to generic
VNC. An explicit `video.backend` or `hid.backend` remains authoritative.
`avct_virtual_media_privilege` controls the final privilege byte of Dell's
direct AVCT login request and defaults to `false` for `protocol: idrac`.

The normal profile needs no APCP reconnect settings. Advanced deployments may
set the source-backed launcher values `avct_reconnect_capability`,
`avct_reconnect_version`, `avct_apcp_selector`, and
`avct_reconnect_preference`. They select an exact Dell APCP writer; they are
not protocol tuning knobs. Do not set them from packet guesses. With no
explicit reconnect capability the backend uses the official 53-byte initial
APCP request.

The default WS-MAN Boot provider also requires an explicit `idrac_generation`
of `6` or `8`; the preset alone does not supply it. Set it in that provider's
`backend_params`, as above, or explicitly select IPMI-only Boot.

For `msd.backend: idrac-legacy-media`, set `idrac_generation: 6` or
`idrac_generation: 8` in that feature's `backend_params`. The iDRAC6 Java and
iDRAC8 native AVMP APCP/discovery forms are intentionally separate. iDRAC7 is
rejected until an official iDRAC7 client artifact establishes its wire profile.

For iDRAC6, the official Java VM client always sends an empty AVMP `0x0400`
after ten seconds without a successful outbound AVMP write. The backend applies
that fixed idle heartbeat automatically; `avmp_heartbeat_interval_s` may be
omitted or set to `10`, but no other iDRAC6 value is accepted. For profiles
with a separately source-confirmed positive heartbeat interval, set
`avmp_heartbeat_interval_s` to that exact value. It is an idle interval, not
an APCP tail field.

For iDRAC8 native AVMP only, `avmp_reconnect_method` may be `0`, `1`, or `2`.
These are the official launcher's raw `VMConnectionInfo.reconnectMethod`
values: only `1` selects APCP setup selector `1`; `0` and `2` select `2`.
It is not a retry count and is ignored by the terminal iDRAC6 profile.

`redfish -> msd` is opt-in. Configure it only after the BMC advertises standard
VirtualMedia actions. It is not implied by `protocol: idrac`.

For a non-iDRAC standard RFB endpoint, configure `video.backend: vnc` and
`hid.backend: vnc` explicitly. For a standard Redfish virtual-media device,
configure `msd.backend: redfish` and submit an absolute HTTPS media URL via the
shared MSD panel. The HTTP API represents this as
`source: { location: "remote", url: "..." }`; `image_url` is not a top-level
MSD request field.

See [the iDRAC device profile](devices/idrac.md),
[Generic VNC backend](backends/vnc.md), [Generic Redfish backend](backends/redfish.md),
and [BMC transport security](security/bmc-transports.md).
For Dell's non-Redfish virtual-media protocol, configure the MSD feature
explicitly with `backend: idrac-legacy-media`. `avct_vm_port` defaults to the
configured `avct_port` (or `5900`) and is intentionally separate from the
console setting for nonstandard appliances.

## Configuration durability

Runtime configuration mutations are serialized through one application
transaction. The complete candidate is written to a unique same-directory
temporary file with mode `0600`, flushed, atomically renamed, and followed by
a parent-directory sync. Failures before rename retain the previous configuration.
If directory sync fails after rename, the new file is already visible: memory
and the device registry follow that version, while uncertain crash durability
is logged. A successful API response does not eliminate this filesystem limit.

## Settings API and UI ownership

The current settings UI has two scopes:

- `/settings` owns browser appearance and aggregator service settings.
- `/devices/:id/settings` owns device identity, credentials, and explicit
  feature/backend assignments. `/devices` is the inventory and
  `/devices/new` is the catalog-driven creation flow.

The server describes editable backend fields through `GET
/api/configuration/catalog`. The catalog is metadata only: backend crates still
own validation and protocol behavior, and the frontend does not contain a
protocol preset table. A feature assignment is either `disabled`, a `single`
backend, or an ordered `provider_set`; provider ordering and IDs are preserved
by the canonical device DTO.

Device and service configuration reads return an opaque `ETag`. Updates and
deletes must send `If-Match`; every persistent mutation must send
`X-Mutation-Id` (creation has no prior ETag). A stale ETag returns `409
configuration_conflict`; retrying the same mutation ID and payload returns the
original committed response. Secret values are never returned: use
`keep`, `replace`, or `clear` mutations. Configuration events are available on
`/api/configuration/ws` and contain only scope, epoch/revision, changed paths,
mutation ID, and affected features. The socket enforces same-origin access;
application authentication is currently a dummy compatibility path and provides
no real principal authorization. Clients use an authoritative HTTP reload after
`configuration.ready` or `configuration.resync_required` and discard delayed
lower-revision events.

Dynamic selectors use allow-listed resources such as
`/api/configuration/options/local.video_inputs`. Empty discovery is distinct
from a failed or unknown resource and each resource has its own revision. The
legacy `protocol` field remains only for YAML normalization and creation
profile hints; runtime registration consumes explicit feature assignments.

Aggregator-managed MSD images are opened relative to the device image
directory on Linux with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS |
RESOLVE_NO_MAGICLINKS)`. Backends receive the already-open descriptor and its
keepalive handle, rather than validating a pathname and reopening it later.
