
import { ref } from 'vue'
import { buildWsUrl, WS_RECONNECT_DELAY } from '@/types/websocket'
import { FeatureLeaseRegistry } from '@/feature-data/core'

export interface WsEvent {
  event: string
  data: unknown
}

type EventHandler = (data: never) => void

let wsInstance: WebSocket | null = null
let wsUrl: string | null = null
const handlers = new Map<string, EventHandler[]>()
let subscribedTopics: string[] = []
let subscribedFeatures: string[] = []
const featureLeases = new FeatureLeaseRegistry()
let reconnectTimeout: number | null = null
let shouldReconnect = false
const connected = ref(false)
const reconnectAttempts = ref(0)
const networkError = ref(false)
const networkErrorMessage = ref<string | null>(null)

function getSubscribedTopics(): string[] {
  return Array.from(handlers.entries())
    .filter(([, eventHandlers]) => eventHandlers.length > 0)
    .map(([event]) => event)
    .sort()
}

function arraysEqual(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((value, index) => value === b[index])
}

function getSubscribedFeatures(): string[] {
  return featureLeases.values()
}

function syncSubscriptions(force = false) {
  const topics = getSubscribedTopics()
  const features = getSubscribedFeatures()

  if (!force && arraysEqual(topics, subscribedTopics) && arraysEqual(features, subscribedFeatures)) {
    return
  }

  if (wsInstance && wsInstance.readyState === WebSocket.OPEN) {
    subscribe(topics, features)
    subscribedTopics = topics
    subscribedFeatures = features
  }
}

function connect() {
  shouldReconnect = true
  if (reconnectTimeout !== null) {
    window.clearTimeout(reconnectTimeout)
    reconnectTimeout = null
  }

  const url = buildWsUrl('/api/ws')
  if (wsInstance && wsUrl === url) {
    if (wsInstance.readyState === WebSocket.OPEN) syncSubscriptions()
    if (wsInstance.readyState === WebSocket.OPEN || wsInstance.readyState === WebSocket.CONNECTING) {
      return
    }
  }

  if (wsInstance) {
    const staleSocket = wsInstance
    wsInstance = null
    wsUrl = null
    staleSocket.close()
  }

  try {
    const socket = new WebSocket(url)
    wsInstance = socket
    wsUrl = url

    socket.onopen = () => {
      if (wsInstance !== socket) return
      const reconnected = reconnectAttempts.value > 0
      connected.value = true
      networkError.value = false
      networkErrorMessage.value = null
      reconnectAttempts.value = 0

      syncSubscriptions(true)
      handleEvent({ event: 'websocket.connected', data: { reconnected } })
    }

    socket.onmessage = (e) => {
      if (wsInstance !== socket) return
      try {
        const event: WsEvent = JSON.parse(e.data)

        if (event.event === 'error') {
          const errorData = event.data as { message?: string } | null
          console.error('[WebSocket] Server error:', errorData?.message)
        } else {
          handleEvent(event)
        }
      } catch (err) {
        console.error('[WebSocket] Failed to parse message:', err)
      }
    }

    socket.onclose = () => {
      if (wsInstance !== socket) return
      wsInstance = null
      wsUrl = null
      connected.value = false
      networkError.value = true
      handleEvent({ event: 'websocket.disconnected', data: {} })

      if (!shouldReconnect) return

      reconnectAttempts.value++
      reconnectTimeout = window.setTimeout(connect, WS_RECONNECT_DELAY)
    }

    socket.onerror = () => {
      if (wsInstance !== socket) return
      networkError.value = true
      networkErrorMessage.value = 'Network connection failed'
    }
  } catch (err) {
    console.error('[WebSocket] Failed to create connection:', err)
  }
}

function disconnect() {
  shouldReconnect = false
  if (reconnectTimeout !== null) {
    window.clearTimeout(reconnectTimeout)
    reconnectTimeout = null
  }

  if (wsInstance) {
    const socket = wsInstance
    wsInstance = null
    wsUrl = null
    socket.close()
  }
  subscribedTopics = []
  subscribedFeatures = []
  connected.value = false
}

function subscribe(topics: string[], features?: string[]) {
  if (wsInstance && wsInstance.readyState === WebSocket.OPEN) {
    const payload: Record<string, unknown> = { topics }
    if (features) {
      payload.features = features
    }
    wsInstance.send(JSON.stringify({ type: 'subscribe', payload }))
  }
}

function setFeatures(features: string[]) {
  featureLeases.setLegacy(features)
  syncSubscriptions()
}

function acquireFeature(feature: string): () => void {
  const release = featureLeases.acquire(feature)
  syncSubscriptions()
  return () => {
    release()
    syncSubscriptions()
  }
}

function on(event: string, handler: EventHandler) {
  if (!handlers.has(event)) {
    handlers.set(event, [])
  }
  handlers.get(event)!.push(handler)
  syncSubscriptions()
}

function off(event: string, handler: EventHandler) {
  const eventHandlers = handlers.get(event)
  if (eventHandlers) {
    const index = eventHandlers.indexOf(handler)
    if (index > -1) {
      eventHandlers.splice(index, 1)
    }
    if (eventHandlers.length === 0) {
      handlers.delete(event)
    }
  }
  syncSubscriptions()
}

function handleEvent(payload: WsEvent) {
  // Feature lifecycle events are emitted by the device event socket as a
  // direct `{type, ...}` payload; older producers use `{event, data}`.  Keep
  // one consumer path so fault updates cannot be silently dropped depending
  // on transport producer.
  const eventName = payload.event ?? (payload as { type?: string }).type
  const data = payload.data ?? payload
  const eventHandlers = handlers.get(eventName)

  if (eventHandlers) {
    eventHandlers.forEach(handler => {
      try {
        handler(data as never)
      } catch (err) {
        console.error(`[WebSocket] Error in handler for ${eventName}:`, err)
      }
    })
  }
}

export function useWebSocket() {
  // Connection is now triggered manually by components after registering handlers

  return {
    connected,
    reconnectAttempts,
    networkError,
    networkErrorMessage,
    on,
    off,
    subscribe,
    setFeatures,
    acquireFeature,
    connect,
    disconnect,
  }
}

if (typeof window !== 'undefined') {
  window.addEventListener('beforeunload', () => {
    disconnect()
  })
}
