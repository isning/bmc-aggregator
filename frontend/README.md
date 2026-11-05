# BMC Aggregator Frontend

Vue 3 client for the Rust/Axum BMC Aggregator. Device-scoped APIs resolve from
the active route: `/devices/:bmcId/{console,settings}` maps to
`/bmc/:bmcId/api/...`.

## Development

From this directory:

```bash
pnpm install --frozen-lockfile
pnpm run dev
pnpm run build
```

Vite proxies `/api` and `/bmc` to `http://localhost:8080` during development.
The production bundle is written to `frontend/dist`; configure the Rust server
with `server.frontend_dir: ./frontend/dist`.

## Architecture boundaries

- Device settings use `src/stores/deviceConfiguration.ts` and the server catalog.
  Console configuration uses feature-owned `*Config.ts` stores; global settings
  own their service request state. There is no global config multiplexer.
- Device API calls and WebSockets are scoped via the active router path.
  Service configuration, authentication and inventory use root `/api/*` URLs.
- Feature UI belongs under `src/features/<feature>/`; shared layout and UI
  primitives live under `src/components/`.

The repository root `AGENTS.md` and `ARCHITECTURE.md` are the authoritative
backend/frontend integration references.

For current device backend configuration, use
[`docs/configuration.md`](../docs/configuration.md). The iDRAC/VNC/Redfish
examples there describe the explicit backend assignments currently exposed by
the server catalog. The frontend must continue to render only catalog-advertised
features and fields, rather than maintaining a protocol-specific preset table.
