import { DeviceScope } from '@/api/deviceScope'
import { getCurrentInstance, onActivated, onDeactivated, onScopeDispose, readonly, ref, shallowRef, watch, type Ref } from 'vue'
import { currentBmcIdFromLocation } from '@/api/request'
import { useWebSocket } from '@/composables/useWebSocket'
import { isNewerRevision, RevisionEpoch } from './core'

export type ResourcePhase = 'idle' | 'loading' | 'ready' | 'refreshing' | 'stale' | 'error'

export interface FeatureDataEvent<T = unknown> {
  feature: string
  resource: string
  epoch?: string
  revision: number
  data: T
}

export interface FeatureDataResourceOptions<T> {
  feature: string
  resource?: string
  load: () => Promise<T>
  revision: (value: T) => number
  epoch?: (value: T) => string | undefined
  pollInterval?: number | ((value: T | null) => number | false)
  eventValue?: (data: unknown, revision: number, current: T | null) => T
}

export function createFeatureDataResource<T>(options: FeatureDataResourceOptions<T>) {
  const data = shallowRef<T | null>(null)
  const phase = ref<ResourcePhase>('idle')
  const error = shallowRef<unknown>(null)
  const revision = ref(0)
  const subscribers = ref(0)
  let scope: string | null = null
  const revisionEpoch = new RevisionEpoch()
  const deviceScope = new DeviceScope(currentBmcIdFromLocation)
  const mutationScope = new DeviceScope(currentBmcIdFromLocation)
  let inFlight: Promise<T> | null = null
  let timer: number | null = null
  let listening = false
  let releaseFeature: (() => void) | null = null
  const ws = useWebSocket()

  function reset(nextScope = currentBmcIdFromLocation()) {
    deviceScope.reset()
    mutationScope.reset()
    scope = nextScope
    data.value = null
    phase.value = 'idle'
    error.value = null
    revision.value = 0
    revisionEpoch.reset()
    inFlight = null
    clearTimer()
  }

  function ensureScope(): boolean {
    const current = currentBmcIdFromLocation()
    if (scope === current) return false
    reset(current)
    if (current !== null) ws.connect()
    return true
  }

  function refreshCurrentScopeSoon() {
    if (subscribers.value === 0) return
    queueMicrotask(() => {
      void refresh().catch(() => undefined).finally(schedule)
    })
  }

  function clearTimer() {
    if (timer !== null) window.clearTimeout(timer)
    timer = null
  }

  function nextInterval(): number | false {
    const policy = options.pollInterval
    if (typeof policy === 'function') return policy(data.value)
    return policy ?? false
  }

  function adoptEpoch(incoming: string | undefined): boolean {
    const result = revisionEpoch.adopt(incoming)
    if (result === 'retired') return false
    if (result === 'changed') revision.value = 0
    return true
  }

  function schedule() {
    clearTimer()
    if (subscribers.value === 0) return
    const interval = nextInterval()
    if (interval === false) return
    timer = window.setTimeout(() => {
      timer = null
      void refresh().catch(() => undefined).finally(schedule)
    }, interval)
  }

  async function refresh(): Promise<T> {
    ensureScope()
    if (inFlight) return inFlight
    const requestScope = deviceScope.capture()
    phase.value = data.value === null ? 'loading' : 'refreshing'
    const request = options.load()
      .then(value => {
        if (scope !== currentBmcIdFromLocation()) {
          reset(currentBmcIdFromLocation())
          refreshCurrentScopeSoon()
          return value
        }
        if (!requestScope.current()) return value
        if (!adoptEpoch(options.epoch?.(value))) return value
        const nextRevision = options.revision(value)
        if (nextRevision >= revision.value) {
          data.value = value
          revision.value = nextRevision
        }
        phase.value = 'ready'
        error.value = null
        return value
      })
      .catch(reason => {
        if (requestScope.current()) {
          error.value = reason
          phase.value = data.value === null ? 'error' : 'stale'
        }
        throw reason
      })
      .finally(() => {
        if (inFlight === request) inFlight = null
      })
    inFlight = request
    return request
  }

  function applyEvent(event: FeatureDataEvent) {
    if (ensureScope()) {
      refreshCurrentScopeSoon()
      return
    }
    if (event.feature !== options.feature || event.resource !== (options.resource ?? 'status')) return
    if (!adoptEpoch(event.epoch)) return
    if (!isNewerRevision(revision.value, event.revision)) return
    data.value = options.eventValue
      ? options.eventValue(event.data, event.revision, data.value)
      : event.data as T
    revision.value = event.revision
    phase.value = 'ready'
    error.value = null
  }

  function invalidatePendingRead() {
    deviceScope.reset()
    inFlight = null
    clearTimer()
    phase.value = data.value === null ? 'idle' : 'stale'
  }

  function refreshAfterReconnect(event?: { epoch?: string }) {
    if (subscribers.value === 0) return
    if (event?.epoch && !adoptEpoch(event.epoch)) return
    invalidatePendingRead()
    void refresh().catch(() => undefined).finally(schedule)
  }

  function refreshAfterSocketConnected(event: { reconnected?: boolean }) {
    if (subscribers.value === 0) return
    if (event.reconnected) {
      revisionEpoch.reset()
      revision.value = 0
      phase.value = data.value === null ? 'idle' : 'stale'
    }
    refreshAfterReconnect()
  }

  function start() {
    ensureScope()
    subscribers.value += 1
    if (subscribers.value !== 1) return
    revisionEpoch.reset()
    revision.value = 0
    if (!listening) {
      ws.on('feature.data_changed', applyEvent)
      ws.on('feature.data_resync_required', refreshAfterReconnect)
      ws.on('websocket.connected', refreshAfterSocketConnected)
      listening = true
    }
    releaseFeature = ws.acquireFeature(options.feature)
    ws.connect()
    void refresh().catch(() => undefined).finally(schedule)
  }

  function stop() {
    subscribers.value = Math.max(0, subscribers.value - 1)
    if (subscribers.value > 0) return
    deviceScope.reset()
    mutationScope.reset()
    inFlight = null
    clearTimer()
    if (listening) {
      ws.off('feature.data_changed', applyEvent)
      ws.off('feature.data_resync_required', refreshAfterReconnect)
      ws.off('websocket.connected', refreshAfterSocketConnected)
      listening = false
    }
    releaseFeature?.()
    releaseFeature = null
  }

  async function mutate<R>(operation: (mutationId: string) => Promise<R>): Promise<R> {
    ensureScope()
    const owner = mutationScope.capture()
    const mutationId = crypto.randomUUID()
    try {
      return await operation(mutationId)
    } finally {
      // A lost response can follow a committed remote mutation. Read back without replaying it.
      if (owner.current()) {
        invalidatePendingRead()
        await refresh().catch(() => undefined).finally(schedule)
      }
    }
  }

  return {
    data: readonly(data) as Readonly<Ref<T | null>>,
    phase: readonly(phase),
    error: readonly(error),
    revision: readonly(revision),
    subscribers: readonly(subscribers),
    refresh,
    mutate,
    start,
    stop,
    reset,
  }
}

export function useFeatureDataSubscription(
  resource: Pick<ReturnType<typeof createFeatureDataResource<unknown>>, 'start' | 'stop'>,
  active: Ref<boolean>,
) {
  let acquired = false
  let activated = true
  let disposed = false
  const release = () => {
    if (!acquired) return
    acquired = false
    resource.stop()
  }
  const sync = () => {
    const enabled = !disposed && activated && active.value
    if (enabled && !acquired) {
      acquired = true
      resource.start()
    } else if (!enabled) release()
  }
  const stopWatch = watch(active, sync, { immediate: true })
  if (getCurrentInstance()) {
    onActivated(() => {
      activated = true
      sync()
    })
    onDeactivated(() => {
      activated = false
      release()
    })
  }
  onScopeDispose(() => {
    disposed = true
    stopWatch()
    release()
  })
}
