# Testing and validation

## Local CI

Run from the repository root in the reproducible Nix environment:

```bash
nix develop --command just ci
```

The same gate is available in groups:

- `just ci-rust`: formatting, dependency-free pure-model builds, workspace
  Clippy, unit/integration tests, architecture boundaries and source inventories.
- `just ci-frontend`: frozen dependency installation, ESLint, behavioral tests,
  Vue/TypeScript checking and the production build.
- `just ci-tools`: generated-contract freshness and Python tool tests.

Edit `contracts/feature-config.schema.json` before changing generated feature
DTOs, then run `just generate-contracts`. Do not edit generated DTOs by hand.

## Architecture and behavioral coverage

- [iDRAC state-machine coverage](idrac-state-machine-coverage.md) records native
  backend source ownership, model bridges, failure policies and protocol limits.
- [Other backend state-machine coverage](backend-state-machine-coverage.md)
  records Mahogany, VNC, Redfish, IPMI and WOL ownership and model constraints.
- [iDRAC compatibility matrix](idrac-compatibility.md) distinguishes automated
  evidence from acceptance on specific device generations and firmware.

AST purity, architecture and inventory checks constrain module boundaries;
behavioral tests verify cancellation, mutation ownership, configuration replay,
readback, malformed inputs, resource limits and device isolation. Passing source
checks does not prove exhaustive state transitions or branch coverage.

## Interpreting results

- Ignored hardware tests are unverified. Some GStreamer tests depend on installed
  plugins; an aggregate pass count does not mean every hardware encoder ran.
- Mock HTTP/protocol and ICE tests do not establish browser rendering, decoded
  video quality, audio playback or complete physical-device interoperability.
  Browser acceptance must include navigation, reconnect and multiple clients.
- Remote command admission and device confirmation are separate observations.
  Unknown mutation results must not imply success or automatic replay. Readback
  is a point-in-time observation; another client can subsequently change a BMC.
- Dummy application authentication and its login/setup/account flows are retained
  for compatibility. Audio remains an explicit unavailable placeholder.
- A configuration rename may become visible before a directory-sync failure.
  Memory/runtime follow the visible configuration while crash durability remains
  uncertain and is logged. A failure before rename retains the old configuration.
- Boot entry IDs are opaque and provider-owned; refresh them after configuration
  changes. Requirement remediation rejects ambiguous providers until the request
  contract supports explicit provider selection.
- CPU admission budgets separate realtime encoding from whole-file work, but do
  not guarantee a hard realtime latency bound. A cancelled blocking IPMI job can
  finish after its caller; its owned transport still closes on completion.

For device acceptance, follow the compatibility matrix and
[transport security policy](../security/bmc-transports.md). Record redacted
firmware/profile observations; never retain credentials, session material,
HID events or display payloads in published test reports.
