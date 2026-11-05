# Backend Requirements and Remediation

Backend requirements describe device-side prerequisites that affect one or
more features: account permissions, configuration switches, licenses,
services, firmware inventory, optional hardware, and session policy. They are
not feature faults. A fault describes a failed or unhealthy operation now; a
requirement describes an authoritative BMC condition that can be ready,
actionable, pending, unavailable, or not yet known.

## Public model

The device-level API aggregates requirement records from configured feature
backends:

```text
GET  /bmc/:id/api/requirements
POST /bmc/:id/api/requirements/remediate
```

Each record identifies its owning feature and backend and contains typed
generic values:

- `id`, `category`, `state`, and `impact`
- localization keys for the title and current-state explanation
- an optional typed remediation action
- confirmation and restart requirements

Vendor resource URIs, selectors, attribute names, protocol integers, raw
commands, and credentials never cross this boundary. The frontend renders the
advertised model and does not match backend names.

## Ownership and dispatch

`FeatureBackendTemplate` may register `BackendRequirementHooks` beside its
normal typed feature connector. Inspection and remediation are routed back to
the feature/backend owner through these hooks. The backend still obtains its
typed runtime through `FeatureConnection::backend_runtime()`; there is no
generic feature operation dispatcher, public runtime handle, or arbitrary
command endpoint.

Long-running discovery results live in the owning feature runtime when they
must survive the protocol adapter's idle disconnect. For Boot this is
`BootFeatureRuntime`: terminal entry discovery and requirement state remain
visible while `backend_runtime` is cleared and rebuilt. A typed discovery
lease prevents overlapping WS-MAN probes; a later reconnect may refresh the
authoritative state without first replacing it with `unknown`.

Inspection is read-only. Before remediation, the handler reconnects the typed
provider and inspects it again. It executes only when the same requirement is
currently `action_required` and the backend currently advertises the exact
typed action. Stale UI state, pending work, satisfied conditions, and invented
actions return a conflict without invoking the backend.

## Restart semantics

Restart requirements describe when a change becomes effective; they do not
authorize the aggregator to perform that restart. In particular,
`next_host_post` means the operator may let the next normal host boot and POST
apply the setting. Remediation must not silently reset the host or BMC.

## iDRAC inventory collection

The first provider is the `idrac-wsman` backend requirement
`firmware_inventory_collection`. When Dell WS-MAN reports **Collect System
Inventory on Restart** disabled, it advertises
`enable_inventory_collection` only when the Lifecycle Controller reports that
it is licensed. An unlicensed controller is `unavailable`: it must not offer an
action which its firmware accepts but does not apply.

The source-confirmed operations are generation-specific:

1. iDRAC6 uses singular `DCIM_LCService.SetAttribute`, requiring
   `ReturnValue=0`, followed by `CreateConfigJob` with an empty input body,
   requiring `ReturnValue=4096`. Although the firmware schema declares
   `ScheduledStartTime` and `RebootIfRequired`, the 2.92 provider ignores those
   arguments and its SFCB/OpenWSMan path rejects them as invalid.
2. iDRAC8 uses `DCIM_LCService.SetAttributes`, requiring `ReturnValue=0`,
   followed by the scheduled `CreateConfigJob` contract.
3. Neither generation sends `RebootJobType` or authorizes an automatic host or
   BMC restart.

The result is `pending` with `next_host_post`. Concrete boot entries are not
fabricated while inventory is absent. This operation is based on the same
selectors, properties, and return contracts exercised by the vendored
official iDRAC6 2.92 provider binaries and the Dell WS-MAN fixtures retained
under `.codegraph`.

## Adding a provider

Add typed IDs only for conditions with stable product semantics. Implement
inspection and remediation in the owning feature trait/backend, register its
hooks in `define_backends!`, add localization, and cover state transitions plus
the public serialization boundary. A read-only requirement may omit
remediation entirely.
