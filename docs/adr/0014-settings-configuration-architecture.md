# ADR-0014: Scope-Owned Settings and Backend-Described Configuration

**Status**: Accepted / Implemented
**Date**: 2026-08-07
**Implementation reference**: [Configuration](../configuration.md)

## Context

The current frontend settings surface is one device-scoped route,
`/devices/:bmcId/settings`, but it edits four different kinds of state:

- browser-local appearance preferences;
- aggregator-wide service, streaming, storage, and diagnostic settings;
- managed-device identity and credentials;
- per-device feature/backend configuration and runtime operations.

`SettingsView.vue` consequently owns nine navigation sections, a complete
managed-device editor, backend presets, local hardware discovery, feature
configuration, and operational actions. It hard-codes a subset of the feature
and backend registry and directly renders `backend_params.*` keys. The public
admin DTO omits feature providers even though `DeviceFeatureConfig` supports
them, so the settings UI cannot represent Boot's provider set or future
feature composition without feature-specific branches.

Configuration persistence and runtime application are also not observable as
separate states. `AppState::commit_config()` durably saves the complete config,
publishes it in memory, and replaces registry entries, but it has no
configuration revision, optimistic concurrency, mutation correlation, or
configuration-change event. A browser can therefore report only a transient
"saved" flag and cannot distinguish durable persistence, registry application,
an unverified lazy backend, and a failed backend connection.

The settings architecture must preserve the existing architecture boundaries:

- `FeatureConnection` remains a lifecycle handle, not a config store;
- `FeatureBackendTemplate` remains a runtime registration seam and does not
  carry configuration presentation metadata;
- feature behavior and feature parameters remain feature-owned;
- protocol backend crates remain independent from the application crate;
- frontend feature config state remains feature-owned instead of returning to
  one central config multiplexer;
- legacy inference remains confined to `DeviceConfig::normalize_feature_config()`.

## Decision

### 1. Separate settings by scope

The product exposes two settings surfaces.

Global settings use `/settings` and contain only aggregator or browser scope:

| Section | Ownership |
|---|---|
| Appearance | Browser-local theme and language preferences |
| Aggregator | Web listener, stream transport infrastructure, and storage locations |
| Diagnostics | Encoder inventory/self-check, configuration health, and environment checks |
| About | Version, build, operating system, and architecture information |

Device inventory remains `/devices`. Device creation uses `/devices/new` and
device settings use `/devices/:bmcId/settings` with these sections:

| Section | Ownership |
|---|---|
| Overview | Device identity, management endpoints, and write-only credentials |
| Features | Explicit feature backend/provider assignments and feature-owned settings |
| Requirements | Permission, license, service, inventory, hardware, and session prerequisites |
| Advanced | Reconnect policy and infrequently changed connection parameters |

Runtime operations do not belong in either surface. Power commands, Wake-on-LAN
send, virtual-media attachment, mouse mode, and video scaling remain console
operations. Aggregator-local hardware configuration such as CH9329 or ATX is
shown under the device feature which selected that backend, not as a global HID
or Power category.

### 2. Use a separate configuration catalog

A configuration catalog describes what can be configured. It is separate from
the runtime backend registry and uses protocol-neutral contracts defined in
`bmc-feature-api`.

Conceptually, the catalog contains:

```rust
pub struct ConfigurationCatalog {
    pub version: String,
    pub features: Vec<FeatureConfigDescriptor>,
    pub backends: Vec<BackendConfigDescriptor>,
}

pub struct FeatureConfigDescriptor {
    pub feature_id: String,
    pub title_key: String,
    pub binding: FeatureBindingKind,
    pub feature_fields: Vec<ConfigFieldDescriptor>,
}

pub enum FeatureBindingKind {
    Single,
    ProviderSet { min: usize, max: Option<usize> },
}

pub struct BackendConfigDescriptor {
    pub feature_id: String,
    pub backend_id: String,
    pub display_name: String,
    pub fields: Vec<ConfigFieldDescriptor>,
    pub defaults: BTreeMap<String, ConfigValue>,
}
```

The field vocabulary is deliberately small:

- text and secret;
- integer and decimal;
- boolean;
- enum;
- a registered discovered-resource selector.

