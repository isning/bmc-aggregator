# MSD Media And Storage Model

The MSD feature separates three concerns that vendor protocols often combine:

1. `MsdMediaKind` describes the device presented to the managed host:
   `optical`, `floppy`, `disk`, or `flash`.
2. `MsdStorageLocation` describes who owns the selected bytes:
   `aggregator`, `bmc`, or `remote`.
3. `MsdStorageOperation` describes what can be done at one location:
   `select`, `list`, `upload`, or `delete`.

Media types and operations are scoped to a storage location. This permits a
backend to advertise combinations such as remote optical media and BMC-stored
disk images without frontend checks for a backend or vendor name.

```json
{
  "virtual_drive": false,
  "slots": [
    {"id": "optical", "media_types": ["optical"]},
    {"id": "floppy", "media_types": ["floppy"]}
  ],
  "storage_locations": [
    {
      "location": "aggregator",
      "operations": ["select", "list", "upload", "delete"],
      "media_types": ["optical", "floppy"]
    },
    {
      "location": "remote",
      "operations": ["select"],
      "media_types": ["optical"]
    }
  ]
}
```

An empty `media_types` list means that the backend has one implicit target and
the request must omit `media_type`. If a location advertises one media type,
the feature layer fills it automatically. If it advertises more than one, the
caller must select one explicitly.

Slots describe independently mountable targets. A mount is valid only when
its media type is supported by both the selected slot and storage location.
Stable slot IDs are feature contract identifiers, not OEM drive numbers.

## Data Paths

| Location | Byte owner | Aggregator service required while mounted | Typical protocol |
|---|---|---:|---|
| `aggregator` | Aggregator managed storage | Yes, the backend reads or serves the selected bytes | Mahogany storage server, Dell AVMP |
| `bmc` | Persistent or temporary BMC storage | No, after a successful upload | Vendor upload/catalog API |
| `remote` | External URL origin | No | Redfish `InsertMedia` with a BMC-reachable URL |

`bmc` is part of the stable feature model, but no current production backend
advertises it without a successful storage probe. `MsdMediaStore` provides its
typed list/upload/delete boundary, and the shared handlers and UI dispatch
those operations by location. A backend must implement and verify that store
before publishing its operations. Capability data must never promise an
unimplemented endpoint.

`unavailable_storage_locations` carries capability-driven negative discovery.
It keeps an unavailable location out of selectors while allowing the shared UI
to explain a typed reason and render a backend-supplied localization key. The
iDRAC legacy-media adapter reports BMC storage as `media_not_present` when
`ServerImageSpace` returns zero and suggests checking the vFlash SD card. It
never requires the frontend to match an iDRAC backend name.

## Selection Contract

Requests use a tagged source reference. Filesystem paths and vendor drive
integers are not public fields.

```json
{
  "slot_id": "optical",
  "mode": "image",
  "source": {
    "location": "aggregator",
    "image_id": "installer.iso"
  },
  "media_type": "optical"
}
```

Other source forms are:

```json
{"location":"bmc","image_id":"opaque-controller-asset-id"}
{"location":"remote","url":"https://images.example/installer.iso"}
```

The status response returns every active mount in `attachments`. Each entry
contains `slot_id`, `mode`, the tagged `media` reference, and normalized
`media_type`. This prevents identically named assets in different storage
domains from being confused and permits optical and floppy media to remain
mounted independently.

```json
{
  "attachments": [
    {
      "slot_id": "optical",
      "mode": "image",
      "media": {"location": "aggregator", "image_id": "installer.iso"},
      "media_type": "optical"
    }
  ]
}
```

Managed image APIs require an explicit location:

```text
GET  /feature/msd/images?location=aggregator
POST /feature/msd/images/upload?location=aggregator
POST /feature/msd/images/delete
```

The delete body contains `location` and `image_id`. Downloading an external
file into aggregator storage also requires `location=aggregator`.

Image ingestion is bounded streaming. Multipart uploads and URL imports write
directly into the device image directory's `.staging` area while counting
bytes with `u64`; the per-device ceiling is configured at
`features.msd.feature_params.max_image_bytes` and defaults to 64 GiB. A successful
aggregator upload is published by an atomic same-filesystem rename, so the
normal path performs one file write and never buffers the complete image.
iDRAC ServerImage consumes the same staged file through a reusable 32 KiB
buffer. Cancellation, parse failure, backend failure, and limit failure remove
the staged file.

