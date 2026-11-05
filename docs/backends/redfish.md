# Generic Redfish Backend

**Status**: Implemented for `power`, standard boot override/BootOptions,
standard `msd` VirtualMedia discovery, chassis identification, firmware inventory, chassis health,
and sensor telemetry.
It is specified in [ADR-0012](../adr/0012-generic-protocol-backends.md).

## Scope

`redfish` is a generic DMTF Redfish client backend, not a Dell-specific API
client. Its feature registrations are:

```text
redfish -> power
redfish -> boot
redfish -> identify
redfish -> msd
redfish -> firmware
redfish -> health
```

It does not provide a standard video or HID transport.

## Discovery-first contract

The client begins at ServiceRoot, conventionally `/redfish/v1/`, then follows
the links advertised by the service to systems, managers, and VirtualMedia
resources. It must not construct vendor resource paths or assume collection
member IDs. Collection discovery follows `Members@odata.nextLink`, rejects
pagination loops, and is bounded to 128 pages. A management endpoint that
advertises multiple Systems, Managers, or Chassis resources is rejected as
ambiguous instead of silently selecting the first member.

For power it reads `ComputerSystem.PowerState` and invokes the advertised
`#ComputerSystem.Reset` action target. The ResetType sent by the backend must
be among the service's `@Redfish.AllowableValues` or ActionInfo values. A
generic client must not guess that every reset type is supported.

For virtual media it discovers every Manager `VirtualMedia` member and verifies the
presence of `#VirtualMedia.InsertMedia` and `#VirtualMedia.EjectMedia` action
targets and maps advertised `MediaTypes` into generic media types. Transfer
protocol AllowableValues are not currently checked during discovery. Each resource URI remains a distinct generic slot; mounting or
ejecting one slot never aliases another resource.

For firmware it follows `ServiceRoot.UpdateService`, then the advertised
`FirmwareInventory` collection and each member URI. For health it follows the
single advertised Chassis member and its Thermal, Power, and Sensors links. The
health parser accepts both legacy aggregate arrays (for example
`ReadingCelsius` and `ReadingRPM`) and the standard Sensors collection; it does
not invent Dell resource paths.

These rules follow the DMTF Redfish resource model; the official schema
requires clients to use the action target and recommends exposing allowable
ResetType values for compatibility.

## Configuration contract

Use the device host/port and standard credential parameters:

```yaml
features:
  power:
    backend: redfish
    backend_params:
      host: bmc.example.invalid
      port: 443
      username: ${BMC_USERNAME}
      password: ${BMC_PASSWORD}
  msd:
    backend: redfish
    backend_params:
      host: bmc.example.invalid
      port: 443
      username: ${BMC_USERNAME}
      password: ${BMC_PASSWORD}
```

The backend uses strict HTTPS verification by default. Certificate exceptions
must use `redfish_insecure_tls: true`, are local to the device configuration,
and never become an automatic HTTP fallback. This is intended only for an
operator-verified BMC certificate migration path.

## Authentication and lifecycle

The client sends HTTP Basic credentials on HTTPS requests. It does not create
Redfish sessions or cache session tokens. This transport authentication is
independent of the application's dummy auth flow.

The client uses a 45-second connect timeout and a 60-second request timeout.
JSON/error/task bodies are limited to 8 MiB; collection traversal is limited to
128 pages. Task polling has a separate 30-second budget. There is no global
request-concurrency semaphore; the idle connection-pool limit is not such a limit.
Task responses and error messages are surfaced as redacted feature status;
tokens, Authorization headers, and passwords never enter logs, traces, or
device read APIs.

## Virtual media boundary

Redfish virtual media needs a URL that the BMC itself can reach. It advertises
the generic `remote` storage location with the `select` operation; it does not
advertise aggregator or BMC-owned storage. The selection request contains a
tagged remote source and an absolute HTTPS URL without embedded credentials.

The backend does not pass a local path to the BMC. It discovers a Manager's
VirtualMedia member and its advertised InsertMedia/EjectMedia targets before
using them. For HTTP 202 actions it follows the same-origin task Location and
waits for a terminal TaskState, honoring bounded `Retry-After` intervals. A
TaskMonitor `204 No Content` is completion; timeout, critical completion, and
terminal failure are surfaced as feature errors. Every JSON/error/task
response is streamed through an 8 MiB bound before parsing.

## Vendor extensions

If a service lacks the standard resource/action necessary for a feature,
`redfish` reports it as unsupported. Dell OEM endpoints belong in a future
`idrac-oem-*` adapter only when a real device trace and focused tests prove
that the standard model is insufficient.

See [transport security](../security/bmc-transports.md) and the
[compatibility matrix](../testing/idrac-compatibility.md).
