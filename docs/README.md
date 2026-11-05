# Documentation Index

## Current architecture

- [Architecture](../ARCHITECTURE.md): three-layer architecture and route/runtime
  ownership.
- [Context](../CONTEXT.md): current-code terminology and orientation.
- [Registry ADR](adr/0008-registry-pattern-for-backend-selection.md): backend
  selection registry.
- [Feature lifecycle ADR](adr/0009-per-feature-connection-state.md): per-feature
  `FeatureConnection` lifecycle.
- [Three-layer ADR](adr/0010-three-layer-architecture.md): architectural decision
  and module map.
- [Unified status ADR](adr/0011-unified-feature-status-interface.md): feature
  status contract.
- [Feature data lifecycle ADR](adr/0013-unified-feature-data-lifecycle.md):
  revisioned shared state, mutation consistency, and browser subscriptions.
- [Settings configuration ADR](adr/0014-settings-configuration-architecture.md):
  scope-owned settings, backend-described configuration, and observable
  configuration application.
- [Pure iDRAC state machine ADR](adr/0015-pure-idrac-state-machines.md): enforced
  decision-model isolation, expanded control coverage, transition contracts, and tests.
- [Other backend state machine ADR](adr/0016-pure-backend-state-machines.md):
  protocol-neutral decision models and enforcement.

## Protocol backends

- [Generic and native iDRAC backend ADR](adr/0012-generic-protocol-backends.md)
- [Native iDRAC protocol evidence](devices/idrac-protocol-reverse-engineering.md)
- [Generic VNC backend](backends/vnc.md)
- [Generic Redfish backend](backends/redfish.md)

## Operations and validation

- [iDRAC state-machine coverage](testing/idrac-state-machine-coverage.md): complete
  source inventory, failure semantics, regression evidence, and protocol limits.
- [Other backend state-machine coverage](testing/backend-state-machine-coverage.md):
  source ownership and model constraints for the remaining protocols.
- [Testing and validation](testing/README.md): CI entrypoints, behavioral coverage
  and hardware/browser verification limits.
- [Configuration reference](configuration.md)
- [iDRAC device profile](devices/idrac.md)
- [BMC transport security](security/bmc-transports.md)
- [iDRAC compatibility and acceptance matrix](testing/idrac-compatibility.md)

## Feature contracts

- [Backend requirements](architecture/backend-requirements.md)
- [Boot and identify](architecture/boot-identify-features.md)
- [MSD media model](architecture/msd-media-model.md)
- [Feature faults](architecture/feature-fault-model.md)
- [Video faults](architecture/video-fault-model.md)
- [Frontend video lifecycle](architecture/frontend-video-lifecycle.md)
