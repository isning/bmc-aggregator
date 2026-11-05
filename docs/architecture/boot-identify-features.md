# Boot and Identify Feature Model

Boot selection and chassis identification are independent generic features.
They do not belong to power control: each has its own typed backend runtime,
lifecycle, capabilities, routes, and faults.

## Boot

`BootBackend` exposes typed `BootTarget`, `BootMode`, and `BootSelection`
values. Protocol integers never cross the feature boundary.

```text
GET  /bmc/:id/api/feature/boot/status
POST /bmc/:id/api/feature/boot/selection
```

The IPMI adapter uses Chassis `Get/Set System Boot Options` commands `0x09`
and `0x08`, parameter 5. `Once` and `Persistent` are separate from the boot
target, and EFI is an explicit flag. A rejected read means the backend cannot
truthfully advertise the current selection, so the UI hides Boot controls.

After RMCP+ authentication, the shared IPMI client sends App `Set Session
Privilege Level` (`0x06/0x3b`) with Administrator (`0x04`) before feature
commands. This matches the established `ipmitool lanplus` session sequence;
authentication alone does not activate the requested management privilege.

## Identify

`IdentifyBackend` exposes `Off`, `Temporary`, and `Indefinite` modes. Status is
read from the Chassis Status identify bits; writes use Chassis Identify `0x04`.

```text
GET  /bmc/:id/api/feature/identify/status
POST /bmc/:id/api/feature/identify/set
```

The IPMI adapter advertises the three modes defined by the protocol. It does not
infer identify permissions from Boot commands and does not mutate the identify
light merely to probe account permissions. The BMC authoritatively checks each
write. Completion code `0xD4` becomes an `identify_permission_denied` feature
fault; other command failures remain `identify_operation_failed` faults.

## iDRAC Profile

`protocol: idrac` expands Boot into two independent providers and Identify to
`ipmi`. The Boot feature layer combines them at the feature layer:

- Dell WS-MAN supplies concrete firmware entries.
- IPMI supplies device categories, mode flags, and BIOS Setup.

The providers remain isolated protocol crates. A selection containing
`entry_id` is sent only to WS-MAN; a category selection without `entry_id` is
sent only to IPMI. There is no retry through, or fallback to, the other
provider. Explicit `idrac-wsman`, `ipmi`, and `redfish` Boot configurations
remain single-provider choices.

The provider configurations keep the primary BMC management endpoint and
optional IPMI endpoint distinct, so an `ipmi_host` override cannot redirect
WS-MAN. There is no `idrac-boot` backend; provider composition is owned by the
Boot feature runtime.
The catalogued AVCT `NextBoot` record and Lifecycle Controller target are not
exposed because their complete product semantics and state contracts have not
been source-confirmed. BIOS Setup is exposed through the standard IPMI boot
selector `0x6`.

## Concrete firmware entries

The generic model also exposes an optional `BootEntry` inventory. An entry has
an opaque backend-owned id, display label, firmware family, generic sequence
scope, enabled state, and a `selectable` flag. `boot_sequence` represents a
top-level firmware entry while `hard_disk_sequence` represents a disk-priority
subentry. Vendor instance ids and UEFI device paths never cross the
feature boundary. Selecting an entry is restricted to `Once`; its firmware
family determines the EFI flag instead of a UI or backend policy forcing UEFI.
For Dell WS-MAN inventory, availability is derived from the device: the
`OneTime` BootConfigSetting must exist, the source must be enabled, and its
published family must be IPL, BCV, or UEFI. The current firmware boot mode is
not used as a compatibility filter because Dell formally supports all three
one-time sequence families and CSM-capable systems may publish Legacy and UEFI
sources concurrently.

The generic `redfish` adapter supports standard Redfish BootOptions. It starts
at ServiceRoot, follows the advertised Systems and BootOptions links, and uses
the documented `BootSourceOverrideTarget`, `BootNext`, and
`BootSourceOverrideEnabled` properties. It contains no Dell resource path.

The Dell WS-MAN adapter invokes `DCIM_BootConfigSetting.ChangeBootOrderByInstanceID` with selector
`InstanceID=OneTime` and the selected full BootSourceSetting InstanceID as the
single `source`. Dell `ReturnValue=0` is required; persistent `UEFI` order is
never rewritten as a substitute. Active state requires both the WS-MAN
BootConfig next-state and the exact device reported by the Dell BIOS inventory.
Management requests are serialized because tested iDRAC8 firmware processes
legacy WS-MAN TLS traffic slowly.

WS-MAN source InstanceIDs remain in a backend-private map keyed by a stable
SHA-256 opaque public id. They are neither serialized to the browser nor
stored in generic feature state. A reconnect can rebuild the map from
inventory. A transient WS-MAN-only refresh retains a previously confirmed
`OneTime` capability. The `idrac-wsman` backend accepts only ids from its own
WS-MAN inventory. The generic `redfish` and `ipmi` backends have independent
connections, capabilities, ids, and write paths; selecting one never invokes
another.

When a one-time source is active, tested iDRAC6 firmware adds a
`OneTime:<original InstanceID>` BootSourceSetting. The adapter treats this
as authoritative selection readback: it maps the embedded original id to the
opaque public id and removes the mirror from the public entry list. This lets
selection status survive an aggregator restart without inventing an eleventh
boot device.

`entry_discovery` distinguishes `discovering`, `available`, `inventory_empty`,
`inventory_unavailable`, `transport_unavailable`, and `unsupported`. Discovery
starts in the background when the backend connects, so acquiring the WS-MAN
feature connection is not blocked by a slow Dell management endpoint. The UI polls only
while discovery is active. iDRAC reports `inventory_unavailable` when the boot
source class is empty and its authoritative CSIOR setting is disabled. A
disabled setting on a Lifecycle Controller which reports `Licensed=No` is a
terminal unavailable precondition rather than an actionable remediation.

CSIOR is exposed through the generic device-level backend requirements model,
not as a Dell-specific control inside the Boot popover. The `idrac-wsman`
backend owns inspection and the typed `enable_inventory_collection`
remediation. Licensed iDRAC6 uses singular `SetAttribute` and an empty-input
`CreateConfigJob`; iDRAC8 uses its plural/scheduled contract. The operation
sets the pending Lifecycle Controller attribute and creates a config job, but
never requests a host or BMC reboot. Its
`next_host_post` result tells the UI that concrete entries become available
only after the next normal POST and inventory collection. See
`docs/architecture/backend-requirements.md`.

Boot entry discovery and requirement state are feature-owned runtime data.
They survive the Boot protocol adapter's 30-second idle disconnect and are
reused immediately on the next request. Reconnect may start one background
refresh, but does not regress a terminal state to `discovering` or `unknown`.
