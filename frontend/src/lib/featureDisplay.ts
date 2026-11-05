const BACKEND_LABELS: Record<string, string> = {
  'idrac-avct-console': 'iDRAC AVCT console',
  'idrac-boot': 'iDRAC boot control',
  'idrac-legacy-media': 'iDRAC virtual media',
  mahogany: 'Mahogany capture',
  vnc: 'VNC console',
  ch9329: 'CH9329 serial HID',
  usb: 'USB HID',
  ipmi: 'IPMI',
  redfish: 'Redfish',
  wol: 'Wake-on-LAN',
  software: 'Software',
  nvenc: 'NVIDIA NVENC',
  none: 'Disabled',
}

/** Convert an internal backend id into a stable user-facing transport name. */
export function formatFeatureBackend(backend?: string | null): string {
  const value = backend?.trim()
  if (!value) return ''
  return BACKEND_LABELS[value.toLowerCase()] ?? value
}
