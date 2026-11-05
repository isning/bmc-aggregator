import { ref, type Ref } from 'vue'
import { buildWsUrl, WS_RECONNECT_DELAY } from '@/types/websocket'

export interface FeatureEventCodec<TActions extends object> {
  responseOk: number
  responseUnavailable: number
  responseInvalidMessage: number
  encode(event: TActions): ArrayBuffer
}

export interface FeatureEventBus<TActions extends object> {
  connected: Ref<boolean>
  reconnectAttempts: Ref<number>
  networkError: Ref<boolean>
  networkErrorMessage: Ref<string | null>
  unavailable: Ref<boolean>
  lastSendError: Ref<string | null>
  droppedEvents: Ref<number>
  connect: () => Promise<boolean>
  disconnect: () => void
  send: (event: TActions) => Promise<void>
}

const buses = new Map<string, FeatureEventBus<never>>()
const MAX_BUFFERED_BYTES = 16 * 1024

function currentDeviceScope(): string {
  if (typeof window === 'undefined') return 'root'
  const match = window.location.pathname.match(/^\/devices\/([^/?#]+)/)
  if (!match?.[1]) return 'root'
  try {
    return decodeURIComponent(match[1])
  } catch {
    return match[1]
  }
}

function decodeErrorPayload(data: ArrayBuffer): string | null {
  if (data.byteLength <= 1) return null
  try {
    const text = new TextDecoder().decode(data.slice(1)).trim()
    return text.length > 0 ? text : null
  } catch {
    return null
  }
}

export function useFeatureEventBus<TActions extends object>(
  featureId: string,
  path: string,
  codec: FeatureEventCodec<TActions>,
): FeatureEventBus<TActions> {
  // A feature bus owns a device-scoped socket. Include the active device in
  // the cache key so navigating between consoles cannot reuse the previous
  // BMC's transport and send input to the wrong target.
  const ownerDevice = currentDeviceScope()
  const busKey = `${ownerDevice}:${featureId}:${path}`
  const existing = buses.get(busKey)
  if (existing) return existing as unknown as FeatureEventBus<TActions>

  let transport: WebSocket | null = null
  let reconnectTimeout: number | null = null
  let connectionPromise: Promise<boolean> | null = null
  let connectionResolved = false
  let shouldReconnect = false
  let pendingResolve: ((ready: boolean) => void) | null = null
  let handshakeTimeout: number | null = null

  const connected = ref(false)
  const reconnectAttempts = ref(0)
  const networkError = ref(false)
  const networkErrorMessage = ref<string | null>(null)
  const unavailable = ref(false)
  const lastSendError = ref<string | null>(null)
  const droppedEvents = ref(0)

  function settleConnection(ready: boolean) {
    if (handshakeTimeout !== null) window.clearTimeout(handshakeTimeout)
    handshakeTimeout = null
    pendingResolve?.(ready)
    pendingResolve = null
    if (!ready) connectionPromise = null
  }

  function connect(): Promise<boolean> {
    if (currentDeviceScope() !== ownerDevice) {
      disconnect()
      return Promise.resolve(false)
    }
    shouldReconnect = true
    if (reconnectTimeout !== null) window.clearTimeout(reconnectTimeout)
    reconnectTimeout = null

    if (transport?.readyState === WebSocket.OPEN && connectionResolved) {
      return Promise.resolve(!unavailable.value)
    }
    if (connectionPromise && pendingResolve) return connectionPromise

    // Retire the previous socket before callbacks can reach this attempt.
    const previous = transport
    transport = null
    settleConnection(false)
    previous?.close()
    connected.value = false
    connectionResolved = false
    networkError.value = false
    networkErrorMessage.value = null
    unavailable.value = false
    const pending = new Promise<boolean>(resolve => { pendingResolve = resolve })
    connectionPromise = pending
    try {
      const socket = new WebSocket(buildWsUrl(path))
      transport = socket
      socket.binaryType = 'arraybuffer'
      handshakeTimeout = window.setTimeout(() => {
        if (transport !== socket) return
        networkError.value = true
        networkErrorMessage.value = `${featureId} event bus handshake timed out`
        settleConnection(false)
        socket.close()
      }, 30_000)

      socket.onopen = () => {
        if (transport !== socket) return
        networkError.value = false
        networkErrorMessage.value = null
        reconnectAttempts.value = 0
      }
      socket.onmessage = event => {
        if (transport !== socket || socket.readyState !== WebSocket.OPEN || currentDeviceScope() !== ownerDevice) return
        if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 1) return
        const code = new DataView(event.data).getUint8(0)
        if (code === codec.responseOk) {
          unavailable.value = false
          networkError.value = false
          networkErrorMessage.value = null
          connected.value = true
          connectionResolved = true
          settleConnection(true)
        } else if (code === codec.responseUnavailable) {
          unavailable.value = true
          networkError.value = false
          networkErrorMessage.value = decodeErrorPayload(event.data) ?? `${featureId} event bus unavailable`
          lastSendError.value = networkErrorMessage.value
          connected.value = false
          connectionResolved = true
          settleConnection(false)
        } else if (code === codec.responseInvalidMessage) {
          lastSendError.value = decodeErrorPayload(event.data) ?? `${featureId} event rejected by backend`
          console.warn(`[${featureId}] Server rejected event: ${lastSendError.value}`)
        }
      }
      socket.onclose = () => {
        if (transport !== socket) return
        transport = null
        connected.value = false
        connectionResolved = false
        settleConnection(false)
        if (!shouldReconnect || currentDeviceScope() !== ownerDevice) return
        if (unavailable.value) return
        networkError.value = true
        networkErrorMessage.value = `${featureId} event bus disconnected`
        reconnectAttempts.value++
        reconnectTimeout = window.setTimeout(() => {
          reconnectTimeout = null
          void connect()
        }, WS_RECONNECT_DELAY)
      }
      socket.onerror = () => {
        if (transport !== socket) return
        connected.value = false
        networkError.value = true
        networkErrorMessage.value = 'Network connection failed'
        connectionResolved = false
        settleConnection(false)
        socket.close()
      }
    } catch (error) {
      console.error(`[${featureId}] Failed to create event bus:`, error)
      networkError.value = true
      networkErrorMessage.value = 'Failed to create event bus connection'
      connectionResolved = false
      settleConnection(false)
    }
    return pending
  }

  function disconnect() {
    shouldReconnect = false
    if (reconnectTimeout !== null) window.clearTimeout(reconnectTimeout)
    reconnectTimeout = null
    const previous = transport
    transport = null
    settleConnection(false)
    previous?.close()
    connected.value = false
    networkError.value = false
    networkErrorMessage.value = null
    connectionPromise = null
    connectionResolved = false
  }

  function send(event: TActions): Promise<void> {
    return new Promise((resolve, reject) => {
      if (currentDeviceScope() !== ownerDevice) {
        disconnect()
        const error = new Error(`${featureId} event bus belongs to another device`)
        lastSendError.value = error.message
        droppedEvents.value++
        reject(error)
        return
      }
      if (!transport || transport.readyState !== WebSocket.OPEN) {
        const err = new Error(`${featureId} event bus not connected`)
        lastSendError.value = err.message
        droppedEvents.value++
        reject(err)
        return
      }

      if (!connectionResolved || unavailable.value) {
        const err = new Error(`${featureId} event bus not ready`)
        lastSendError.value = err.message
        droppedEvents.value++
        reject(err)
        return
      }

      if (transport.bufferedAmount > MAX_BUFFERED_BYTES) {
        const err = new Error(`${featureId} event bus backpressure`)
        lastSendError.value = err.message
        droppedEvents.value++
        reject(err)
        return
      }

      try {
        transport.send(codec.encode(event))
        lastSendError.value = null
        resolve()
      } catch (err) {
        lastSendError.value = err instanceof Error ? err.message : String(err)
        droppedEvents.value++
        reject(err)
      }
    })
  }

  const bus: FeatureEventBus<TActions> = {
    connected,
    reconnectAttempts,
    networkError,
    networkErrorMessage,
    unavailable,
    lastSendError,
    droppedEvents,
    connect,
    disconnect,
    send,
  }

  buses.set(busKey, bus)

  return bus
}

export function disconnectAllFeatureEventBuses() {
  buses.forEach(bus => bus.disconnect())
}

if (typeof window !== 'undefined') {
  window.addEventListener('beforeunload', () => {
    disconnectAllFeatureEventBuses()
  })
}
