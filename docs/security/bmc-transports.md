# BMC Transport Security

**Scope**: BMC-facing iDRAC AVCT, VNC, IPMI, Redfish, MSD image delivery, and their
configuration. This does not design or replace the application's deferred auth
module.

## Baseline

- Place BMC management interfaces on a dedicated, trusted management network.
- Use environment placeholders or a secret manager for BMC credentials; never
  commit secret literals into YAML, test fixtures, traces, or documentation.
- Device read APIs, status endpoints, error responses, and logs must redact
  keys containing password, token, secret, credential, or authorization.
- Backend transport credentials are separate from end-user application auth.

## iDRAC AVCT

- AVCT uses the device Web-account credentials directly over its dedicated
  control socket. They must never appear in traces, diagnostics, pool keys, or
  browser-facing configuration reads.
- The `idrac-legacy-rsa` TLS policy accepts the appliance's legacy/self-signed
  console TLS only inside the AVCT connector. Restrict this traffic to a trusted
  management network; do not copy that policy to VNC, Redfish, or HTTP clients.
- Do not substitute a VNC connection after AVCT failure. A different transport
  and security model requires explicit operator configuration.
- Bound APCP records, AVCT packets, display resolution, queues, and frame
  fanout; disconnect and reconnect rather than retaining malformed payloads.

## VNC/RFB

- Standard VNC password authentication is a legacy mechanism; it is not a
  substitute for network isolation or TLS.
- Disable unauthenticated VNC by default. A `none` security mode requires an
  explicit, reviewed opt-in and is inappropriate for a shared network.
- TLS is currently unsupported: `vnc_tls: true` is rejected. Plain RFB
  requires an appropriately isolated network. Authentication failure never
  triggers a weaker security type or a different endpoint.
- Keep VNC passwords separate from iDRAC Web/IPMI credentials and redact both.
- Bound all handshake, framebuffer, rectangle, decompression, and outbound
  input resources before allocation or queueing.

## Redfish

- Prefer HTTPS and strict certificate validation.
- Treat Basic credentials and Authorization headers as secrets. The current
  client uses Basic authentication and does not create Redfish sessions.
- Follow only discovered Redfish links that remain within the configured BMC
  authority. Do not let a service-provided URL redirect the client to an
  unrelated host without explicit validation.
- Use action targets and AllowableValues discovered from the service. Do not
  guess OEM endpoints or request types.

## MSD and BMC-side fetches

- An image URL passed to virtual media causes the BMC to fetch it. The aggregator
  requires an absolute HTTPS URL without embedded credentials. It does not
  control the BMC's DNS resolution, redirects or network access; enforce those
  policies on the BMC and management network separately.
- Do not expose a local filesystem path as a remote-media URL.
- Preserve current image-download protections: bounded size/concurrency,
  cancellation, no automatic redirects, and explicit private-network policy.

## Operational checklist

1. Record an endpoint's TLS/security mode without recording secrets.
2. Confirm firewall rules permit only expected aggregator-to-BMC flows.
3. Verify API/config read paths show `{ configured: boolean }` secret descriptors rather than secrets.
4. Test config reload and process shutdown for task/connection cleanup.
5. Rotate any credential accidentally put in a source-controlled file before
   treating the remediation as complete.
