import { onBeforeUnmount, onMounted } from 'vue'
import { ConfigurationEventCursor } from './eventCursor'

export interface ConfigurationEvent {
  type: 'configuration.ready' | 'configuration.changed' | 'configuration.resync_required'
  epoch: string
  revision: number
  scope?: { kind: string; section?: string; id?: string; device_id?: string; feature_id?: string; resource_id?: string }
  mutation_id?: string
  changed_paths?: string[]
  affected_features?: string[]
}

function isConfigurationEvent(value: unknown): value is ConfigurationEvent {
  if (!value || typeof value !== 'object') return false
  const event = value as Record<string, unknown>
  return ['configuration.ready', 'configuration.changed', 'configuration.resync_required'].includes(event.type as string)
    && typeof event.epoch === 'string' && event.epoch.length > 0
    && Number.isSafeInteger(event.revision) && (event.revision as number) >= 0
    && (event.scope === undefined || (event.scope !== null && typeof event.scope === 'object'
      && typeof (event.scope as Record<string, unknown>).kind === 'string'))
    && [event.changed_paths, event.affected_features].every(value => value === undefined
      || (Array.isArray(value) && value.every(item => typeof item === 'string')))
}

export function useConfigurationEvents(onEvent: (event: ConfigurationEvent) => void | Promise<void>) {
  let socket: WebSocket | null = null
  let closed = false
  let reconnectTimer: number | undefined
  const cursor = new ConfigurationEventCursor()

  const scheduleReconnect = () => {
    if (!closed && reconnectTimer === undefined) {
      reconnectTimer = window.setTimeout(() => {
        reconnectTimer = undefined
        connect()
      }, 1000)
    }
  }
  const connect = () => {
    if (closed || socket) return
    let current: WebSocket
    try {
      current = new WebSocket(`${location.origin.replace(/^http/, 'ws')}/api/configuration/ws`)
    } catch {
      scheduleReconnect()
      return
    }
    socket = current
    current.onmessage = message => {
      if (closed || socket !== current) return
      try {
        const event: unknown = JSON.parse(message.data)
        if (isConfigurationEvent(event) && cursor.accept(event)) {
          void Promise.resolve(onEvent(event)).catch(() => {
            // A consumer's failed HTTP refresh must not become an unhandled rejection.
          })
        }
      } catch {
        // Ignore malformed unsolicited messages; HTTP remains authoritative.
      }
    }
    current.onclose = () => {
      if (closed || socket !== current) return
      socket = null
      scheduleReconnect()
    }
  }

  const close = () => {
    closed = true
    if (reconnectTimer !== undefined) window.clearTimeout(reconnectTimer)
    reconnectTimer = undefined
    const current = socket
    socket = null
    current?.close()
  }

  onMounted(connect)
  onBeforeUnmount(close)
  return { close }
}
