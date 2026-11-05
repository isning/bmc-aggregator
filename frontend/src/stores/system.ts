import { DeviceScope } from '@/api/deviceScope'
import { currentBmcIdFromLocation } from '@/api/request'
import { defineStore } from 'pinia'
import { computed, ref, watch } from 'vue'
import {
  systemApi,
  streamApi,
  hidApi,
  msdApi,
  type DeviceInfo,
  type MsdCapabilities,
  type MsdAttachment,
  type MsdStatus,
  type PlatformCapabilities,
} from '@/api'
import type { DeviceFeatureInfo, FeatureStatusInfo } from '@/api/config'
import type { FeatureFault } from '@/api/request'
import { msdStatusResource, powerStatusResource } from '@/feature-data/resources'

interface SystemCapabilities {
  video: { available: boolean; backend?: string; reason?: string }
  hid: { available: boolean; backend?: string; reason?: string }
  msd: { available: boolean; backend?: string; reason?: string }
  atx: { available: boolean; backend?: string; reason?: string }
  audio: { available: boolean; backend?: string; reason?: string }
}

interface DiskSpaceInfo {
  total: number
  available: number
  used: number
}

interface StreamState {
  online: boolean
  active: boolean
  device: string | null
  format: string | null
  resolution: [number, number] | null
  targetFps: number
  clients: number
  streamMode: string  // 'mjpeg' or 'webrtc'
  error: string | null
  fault: FeatureFault | null
}

interface HidState {
  available: boolean
  backend: string
  initialized: boolean
  online: boolean
  supportsAbsoluteMouse: boolean
  keyboardLedsEnabled: boolean
  ledState: {
    numLock: boolean
    capsLock: boolean
    scrollLock: boolean
  }
  device: string | null
  error: string | null
  errorCode: string | null
  fault: FeatureFault | null
}

interface AtxState {
  available: boolean
  backend: string
  initialized: boolean
  powerOn: boolean
  error: string | null
  fault: FeatureFault | null
}

interface MsdState {
  available: boolean
  attachments: MsdAttachment[]
  occupancy: NonNullable<MsdStatus['occupancy']>
  error: string | null
  fault: FeatureFault | null
  capabilities: MsdCapabilities
  backendStatus: Record<string, unknown> | null
}

interface AudioState {
  available: boolean
  streaming: boolean
  device: string | null
  quality: string
  error: string | null
  fault: FeatureFault | null
}

interface ConnectionState {
  wsConnected: boolean
  hidEventBusConnected: boolean
  wsNetworkError: boolean
  hidEventBusNetworkError: boolean
}

export interface FeatureConnectionStatus {
  connectionId: string
  state: string
}

export interface FeatureConnectionHealth {
  connectionId: string
  online: boolean
  fps: number
  resolution: [number, number]
}

interface FeatureConnectionSnapshot {
  connection_id?: string
  state?: string
  online?: boolean
  fps?: number
  width?: number
  height?: number
  resolution?: [number, number]
  fault?: FeatureFault | null
}

// DeviceInfo event payload types (from WebSocket)
export interface VideoDeviceInfo {
  available: boolean
  device: string | null
  format: string | null
  resolution: [number, number] | null
  fps: number
  online: boolean
  stream_mode: string  // 'mjpeg' or 'webrtc'
  config_changing?: boolean
  error: string | null
  fault?: FeatureFault | null
  connection?: { state: string }
  instances?: FeatureConnectionSnapshot[]
}

export interface HidDeviceInfo {
  available: boolean
  backend: string
  initialized: boolean
  online: boolean
  supports_absolute_mouse: boolean
  keyboard_leds_enabled: boolean
  led_state: {
    num_lock: boolean
    caps_lock: boolean
    scroll_lock: boolean
    compose: boolean
    kana: boolean
  }
  device: string | null
  error: string | null
  error_code?: string | null
  fault?: FeatureFault | null
  connection?: { state: string }
  instances?: FeatureConnectionSnapshot[]
}

export interface MsdDeviceInfo {
  available: boolean
  mode: string
  connected: boolean
  image_id: string | null
  error: string | null
  fault?: FeatureFault | null
  connection?: { state: string }
  instances?: FeatureConnectionSnapshot[]
}

export interface AtxDeviceInfo {
  available: boolean
  backend: string
  initialized: boolean
  power_on: boolean
  error: string | null
  fault?: FeatureFault | null
  connection?: { state: string }
  instances?: FeatureConnectionSnapshot[]
}