The feature model exposes MD5, SHA-1, SHA-256, and SHA-512 digests for manual
comparison with vendor-published values. One-shot upload and URL-import paths
update them incrementally while writing; resumable browser uploads perform one
bounded-buffer sequential checksum pass at finalization so hashing remains
correct across process restarts. SHA-1 and MD5 are compatibility fingerprints,
not secure integrity recommendations. Aggregator-owned image metadata persists the
digests beside the catalog without exposing sidecar files as images.

An image exceeding the limit returns HTTP 413 with a non-retryable storage
operation fault whose code is `upload_too_large` and operation is
`upload_image`. URL imports are asynchronous and expose the corresponding
`download_too_large` child fault through transfer status.

Large browser uploads use feature-owned offset sessions:

```text
POST   /feature/msd/images/uploads
GET    /feature/msd/images/uploads/:upload_id
PATCH  /feature/msd/images/uploads/:upload_id  (Upload-Offset header)
DELETE /feature/msd/images/uploads/:upload_id
```

The JSON sidecar is the committed-offset authority. A PATCH serializes per
session, truncates any uncommitted tail left by interruption, rejects stale
offsets with HTTP 409, and publishes only after the declared length is reached.
The same completion path dispatches to aggregator storage or any BMC-owned
store advertising the generic upload operation. The frontend sends 8 MiB
chunks and retains the session id using device, location, file name, size, and
last-modified identity.

URL imports retain a mode-0600 checkpoint containing offset and ETag or
Last-Modified validator. Transient failures retry with exponential backoff and
`Range`/`If-Range`; if the source returns a complete 200 response, staged data
and digest state restart safely from zero. Checkpoints survive runtime loss,
are exposed as `paused`, and can be resumed by id without exposing the source
URL through public status.

## Backend Boundary

The feature layer validates location, operation, and media-type combinations,
resolves aggregator image IDs to trusted managed paths, and stores portable
per-slot attachments. Backends implement `mount(slot_id, media)` and
`unmount(slot_id)` and map those operations to their protocol:

- Mahogany accepts aggregator-backed `optical` media. Its retained image file
  is served as a read-only 2048-byte-block MMC device over the BMC-initiated
  storage peer connection.
- Dell legacy AVMP accepts aggregator-backed media. Official drive type `2`
  maps to `optical`. iDRAC6 uses type `1` for floppy; the iDRAC8 native
  compatibility predicate accepts removable type `1` or floppy type `4` for
  a floppy image. The verified iDRAC6 type-zero capability fallback maps to
  `optical`. Raw values remain private.
- Redfish accepts an absolute HTTPS `remote` URL and maps the selected standard
  VirtualMedia resource's `MediaTypes` (`CD`, `DVD`, `Floppy`, `USBStick`) to
  generic media kinds. Unknown standard-extension values remain implicit.

iDRAC6 and iDRAC8 advertise floppy only when a usable generation-specific
floppy slot is present. Both map readonly floppy images through the official
`VDiskRequest2 (0x0211)` geometry lifecycle.

The Dell AVMP worker keeps mapped images keyed by the BMC's remote drive ID.
Read requests are routed by that ID, release and asynchronous failure remove
only the matching mapping, shutdown releases every active drive, and native
reconnect retains and remaps every local mount intent.

Backends whose remote capability table can settle asynchronously implement
`MsdBackend::refresh_capabilities(requested_media_type)`. Every feature entry
point that selects media invokes this hook before generic location/type
validation. Stable backends use the default no-op; the iDRAC6 adapter uses the
official bounded VDiskInfo refresh and waits for the requested generic media
kind rather than exposing AVMP drive integers to the feature layer. Refresh is
bounded by the same ten-second discovery deadline as initial setup; transport
loss and timeout complete the feature operation with a fault instead of
leaving the client pending.

Passive `/feature/msd/status` polling never starts a backend session. The MSD
panel explicitly requests `/feature/msd/status?discover=true`; that request
temporarily acquires the feature, refreshes the drive table, and returns live
capabilities. Its guard is released with the response, so normal idle teardown
still applies. This keeps dynamic slots usable after idle disconnect without
pinning a legacy AVMP connection from dashboard polling.

iDRAC8 may advertise two disabled type-zero placeholders for its optical and
floppy devices. They are not direct map targets, but the backend advertises
the two generic media kinds as activatable capabilities. An explicit media
selection invokes the native `SetDeviceCount (0x0240)` API with one optical,
one floppy, and one flash role when the negotiated AVMP 1.2 extended layout is
available. Provisioning all source-confirmed roles on first activation makes
later optical/removable selection order-independent; it does not imply three
physical targets. The normal completion is `0x8241` followed
by refreshed discovery. Firmware that omits `0x8241` is accepted only when a
new `VDiskInfo` independently proves that the requested real slot exists;
`0x8410` alone never completes the operation. Login, status, reconnect,
disconnect, and close never mutate the device layout.

