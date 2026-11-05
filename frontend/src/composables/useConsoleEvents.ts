import { watch, type WatchStopHandle } from 'vue'
import { useI18n } from 'vue-i18n'
import { toast } from 'vue-sonner'
import { useSystemStore } from '@/stores/system'
import type { DeviceInfoEvent } from '@/stores/system'
import type { StreamDeviceLostEventData } from '@/types/websocket'
import type { FeatureFault } from '@/api/request'
import { useWebSocket } from '@/composables/useWebSocket'
import { isAudioStreamDeviceLostPayload } from '@/lib/streamSignal'

export interface ConsoleEventHandlers {
  onStreamConfigChanging?: (data: { reason?: string }) => void
  onStreamConfigApplied?: (data: { device: string; resolution: [number, number]; fps: number }) => void
  onStreamStatsUpdate?: (data: { clients?: number; clients_stat?: Record<string, { fps: number }> }) => void
  onStreamModeChanged?: (data: { mode: string; previous_mode: string }) => void
  onStreamModeSwitching?: (data: { transition_id: string; to_mode: string; from_mode: string }) => void
  onStreamModeReady?: (data: { transition_id: string; mode: string }) => void
  onWebRTCReady?: (data: { codec: string; hardware: boolean; transition_id?: string }) => void
  onStreamStateChanged?: (data: {
    state: string
    device?: string | null
    reason?: string | null
    next_retry_ms?: number | null
  }) => void
  onStreamDeviceLost?: (data: StreamDeviceLostEventData) => void
  onStreamReconnecting?: (data: { device: string; attempt: number }) => void
  onStreamRecovered?: (data: { device: string }) => void
  onDeviceInfo?: (data: DeviceInfoEvent) => void
  onConnectionStateChanged?: (data: {
    feature: string
    connection_id: string
    state: string
  }) => void
  onConnectionHealth?: (data: {
    feature: string
    connection_id: string
    online: boolean
    fps: number
    width?: number
    height?: number
    resolution?: [number, number]
  }) => void
  onFeatureFaultChanged?: (data: {
    feature: string
    connection_id: string
    fault: FeatureFault | null
  }) => void
}

export function useConsoleEvents(handlers: ConsoleEventHandlers) {
  const { t } = useI18n()
  const systemStore = useSystemStore()
  const { on, off, setFeatures, connect } = useWebSocket()
  const noop = () => {}
  let stopFeatureLeaseWatch: WatchStopHandle | null = null

  function handleStreamDeviceLost(data: StreamDeviceLostEventData) {
    const audioLost = isAudioStreamDeviceLostPayload(data)
    if (systemStore.stream && !audioLost) {
      systemStore.stream.online = false
    }
    toast.error(t(audioLost ? 'audio.deviceLost' : 'console.deviceLost'), {
      description: t(audioLost ? 'audio.deviceLostDesc' : 'console.deviceLostDesc', {
        device: data.device,
        reason: data.reason,
      }),
      duration: 5000,
    })
    handlers.onStreamDeviceLost?.(data)
  }

  function handleStreamReconnecting(data: { device: string; attempt: number }) {
    handlers.onStreamReconnecting?.(data)
  }

  function handleStreamRecovered(_data: { device: string }) {
    if (systemStore.stream) {
      systemStore.stream.online = true
    }
    handlers.onStreamRecovered?.(_data)
  }

  function handleStreamStateChangedForward(data: { state: string; device?: string | null }) {
    handlers.onStreamStateChanged?.(data)
  }

  function subscribe() {
    // Keep the video source lifecycle observable even when no render surface
    // is mounted (for example while the BMC reports no signal). The device
    // event socket owns this lightweight lease until the console disconnects.
    // Feature discovery is asynchronous, so the lease must follow the store
    // instead of taking a one-time snapshot during component activation.
    stopFeatureLeaseWatch?.()
    stopFeatureLeaseWatch = watch(
      () => systemStore.availableFeatureIds.includes('video'),
      available => setFeatures(available ? ['video'] : []),
      { immediate: true },
    )

    on('stream.config_changing', handlers.onStreamConfigChanging ?? noop)
    on('stream.config_applied', handlers.onStreamConfigApplied ?? noop)
    on('stream.stats_update', handlers.onStreamStatsUpdate ?? noop)
    on('stream.mode_changed', handlers.onStreamModeChanged ?? noop)
    on('stream.mode_switching', handlers.onStreamModeSwitching ?? noop)
    on('stream.mode_ready', handlers.onStreamModeReady ?? noop)
    on('stream.webrtc_ready', handlers.onWebRTCReady ?? noop)
    on('stream.state_changed', handleStreamStateChangedForward)
    on('stream.device_lost', handleStreamDeviceLost)
    on('stream.reconnecting', handleStreamReconnecting)
    on('stream.recovered', handleStreamRecovered)

    on('system.device_info', handlers.onDeviceInfo ?? noop)

    on('connection.state_changed', handlers.onConnectionStateChanged ?? noop)
    on('connection.health', handlers.onConnectionHealth ?? noop)
    on('feature.fault_changed', handlers.onFeatureFaultChanged ?? noop)

    connect()
  }

  function unsubscribe() {
    stopFeatureLeaseWatch?.()
    stopFeatureLeaseWatch = null
    setFeatures([])
    off('stream.config_changing', handlers.onStreamConfigChanging ?? noop)
    off('stream.config_applied', handlers.onStreamConfigApplied ?? noop)
    off('stream.stats_update', handlers.onStreamStatsUpdate ?? noop)
    off('stream.mode_changed', handlers.onStreamModeChanged ?? noop)
    off('stream.mode_switching', handlers.onStreamModeSwitching ?? noop)
    off('stream.mode_ready', handlers.onStreamModeReady ?? noop)
    off('stream.webrtc_ready', handlers.onWebRTCReady ?? noop)
    off('stream.state_changed', handleStreamStateChangedForward)
    off('stream.device_lost', handleStreamDeviceLost)
    off('stream.reconnecting', handleStreamReconnecting)
    off('stream.recovered', handleStreamRecovered)

    off('system.device_info', handlers.onDeviceInfo ?? noop)

    off('connection.state_changed', handlers.onConnectionStateChanged ?? noop)
    off('connection.health', handlers.onConnectionHealth ?? noop)
    off('feature.fault_changed', handlers.onFeatureFaultChanged ?? noop)
  }

  return {
    subscribe,
    unsubscribe,
  }
}
