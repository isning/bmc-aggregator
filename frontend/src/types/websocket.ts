function currentBmcIdFromPathname(pathname: string): string | null {
  const match = pathname.match(/^\/devices\/([^/?#]+)/)
  const rawId = match?.[1]
  if (!rawId) return null

  try {
    return decodeURIComponent(rawId)
  } catch {
    return rawId
  }
}

/** Build WebSocket URL from current location */
export function buildWsUrl(path: string): string {
  const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  const normalized = path.startsWith('/') ? path : `/${path}`

  if (normalized === '/api' || normalized.startsWith('/api/')) {
    const bmcId = currentBmcIdFromPathname(window.location.pathname)
    if (!bmcId) {
      throw new Error('No BMC device is selected in the current route')
    }
    const devicePath = `/bmc/${encodeURIComponent(bmcId)}/api${normalized.slice('/api'.length)}`
    return `${wsProtocol}//${window.location.host}${devicePath}`
  }

  if (normalized.startsWith('/bmc/')) {
    return `${wsProtocol}//${window.location.host}${normalized}`
  }

  return `${wsProtocol}//${window.location.host}${normalized}`
}

/** Default reconnect delay in milliseconds */
export const WS_RECONNECT_DELAY = 3000

export type StreamDeviceLostKind = 'video' | 'audio'

export interface StreamDeviceLostEventData {
  kind: StreamDeviceLostKind
  device: string
  reason: string
}