Tested iDRAC8 firmware still converges to one type-2 CD
target and one type-1 removable target; the official client maps that type-1
target as either floppy or flash with `VDiskRequest2`. The backend therefore
allows optical plus either removable mode and rejects a second removable map
when no second remote drive exists. iDRAC6 exhibits the same type-1
floppy/flash sharing. The generic API models this honestly as one `removable`
slot whose supported media types are `floppy` and `flash`; firmware exposing a
separate type-4 floppy target receives an additional independent `floppy`
slot. This generic flash-media capability is not Dell VFlash SD partition
management and is not the AVMP ServerImage persistent store.

### Dynamic targets

The public feature contract does not impose a singleton per media kind.
`MsdCapabilities.target_management` advertises the media kinds a backend can
instantiate, whether created targets are deletable, and typed per-media target
limits. The legacy `max_targets` remains a compatibility summary.
`create_target(media_type, label)` returns a backend-assigned stable
target id; mount state and lifecycle guards remain keyed by that id. Therefore
two optical targets, several removable targets, and arbitrary mixed
combinations are valid at the feature layer. A backend with fixed hardware
returns an empty creation list and continues to publish its discovered targets
through `slots`.

```text
POST /feature/msd/targets/create  { media_type, label? }
POST /feature/msd/targets/delete  { target_id }
```

Deleting a mounted target is rejected before backend dispatch. Backend create
and delete failures use the common feature-fault response and never expose OEM
protocol text. The UI renders target count dynamically and only shows lifecycle
controls when the backend advertises them.

Target lifecycle operations and mount/unmount are serialized per feature
connection. Capacity checks are repeated atomically inside the feature-owned
registry, so concurrent create requests cannot overbook the advertised target
limit and a target cannot be deleted while a mount is being committed.

Dynamic targets are logical user-owned definitions, distinct from physical BMC
slots. Their registry is feature-owned and survives backend transport
reconnects. They are enabled only when a backend has more than one independently
addressable physical target to schedule. A single-target backend keeps its
fixed target and does not expose create/delete controls.

- Standard Redfish VirtualMedia is one service-defined resource in this
  backend, so it remains the fixed `default` target. The Redfish schema has no
  standard target-creation action.
- Mahogany has one storage-server channel, so it remains the fixed `optical`
  target and does not advertise dynamic target creation.
- Legacy iDRAC exposes optical/removable remote drive IDs. It permits logical
  targets for each discovered generic media kind; AVMP assigns them to distinct
  compatible drive IDs and rejects mounts once the firmware-authoritative
  VDiskInfo table has no free slot.

Creation never claims that a single physical channel has become multiple BMC
devices. Fixed discovered targets remain non-deletable; only targets marked
`dynamic` are deletable.

### Removable image layout

`bmc-backend-support::removable_image` owns reusable removable-image layout detection. Backends do
not parse disk layout themselves. It currently supports FAT superfloppies with
standard `EB` or `E9` BPB geometry, MBR and GPT partitioned images,
conventional floppy capacities, and arbitrary unpartitioned filesystem or
blank raw images through translated `16/63` geometry. MBR partitions and GPT
usable ranges are bounds-checked against the image before a backend can expose
them; partitioned images retain their complete raw capacity. iDRAC maps this generic result into
VDiskRequest2; other backends can consume the same model without importing
Dell protocol details.

Backends opt into source-specific validation through the typed `validate_media`
hook. Dell AVMP uses it for aggregator-backed optical, floppy, and flash
selections before protocol dispatch. Invalid or non-block-aligned images
therefore return a non-retryable request fault instead of being reported as a
transport failure, without imposing Dell geometry on other backends.

The supplied official native library exposes no VFlash SD partition-management
entry points. Its separate ServerImage create serializer leaves part of the
wire record uninitialized; the backend uses a bounded zero-filled replacement
and the source-confirmed create, 32 KiB data, finalize, inventory-refresh, and
delete lifecycle. BMC storage is advertised with list/upload/delete only after
`ServerImageSpace` reports nonzero total and free capacity. Upload requires the
official empty-inventory precondition, validates every response status, and
delete succeeds only after inventory no longer contains the index. Firmware
and MAS entries are listable but non-deletable; only names created with the
aggregator ownership prefix may be deleted. ServerImage selection is not
advertised because its map action is not yet source-confirmed.