export interface AudioDeviceInfo {
  available: boolean
  streaming: boolean
  device: string | null
  quality: string
  error: string | null
  fault?: FeatureFault | null
}

export interface TtydDeviceInfo {
  available: boolean
  running: boolean
}

export interface DeviceInfoEvent {
  video: VideoDeviceInfo
  hid: HidDeviceInfo
  msd: MsdDeviceInfo | null
  atx: AtxDeviceInfo | null
  audio: AudioDeviceInfo | null
  ttyd: TtydDeviceInfo
}

export const useSystemStore = defineStore('system', () => {
  const version = ref<string>('')
  const buildDate = ref<string>('')
  const platform = ref<PlatformCapabilities | null>(null)
  const capabilities = ref<SystemCapabilities | null>(null)
  const diskSpace = ref<DiskSpaceInfo | null>(null)
  const deviceInfo = ref<DeviceInfo | null>(null)
  const stream = ref<StreamState | null>(null)
  const hid = ref<HidState | null>(null)
  const atx = ref<AtxState | null>(null)
  const msd = ref<MsdState | null>(null)
  const audio = ref<AudioState | null>(null)
  const ttyd = ref<TtydDeviceInfo | null>(null)
  const loading = ref(false)
  const error = ref<string | null>(null)
  const connectionState = ref<ConnectionState>({
    wsConnected: false,
    hidEventBusConnected: false,
    wsNetworkError: false,
    hidEventBusNetworkError: false,
  })

  const featureStates = ref<Record<string, string>>({})
  const featureHealth = ref<Record<string, { online: boolean; fps: number; resolution: [number, number] }>>({})
  const featureConnectionStates = ref<Record<string, Record<string, FeatureConnectionStatus>>>({})
  const featureConnectionHealth = ref<Record<string, Record<string, FeatureConnectionHealth>>>({})
  const featureFaults = ref<Record<string, Record<string, FeatureFault | null>>>({})
  const features = ref<DeviceFeatureInfo[]>([])
  const featureStatuses = ref<FeatureStatusInfo[]>([])
  const availableFeatureIds = computed(() => features.value.map(feature => feature.id))
  const availableFeatureIdSet = computed(() => {
    const ids = new Set(features.value.map(feature => feature.id))
    for (const feature of featureStatuses.value) {
      if (feature.available) ids.add(feature.id)
    }
    return ids
  })
  const isFeatureAvailable = (featureId: string) => availableFeatureIdSet.value.has(featureId)
  const getFeatureBackend = (featureId: string): string | null => {
    const status = featureStatuses.value.find(feature => feature.id === featureId)
    if (status?.backend) return status.backend
    const feature = features.value.find(item => item.id === featureId)
    return feature?.backend || null
  }
  let featureStatusPoller: number | null = null
  let featureStatusPollerRefs = 0
  let featureStatusRequest: Promise<Awaited<ReturnType<typeof systemApi.featureStatuses>>> | null = null
  const deviceScope = new DeviceScope(currentBmcIdFromLocation)

  function normalizeFeatureState(state: string): string {
    return state.trim().replace(/^[A-Z]/, c => c.toLowerCase())
  }

  function summarizeFeatureState(feature: string) {
    const states = Object.values(featureConnectionStates.value[feature] ?? {}).map(item => item.state)
    if (states.includes('connected')) return 'connected'
    if (states.includes('connecting')) return 'connecting'
    if (states.includes('disconnecting')) return 'disconnecting'
    if (states.includes('disconnected')) return 'disconnected'
    return featureStates.value[feature] ?? 'unknown'
  }

  function summarizeFeatureHealth(feature: string) {
    const healthValues = Object.values(featureConnectionHealth.value[feature] ?? {})
    const online = healthValues.find(item => item.online)
    const selected = online ?? healthValues[0]
    if (selected) {
      featureHealth.value = {
        ...featureHealth.value,
        [feature]: {
          online: selected.online,
          fps: selected.fps,
          resolution: selected.resolution,
        },
      }
    }
  }

  async function fetchSystemInfo() {
    const requestScope = deviceScope.capture()
    try {
      const info = await systemApi.info()
      if (!requestScope.current()) return info
      version.value = info.version
      return info
    } catch (e) {
      console.error('Failed to fetch system info:', e)
      throw e
    }
  }

  async function fetchFeatures() {
    const requestScope = deviceScope.capture()
    try {
      const result = await systemApi.features()
      if (!requestScope.current()) return result
      features.value = Array.isArray(result.features) ? result.features : []
      for (const feature of features.value) {
        updateFeatureState(feature.id, feature.state, feature.instance_id)
      }
      return result
    } catch (e) {
      console.error('Failed to fetch feature list:', e)
      throw e
    }
  }

  async function fetchFeatureStatuses() {
    if (featureStatusRequest) return featureStatusRequest

    const requestScope = deviceScope.capture()
    const request = (async () => {
      try {
        const result = await systemApi.featureStatuses()
        // Router reuse can switch the selected device while this request is
        // in flight. Do not let the old response repopulate the new device.
        if (!requestScope.current()) return result

        featureStatuses.value = Array.isArray(result.features) ? result.features : []
        features.value = featureStatuses.value.map(feature => ({
          id: feature.id,
          backend: feature.backend,
          instance_id: feature.connection_id,
          state: feature.state,
          healthy: feature.online,
          ref_count: feature.ref_count,
        }))
        for (const feature of featureStatuses.value) {
          updateFeatureInstances(feature.id, feature.instances)
          updateFeatureState(feature.id, feature.state, feature.connection_id)
          updateFeatureFault(feature.id, feature.fault ?? null, feature.connection_id)
        }
        return result
      } catch (e) {
        console.error('Failed to fetch feature statuses:', e)
        throw e
      }
    })().finally(() => {
      if (featureStatusRequest === request) featureStatusRequest = null
    })
    featureStatusRequest = request

    return request
  }

  function startFeatureStatusPolling() {
    featureStatusPollerRefs += 1
    void fetchFeatureStatuses()
    if (featureStatusPoller === null) {
      featureStatusPoller = window.setInterval(() => {
        void fetchFeatureStatuses()
      }, 2500)
    }
  }

  function stopFeatureStatusPolling() {
    featureStatusPollerRefs = Math.max(0, featureStatusPollerRefs - 1)
    if (featureStatusPollerRefs === 0 && featureStatusPoller !== null) {
      window.clearInterval(featureStatusPoller)
      featureStatusPoller = null
    }
  }

  /** Clear all server-derived state before the selected BMC changes. */
  function resetDeviceState() {
    deviceScope.reset()
    featureStatusRequest = null
    loading.value = false
    error.value = null
    platform.value = null
    capabilities.value = null
    diskSpace.value = null
    deviceInfo.value = null
    stream.value = null
    hid.value = null
    atx.value = null
    msd.value = null
    audio.value = null
    ttyd.value = null
    featureStates.value = {}
    featureHealth.value = {}
    featureConnectionStates.value = {}
    featureConnectionHealth.value = {}
    featureFaults.value = {}
    features.value = []
    featureStatuses.value = []
    connectionState.value = {
      wsConnected: false,
      hidEventBusConnected: false,
      wsNetworkError: false,
      hidEventBusNetworkError: false,
    }
  }

  function startStream() {
    // Stream lifecycle is connection-based (MJPEG/WebRTC).
    // No explicit start is needed — connecting to the stream URL
    // implicitly begins the flow when FC is Connected.
  }

  function stopStream() {
    // No explicit stop needed — disconnecting from the stream URL
    // stops the flow. Keeping session open benefits other features
    // that share the same FC.
  }

  async function fetchHidState() {
    const requestScope = deviceScope.capture()
    try {
      const state = await hidApi.status()
      if (!requestScope.current()) return state
      updateFeatureInstances('hid', state.instances)
      hid.value = {
        available: state.available,
        backend: state.backend || '',
        initialized: state.initialized ?? false,
        online: state.online,
        supportsAbsoluteMouse: state.supports_absolute_mouse ?? false,
        keyboardLedsEnabled: state.keyboard_leds?.capabilities?.reports_state ?? false,
        ledState: {
          numLock: state.keyboard_leds?.state?.num_lock ?? false,
          capsLock: state.keyboard_leds?.state?.caps_lock ?? false,
          scrollLock: state.keyboard_leds?.state?.scroll_lock ?? false,
        },
        device: state.device ?? null,
        error: state.fault?.message ?? state.error ?? null,
        errorCode: state.error_code ?? null,
        fault: state.fault ?? null,
      }
      return state
    } catch (e) {
      console.error('Failed to fetch HID state:', e)
      throw e
    }
  }

  async function fetchAtxState() {
    const requestScope = deviceScope.capture()
    try {
      const state = await powerStatusResource.refresh()
      if (!requestScope.current()) return state
      atx.value = {
        available: state.available,
        backend: state.backend,
        initialized: state.initialized,
        powerOn: state.power_status === 'on',
        error: state.fault?.message ?? null,
        fault: state.fault ?? null,
      }
      return state
    } catch (e) {
      console.error('Failed to fetch ATX state:', e)
      throw e
    }
  }

  async function fetchMsdState(discover = false) {
    const requestScope = deviceScope.capture()
    void discover
    try {
      const result = await msdStatusResource.refresh()
      if (!requestScope.current()) return result
      applyMsdStatus(result)
      return result
    } catch (e) {
      console.error('Failed to fetch MSD state:', e)
      throw e
    }
  }

  function applyMsdStatus(result: Awaited<ReturnType<typeof msdApi.status>>) {
    updateFeatureInstances('msd', result.instances)
    msd.value = {
      available: result.available ?? true,
      attachments: result.attachments ?? [],
      occupancy: result.occupancy ?? {
        state: result.fault?.code === 'session_in_use' ? 'external' : 'idle',
        details_available: result.fault?.code !== 'session_in_use',
      },
      error: result.fault?.message ?? null,
      fault: result.fault ?? null,
      capabilities: result.capabilities ?? {
        virtual_drive: false,
        slots: [],
        storage_locations: [],
        unavailable_storage_locations: [],
        target_management: { media_types: [], deletable: false, limits: [], max_targets: null },
      },
      backendStatus: result.backend_status && typeof result.backend_status === 'object'
        ? result.backend_status as Record<string, unknown>
        : null,
    }
  }

  watch(msdStatusResource.data, result => {
    if (result) applyMsdStatus(result)
  })

  async function fetchStreamState() {
    const requestScope = deviceScope.capture()
    try {
      const [status, modeResp] = await Promise.all([
        streamApi.status(),
        streamApi.getMode().catch(() => ({ success: false, mode: 'mjpeg' }))
      ])
      if (!requestScope.current()) return status
      updateFeatureInstances('video', status.instances)
      stream.value = {
        online: status.streaming,
        active: status.streaming || status.connected,
        device: null,
        format: null,
        resolution: null,
        targetFps: 0,
        clients: 0,
        streamMode: modeResp.mode || 'mjpeg',
        error: status.fault?.message ?? null,
        fault: status.fault ?? null,
      }
      return status
    } catch (e) {
      console.error('Failed to fetch stream state:', e)
      throw e
    }
  }

  async function fetchAllStates() {
    const requestScope = deviceScope.capture()
    loading.value = true
    error.value = null

    try {
      await Promise.all([
        fetchSystemInfo(),
        fetchFeatures().catch(() => null),
        fetchFeatureStatuses().catch(() => null),
        fetchStreamState().catch(() => null),
        fetchHidState().catch(() => null),
        fetchAtxState().catch(() => null),
        fetchMsdState().catch(() => null),
      ])
    } catch (e) {
      if (!requestScope.current()) return
      error.value = e instanceof Error ? e.message : 'Failed to fetch states'
    } finally {
      if (requestScope.current()) loading.value = false
    }
  }

  /**
   * Update WebSocket connection state
   */
  function updateWsConnection(connected: boolean, networkError: boolean) {
    connectionState.value.wsConnected = connected
    connectionState.value.wsNetworkError = networkError
  }

  /**
   * Update HID feature event bus connection state
   */
  function updateHidEventBusConnection(connected: boolean, networkError: boolean) {
    connectionState.value.hidEventBusConnected = connected
    connectionState.value.hidEventBusNetworkError = networkError
  }

  /**
   * Update store state from WebSocket DeviceInfo event
   * Called when receiving system.device_info event from server
   */
  function updateFromDeviceInfo(data: DeviceInfoEvent) {
    const prevAudioStreaming = audio.value?.streaming ?? false
    const prevAudioDevice = audio.value?.device ?? null

    // Update video/stream state
    stream.value = {
      online: data.video.online,
      active: data.video.online || data.video.available,
      device: data.video.device,
      format: data.video.format,
      resolution: data.video.resolution,
      targetFps: data.video.fps,
      clients: stream.value?.clients ?? 0,
      streamMode: data.video.stream_mode || 'mjpeg',
      error: data.video.fault?.message ?? data.video.error,
      fault: data.video.fault ?? null,
    }

    // Update HID state
    hid.value = {
      available: data.hid.available,
      backend: data.hid.backend,
      initialized: data.hid.initialized,
      online: data.hid.online,
      supportsAbsoluteMouse: data.hid.supports_absolute_mouse,
      keyboardLedsEnabled: data.hid.keyboard_leds_enabled,
      ledState: {
        numLock: data.hid.led_state.num_lock,
        capsLock: data.hid.led_state.caps_lock,
        scrollLock: data.hid.led_state.scroll_lock,
      },
      device: data.hid.device,
      error: data.hid.fault?.message ?? data.hid.error,
      errorCode: data.hid.error_code ?? null,
      fault: data.hid.fault ?? null,
    }

    // Update MSD state (optional)
    if (data.msd) {
      msd.value = {
        available: data.msd.available,
        attachments: msd.value?.attachments ?? [],
        occupancy: msd.value?.occupancy ?? {
          state: data.msd.fault?.code === 'session_in_use' ? 'external' : 'idle',
          details_available: data.msd.fault?.code !== 'session_in_use',
          ...(data.msd.fault?.code === 'session_in_use' ? {} : { active_sessions: 0 }),
        },
        // Device-info events contain the common MSD shape, whereas the
        // feature status endpoint adds backend-specific media details.
        // Preserve those details until the next feature refresh.
        error: data.msd.fault?.message ?? data.msd.error,
        fault: data.msd.fault ?? null,
        capabilities: msd.value?.capabilities ?? {
          virtual_drive: false,
          slots: [],
          storage_locations: [],
          unavailable_storage_locations: [],
          target_management: { media_types: [], deletable: false, limits: [], max_targets: null },
        },
        backendStatus: msd.value?.backendStatus ?? null,
      }
    } else {
      msd.value = null
    }

    // Update ATX state (optional)
    if (data.atx) {
      atx.value = {
        available: data.atx.available,
        backend: data.atx.backend,
        initialized: data.atx.initialized,
        powerOn: data.atx.power_on,
        error: data.atx.fault?.message ?? data.atx.error,
        fault: data.atx.fault ?? null,
      }
    } else {
      atx.value = null
    }

    // Update Audio state (optional)
    if (data.audio) {
      audio.value = {
        available: data.audio.available,
        streaming: data.audio.streaming,
        device: data.audio.device,
        quality: data.audio.quality,
        error: data.audio.fault?.message ?? data.audio.error,
        fault: data.audio.fault ?? null,
      }
    } else {
      audio.value = null
    }

    ttyd.value = data.ttyd ?? null

    // Populate per-feature lifecycle state from device_info snapshot
    if (data.video?.connection?.state) {
      updateFeatureState('video', data.video.connection.state)
    }
    if (data.hid?.connection?.state) {
      updateFeatureState('hid', data.hid.connection.state)
    }
    if (data.msd?.connection?.state) {
      updateFeatureState('msd', data.msd.connection.state)
    }
    if (data.atx?.connection?.state) {
      updateFeatureState('power', data.atx.connection.state)
    }

    updateFeatureInstances('video', data.video.instances)
    updateFeatureInstances('hid', data.hid.instances)
    updateFeatureInstances('msd', data.msd?.instances)
    updateFeatureInstances('power', data.atx?.instances)

    const nextAudioStreaming = audio.value?.streaming ?? false
    const nextAudioDevice = audio.value?.device ?? null

    return {
      audioChanged: prevAudioStreaming !== nextAudioStreaming || prevAudioDevice !== nextAudioDevice,
      audio: {
        streaming: nextAudioStreaming,
        device: nextAudioDevice,
      },
    }
  }

  /**
   * Update stream clients count from WebSocket stream.stats_update event
   */
  function updateStreamClients(clients: number) {
    if (stream.value) {
      stream.value = {
        ...stream.value,
        clients,
      }
    }
  }

  /**
   * Set stream online status
   * Called when MJPEG video successfully loads (frontend-side detection)
   * This fixes the timing issue where device_info event arrives before stream is fully active
   */
  function setStreamOnline(online: boolean) {
    if (stream.value && stream.value.online !== online) {
      console.log('[Store] setStreamOnline:', online, '(was:', stream.value.online, ')')
      stream.value = {
        ...stream.value,
        online,
      }
    }
  }

  function updateFeatureState(feature: string, state: string, connectionId = 'default') {
    const normalizedState = normalizeFeatureState(state)
    const prev = featureConnectionStates.value[feature] ?? {}
    const nextForFeature = {
      ...prev,
      [connectionId]: {
        connectionId,
        state: normalizedState,
      },
    }
    featureConnectionStates.value = {
      ...featureConnectionStates.value,
      [feature]: nextForFeature,
    }
    featureStates.value = {
      ...featureStates.value,
      [feature]: summarizeFeatureState(feature),
    }
  }

  function updateFeatureHealth(
    feature: string,
    health: { online: boolean; fps: number; resolution?: [number, number]; width?: number; height?: number },
    connectionId = 'default',
  ) {
    const resolution = health.resolution ?? [health.width ?? 0, health.height ?? 0]
    const prev = featureConnectionHealth.value[feature] ?? {}
    featureConnectionHealth.value = {
      ...featureConnectionHealth.value,
      [feature]: {
        ...prev,
        [connectionId]: {
          connectionId,
          online: health.online,
          fps: health.fps,
          resolution,
        },
      },
    }
    summarizeFeatureHealth(feature)
  }

  /** Keep one fault snapshot per feature connection.  The human-readable
   * message is copied into the legacy card state only for presentation; the
   * typed fault remains the source of truth for codes and retryability. */
  function updateFeatureFault(
    feature: string,
    fault: FeatureFault | null,
    connectionId = 'default',
  ) {
    const previous = featureFaults.value[feature] ?? {}
    featureFaults.value = {
      ...featureFaults.value,
      [feature]: {
        ...previous,
        [connectionId]: fault,
      },
    }

    const message = fault?.message ?? null
    if (feature === 'video' && stream.value) {
      stream.value = { ...stream.value, error: message, fault }
    } else if (feature === 'hid' && hid.value) {
      hid.value = { ...hid.value, error: message, fault }
    } else if (feature === 'msd' && msd.value) {
      const occupancy = fault?.code === 'session_in_use'
        ? { state: 'external' as const, details_available: false }
        : msd.value.attachments.length > 0
          ? { state: 'managed' as const, details_available: true }
          : { state: 'idle' as const, details_available: true, active_sessions: 0 }
      msd.value = { ...msd.value, error: message, fault, occupancy }
    } else if (feature === 'power' && atx.value) {
      atx.value = { ...atx.value, error: message, fault }
    } else if (feature === 'audio' && audio.value) {
      audio.value = { ...audio.value, error: message, fault }
    }
  }

  function updateFeatureInstances(feature: string, instances: FeatureConnectionSnapshot[] | undefined) {
    if (!instances?.length) return
    const states: Record<string, FeatureConnectionStatus> = {}
    const health: Record<string, FeatureConnectionHealth> = {}

    for (const instance of instances) {
      const connectionId = instance.connection_id ?? 'default'
      if (instance.state) {
        states[connectionId] = {
          connectionId,
          state: normalizeFeatureState(instance.state),
        }
      }
      if (typeof instance.online === 'boolean') {
        health[connectionId] = {
          connectionId,
          online: instance.online,
          fps: instance.fps ?? 0,
          resolution: instance.resolution ?? [instance.width ?? 0, instance.height ?? 0],
        }
      }
      if ('fault' in instance) {
        updateFeatureFault(feature, instance.fault ?? null, connectionId)
      }
    }

    if (Object.keys(states).length > 0) {
      featureConnectionStates.value = {
        ...featureConnectionStates.value,
        [feature]: states,
      }
      featureStates.value = {
        ...featureStates.value,
        [feature]: summarizeFeatureState(feature),
      }
    }

    if (Object.keys(health).length > 0) {
      featureConnectionHealth.value = {
        ...featureConnectionHealth.value,
        [feature]: health,
      }
      summarizeFeatureHealth(feature)
    }
  }

  return {
    version,
    buildDate,
    platform,
    capabilities,
    diskSpace,
    deviceInfo,
    stream,
    hid,
    atx,
    msd,
    audio,
    ttyd,
    loading,
    error,
    connectionState,
    featureStates,
    featureHealth,
    featureConnectionStates,
    featureConnectionHealth,
    featureFaults,
    features,
    featureStatuses,
    availableFeatureIds,
    isFeatureAvailable,
    getFeatureBackend,
    fetchSystemInfo,
    fetchFeatures,
    fetchFeatureStatuses,
    startFeatureStatusPolling,
    stopFeatureStatusPolling,
    resetDeviceState,
    startStream,
    stopStream,
    fetchHidState,
    fetchAtxState,
    fetchMsdState,
    fetchAllStates,
    updateWsConnection,
    updateHidEventBusConnection,
    updateFromDeviceInfo,
    updateStreamClients,
    setStreamOnline,
    updateFeatureState,
    updateFeatureHealth,
    updateFeatureFault,
    updateFeatureInstances,
  }
})