Fields may be grouped as `primary` or `advanced`. The catalog is not an
arbitrary UI language: it has no layout coordinates, executable validation,
cross-field expression language, or backend-supplied component names. Complex
behavior stays in a feature-owned panel. The descriptor controls primitive
connection fields and backend selection only.

Static descriptors are exported by their owning backend crates. Feature-field
descriptors are owned by feature modules. The application assembles them in a
configuration catalog registry distinct from `registered_backends()`. Adding
configuration metadata must not add presentation fields to
`FeatureBackendTemplate` or move wire/config parsing into application adapters.

The catalog is presentation metadata, not an authorization or validation
boundary. Server-side typed builders and validators remain authoritative.

### 3. Make composition data-driven

The frontend never assumes that every feature has exactly one backend. The
catalog declares either a single binding or a provider set. The administrative
API exposes a tagged assignment, so invalid simultaneous `backend` and
`providers` values are not representable to clients:

```text
assignment:
  { kind: "single", backend: { id, params } }

assignment:
  { kind: "provider_set", providers: [{ id, backend, params }, ...] }
```

The server maps this boundary DTO to the existing typed device configuration.
This is an ownership adapter, not a runtime fallback. Feature-specific provider
selection and merge behavior remain in the feature layer. The frontend renders
the declared cardinality and never matches `boot`, `idrac`, or another backend
name to decide composition.

### 4. Keep creation profiles out of runtime selection

A creation profile may generate an explicit draft for common devices such as
iDRAC or Mahogany. Applying a profile shows the resulting feature assignments
before creation. The saved configuration contains explicit backends, providers,
and parameters.

`protocol` remains a legacy migration/creation hint while old configuration is
supported. It is not an editable runtime backend selector and must not cause a
runtime fallback. New frontend code does not implement protocol presets.

### 5. Preserve owner-specific configuration APIs

There is no endpoint which returns or mutates every setting in the application.
Canonical ownership remains:

- `/api/devices` and `/api/devices/:id` for device identity and feature
  assignments;
- `/api/server/*` for aggregator service settings;
- `/bmc/:id/api/feature/:feature/config` for feature-owned parameters;
- `/bmc/:id/api/requirements` for device-side prerequisites;
- `/api/configuration/catalog` for static configuration metadata;
- `/api/configuration/options/:resource` for allow-listed dynamic options;
- `/api/configuration/ws` for metadata-only configuration events. It currently
  enforces same-origin access; principal authentication is deferred until the
  existing compatibility auth stub is replaced.

Dynamic option resources use registered IDs such as `local.video_inputs` or
`local.serial_ports`; descriptors never provide arbitrary URLs. Their snapshots
are revisioned and their updates use the same resync-on-event-loss principles as
feature data, but they have their own configuration event namespace.

### 6. Separate concurrency versions from event ordering

Every configuration read returns an opaque content version as an HTTP `ETag`.
Mutations use `If-Match`; a stale version returns `409 Conflict` with the latest
version. The ETag is derived from the owned scope, so an unrelated device change
does not reject a device edit.

Configuration events separately carry process-local `{epoch, revision}` for
ordering and resync. These values are not used as optimistic-lock versions.
Every mutation also carries a caller-generated `mutation_id`, returned by the
response and included in the corresponding event. This handles an event arriving
before its initiating HTTP response without relying on timing.

The coordinator retains a bounded replay record by principal, scope, mutation
ID, and payload digest. The current compatibility path uses a fixed
compatibility principal; a real authenticated principal must replace it before
multi-user mutation isolation is claimed. Retrying the same committed mutation
returns its original result; reusing an ID with a different payload is rejected.
This check precedes the ordinary stale-ETag conflict so a lost HTTP response can
be retried safely.

Secrets use an explicit write-only mutation:

```text
{ operation: "keep" }
{ operation: "replace", value: "..." }
{ operation: "clear" }
```

Read responses expose only `{configured: boolean}`. Empty strings and redacted
nulls are not secret update semantics.

### 7. Treat persistence and backend health as separate state

A configuration mutation executes in this order:

1. acquire the configuration transaction;
2. compare `If-Match` and validate the complete candidate;
3. calculate an effective per-feature change set and prepare replacements;
4. durably save the candidate;
5. atomically publish config and registry changes;
6. emit the revisioned configuration event;
7. return the new ETag, revision, mutation ID, and affected features.

Only affected `FeatureConnection` values are replaced. An identical effective
feature config retains its connection, runtime, data epoch, and subscribers.
Renaming a device replaces all of that device's feature identities.

Backend connection remains lazy. Settings therefore do not claim that a saved
but unused feature is online. The UI derives a combined presentation from two
independent facts:

- configuration: dirty, saving, applied, or conflicted;
- feature lifecycle: inactive/unverified, connecting, active, or faulted.

An applied inactive backend is displayed as "Applied, not yet verified". Active
consumers reconnect through the normal feature lifecycle and eventually report
active or a structured feature fault.

### 8. Reuse revision machinery without centralizing stores

The frontend extracts the generic epoch/revision/request-generation machinery
from `createFeatureDataResource()` into a lower-level revisioned-resource
primitive. `createFeatureDataResource()` and configuration resources wrap that
primitive with their own event namespaces and scope resolution.

State ownership stays distributed:

- a device-administration store owns device identity and assignments;
- existing feature config stores own feature parameters;
- server config stores own their `/api/server/*` scopes;
- local appearance preferences remain local;
- no `config.ts` store multiplexes all settings.

### 9. Use progressive disclosure in the UI

The default device editor shows name, endpoint, credential state, a feature
summary, and current requirements. A feature row shows its selected backend or
provider count plus applied/runtime status. Selecting the row opens its details.
Primitive backend fields appear under "Connection details"; low-frequency
fields appear under "Advanced".

The desktop layout uses a stable left navigation and unframed, divided sections.
Mobile uses a compact section selector and a full-width detail view. Repeated
backend/provider rows may use cards, but page sections and feature panels do not
nest cards. Standard shadcn controls provide Select, Switch, Accordion, Sheet,
Alert, Badge, and form primitives.

## Rejected Alternatives

### Continue splitting `SettingsView.vue` into components

This improves file size but retains the incorrect scope, hard-coded feature
model, duplicated defaults, and manual lifecycle branches.

### Add UI metadata to `FeatureBackendTemplate`

This couples runtime connection registration to presentation and violates the
registry's intentionally narrow responsibility.

### Build a fully generic JSON form language

Conditional expressions, layout descriptions, and backend-provided components
would become another frontend framework and make behavior difficult to test.
The selected hybrid handles primitive connection fields generically and keeps
complex behavior feature-owned.

### Use one global configuration store and endpoint

This would erase route ownership, reintroduce cross-feature coupling, and make
unrelated concurrent edits conflict.

### Probe every backend immediately after save

This changes lazy feature lifecycle semantics, can consume exclusive BMC
sessions, and would make saving unrelated settings depend on network health.
Connection verification remains explicit or consumer-driven.

## Consequences

### Positive

- Route scope and displayed configuration ownership are unambiguous.
- New backends with primitive fields require no changes to the main settings
  page.
- Provider-set features are represented without frontend feature-name checks.
- Configuration races are detected instead of silently overwriting changes.
- Multiple clients observe saved changes through one revisioned event path.
- Changing one feature does not tear down unrelated feature connections.
- A successful disk write is no longer misrepresented as verified BMC health.

### Negative

- The application gains a second registry for configuration metadata.
- Backend crates must maintain descriptors alongside typed configuration
  parsing and validation.
- Device settings require a boundary DTO because the persistence model and UI
  composition model have different invariants.
- Configuration events need a root WebSocket in addition to the device feature
  WebSocket. Authentication of that root socket is deferred with the existing
  compatibility auth stub.

## Architectural Invariants

- Runtime backend selection remains explicit; no catalog entry authorizes a
  fallback.
- `FeatureBackendTemplate` remains runtime-only.
- Descriptors never carry credentials, arbitrary URLs, executable expressions,
  or raw commands.
- Server-side validation is authoritative.
- A configuration event is emitted only after the authoritative snapshot and
  registry are readable.
- Feature runtime data continues to follow ADR-0013.
- Operational commands never enter the settings mutation API.
