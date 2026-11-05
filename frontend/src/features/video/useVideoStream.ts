import { ref, computed, watch, nextTick, type Ref, type WatchStopHandle } from 'vue'
import { DeviceScope } from '@/api/deviceScope'
import { currentBmcIdFromLocation } from '@/api/request'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import { useSystemStore, type DeviceInfoEvent } from '@/stores/system'
import { useVideoConfigStore } from '@/stores/videoConfig'
import { useVideoStore } from './store'
import { streamApi, type VideoTransport } from '@/api'
import { toast } from 'vue-sonner'
import { videoDebugLog } from '@/lib/debugLog'
import { generateUUID } from '@/lib/utils'
import type { StreamDeviceLostEventData } from '@/types/websocket'
import type { VideoMode } from './store'
import type { FeatureFault } from '@/api/request'
import { isAudioStreamDeviceLostPayload, isAudioDeviceLostStateReason } from '@/lib/streamSignal'
import {
  initialVideoLifecycle,
  presentVideoLifecycle,
  transitionVideoLifecycle,
  type VideoLifecycleEvent,
} from './lifecycle'
import { MjpegBrowserTransport } from './mjpegStream'

export interface VideoStreamOptions {
  videoRef: Ref<HTMLImageElement | null>
  webrtcVideoRef: Ref<HTMLVideoElement | null>
  webTransportCanvasRef: Ref<HTMLCanvasElement | null>
  videoContainerRef: Ref<HTMLDivElement | null>
  webrtc: ReturnType<typeof import('@/composables/useWebRTC').useWebRTC>
  webtransport: ReturnType<typeof import('@/composables/useWebTransport').useWebTransport>
  videoSession: ReturnType<typeof import('@/composables/useVideoSession').useVideoSession>
  unifiedAudio: ReturnType<typeof import('@/composables/useUnifiedAudio').getUnifiedAudio>
  isConsoleActive: Ref<boolean>
}

export function useVideoStream(opts: VideoStreamOptions) {
  const { t } = useI18n()
  const systemStore = useSystemStore()
  const videoConfigStore = useVideoConfigStore()
  const videoStore = useVideoStore()
  videoStore.reset()

  const {
    videoMode, httpModes, backendFps, mjpegFrameReceived, mjpegTimestamp,
    videoLoading, videoError, videoErrorMessage, videoRestarting,
    backendPerf, selectedEncoderBackend, clientsStats, isHttpMode, isWebTransportMode,
    transportPreference, webTransportCapabilities, activeTransport,
  } = storeToRefs(videoStore)
  const { setVideoMode, setActiveTransport } = videoStore

  const videoDeviceId = currentBmcIdFromLocation()
  const videoWorkScope = new DeviceScope(currentBmcIdFromLocation)
  let videoDisposed = false
  const stopVideoActivationWatch = watch(opts.isConsoleActive, (active) => {
    if (!active) {
      suspendVideoWork()
    } else if (currentBmcIdFromLocation() === videoDeviceId) {
      videoRestarting.value = false
      if (isHttpMode.value && mjpegTimestamp.value !== 0) refreshVideo()
    }
  }, { flush: 'sync' })

  function captureVideoWork() {
    const owner = videoWorkScope.capture()
    return () => !videoDisposed && opts.isConsoleActive.value
      && owner.id === videoDeviceId && owner.current()
  }

  const myClientId = generateUUID()

  // ──────────────────────────────────────────
  // Retry / Recovery state
  // ──────────────────────────────────────────
  let retryTimeoutId: number | null = null
  let retryCount = 0
  let gracePeriodTimeoutId: number | null = null
  let consecutiveErrors = 0
  const BASE_RETRY_DELAY = 2000
  const GRACE_PERIOD = 2000
  const MAX_CONSECUTIVE_ERRORS = 2
  let pendingWebRTCReadyGate = false
  let webrtcConnectTask: Promise<boolean> | null = null

  let webrtcRecoveryTimerId: number | null = null
  let webrtcRecoveryAttempts = 0
  const MAX_WEBRTC_RECOVERY_ATTEMPTS = 8
  const WEBRTC_RECOVERY_BASE_DELAY = 2000

  const frameOverlayUrl = ref<string | null>(null)
  const mjpegFrameUrl = ref('')
  const lifecycle = ref(initialVideoLifecycle)
  const mjpegTransport = new MjpegBrowserTransport()

  function transitionLifecycle(event: VideoLifecycleEvent) {
    lifecycle.value = transitionVideoLifecycle(lifecycle.value, event)
  }

  const webrtcProgressStages = [
    'fetching_ice_servers',
    'creating_peer_connection',
    'creating_offer',
    'waiting_server_answer',
    'setting_remote_description',
    'applying_ice_candidates',
    'waiting_connection',
  ] as const

  const mjpegProgressStages = [
    'connecting_websocket',
    'requesting_stream',
    'waiting_first_frame',
  ] as const

  type MjpegProgressStage = (typeof mjpegProgressStages)[number]

  const webTransportProgressStages = [
    'connecting_transport',
    'waiting_first_frame',
  ] as const

  type WebTransportProgressStage = (typeof webTransportProgressStages)[number]

  let webrtcReconnectTimeout: ReturnType<typeof setTimeout> | null = null
  let webrtcReconnectFailures = 0

  let isRefreshingVideo = false
  let initialDeviceInfoReceived = false
  let initialModeRestoreDone = false
  let initialModeRestoreInProgress = false
  let mjpegRefreshGeneration = 0
  let mjpegRefreshTimeoutId: number | null = null
  let mjpegFirstFrameDeadlineId: number | null = null
  let lastWebRtcDecodedFrames = 0
  let lastWebTransportDecodedFrames = 0
  let intrinsicSizeTimerId: number | null = null
  const MJPEG_FIRST_FRAME_DEADLINE_MS = 15_000
  const stopHandles: WatchStopHandle[] = []

  const videoSourceSize = ref<{ width: number; height: number } | null>(null)

  function updateVideoSourceSize(width: number, height: number): void {
    if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) return
    const normalized = { width: Math.round(width), height: Math.round(height) }
    const current = videoSourceSize.value
    if (current?.width === normalized.width && current.height === normalized.height) return
    videoSourceSize.value = normalized
  }

  /** Collect the intrinsic decoded size at the browser surface. All transports
   * converge here so the viewport never depends on a transport-specific
   * aspect-ratio string or a one-shot load event. */
  function syncVideoSourceSize(): void {
    if (!captureVideoWork()()) return
    if (isHttpMode.value) {
      const image = opts.videoRef.value
      if (image?.naturalWidth && image.naturalHeight) {
        updateVideoSourceSize(image.naturalWidth, image.naturalHeight)
      }
      return
    }

    if (isWebTransportMode.value) {
      const stats = opts.webtransport.stats.value
      if (stats.frameWidth && stats.frameHeight) {
        updateVideoSourceSize(stats.frameWidth, stats.frameHeight)
        return
      }
      const canvas = opts.webTransportCanvasRef.value
      if (canvas?.width && canvas.height) updateVideoSourceSize(canvas.width, canvas.height)
      return
    }

    const video = opts.webrtcVideoRef.value
    if (video?.videoWidth && video.videoHeight) {
      updateVideoSourceSize(video.videoWidth, video.videoHeight)
      return
    }
    const stats = opts.webrtc.stats.value
    if (stats.frameWidth && stats.frameHeight) {
      updateVideoSourceSize(stats.frameWidth, stats.frameHeight)
    }
  }

  // ──────────────────────────────────────────
  // Frame overlay
  // ──────────────────────────────────────────
  function clearFrameOverlay() {
    frameOverlayUrl.value = null
  }

  async function captureFrameOverlay() {
    try {
      const canvas = document.createElement('canvas')
      const ctx = canvas.getContext('2d')
      if (!ctx) return
      const MAX_WIDTH = 1280

      if (isHttpMode.value) {
        const img = opts.videoRef.value
        if (!img || !img.naturalWidth || !img.naturalHeight) return
        const scale = Math.min(1, MAX_WIDTH / img.naturalWidth)
        canvas.width = Math.max(1, Math.round(img.naturalWidth * scale))
        canvas.height = Math.max(1, Math.round(img.naturalHeight * scale))
        ctx.drawImage(img, 0, 0, canvas.width, canvas.height)
      } else {
        const video = opts.webrtcVideoRef.value
        if (!video || !video.videoWidth || !video.videoHeight) return
        const scale = Math.min(1, MAX_WIDTH / video.videoWidth)
        canvas.width = Math.max(1, Math.round(video.videoWidth * scale))
        canvas.height = Math.max(1, Math.round(video.videoHeight * scale))
        ctx.drawImage(video, 0, 0, canvas.width, canvas.height)
      }
      frameOverlayUrl.value = canvas.toDataURL('image/jpeg', 0.7)
    } catch { /* ignore */ }
  }

  // ──────────────────────────────────────────
  // WebRTC connection helpers
  // ──────────────────────────────────────────
  function waitForVideoFirstFrame(el: HTMLVideoElement, timeoutMs = 2000): Promise<boolean> {
    return new Promise((resolve) => {
      let done = false
      const cleanup = () => {
        el.removeEventListener('loadeddata', onReady)
        el.removeEventListener('playing', onReady)
      }
      const onReady = () => {
        if (done) return
        done = true; cleanup(); resolve(true)
      }
      el.addEventListener('loadeddata', onReady)
      el.addEventListener('playing', onReady)
      setTimeout(() => {
        if (done) return
        done = true; cleanup(); resolve(false)
      }, timeoutMs)
    })
  }

  function shouldSuppressAutoReconnect(): boolean {
    return isHttpMode.value
      || isWebTransportMode.value
      || !opts.isConsoleActive.value
      || opts.videoSession.localSwitching.value
      || opts.videoSession.backendSwitching.value
      || videoRestarting.value
  }

  function markWebRTCFailure(reason: string, description?: string) {
    if (!captureVideoWork()()) return
    if (isHttpMode.value || isWebTransportMode.value) {
      videoDebugLog('Ignoring WebRTC failure outside active WebRTC transport', {
        reason,
        description,
        videoMode: videoMode.value,
        isHttpMode: isHttpMode.value,
        isWebTransportMode: isWebTransportMode.value,
        webrtcState: opts.webrtc.state.value,
        webrtcStage: opts.webrtc.connectStage.value,
      })
      return
    }
    const video = opts.webrtcVideoRef.value
    const hasRenderedVideo = !!video && video.videoWidth > 0 && video.videoHeight > 0 && video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA
    const hasDecodedFrames = (opts.webrtc.stats.value.framesDecoded ?? 0) > 0 || (opts.webrtc.stats.value.bytesReceived ?? 0) > 0
    if (!isHttpMode.value && !isWebTransportMode.value && (opts.webrtc.isConnected.value || hasRenderedVideo || hasDecodedFrames)) {
      videoDebugLog('Suppressing stale WebRTC failure because video is active', {
        reason,
        description,
        webrtcState: opts.webrtc.state.value,
        readyState: video?.readyState,
        videoWidth: video?.videoWidth,
        videoHeight: video?.videoHeight,
        framesDecoded: opts.webrtc.stats.value.framesDecoded,
        bytesReceived: opts.webrtc.stats.value.bytesReceived,
      })
      videoError.value = false
      videoErrorMessage.value = ''
      videoLoading.value = false
      systemStore.setStreamOnline(true)
      return
    }
    videoDebugLog('Marking WebRTC failure', {
      reason, description,
      videoMode: videoMode.value,
      webrtcState: opts.webrtc.state.value,
      webrtcStage: opts.webrtc.connectStage.value,

    })
    pendingWebRTCReadyGate = false
    videoError.value = true
    videoErrorMessage.value = reason
    transitionLifecycle({ type: 'fault', message: reason })
    videoLoading.value = false
    systemStore.setStreamOnline(false)
    toast.error(reason, { description: description ?? '', duration: 5000 })
  }

  function errorMessage(err: unknown, fallback: string): string {
    if (err instanceof Error && err.message.trim()) return err.message
    if (typeof err === 'string' && err.trim()) return err
    return fallback
  }

  function isSupersededWebTransportConnect(err: unknown): boolean {
    return errorMessage(err, '') === 'webtransport_connection_superseded'
  }

  function currentWebRTCError(fallback: string): string {
    return opts.webrtc.error.value || fallback
  }

  async function waitForWebRTCReadyGate(reason: string, timeoutMs = 3000) {
    const current = captureVideoWork()
    if (!current()) return

    if (!pendingWebRTCReadyGate) return
    videoDebugLog('Waiting for WebRTC backend ready gate', { reason, timeoutMs })
    const ready = await opts.videoSession.waitForWebRTCReadyAny(timeoutMs)
    if (!current()) return
    if (!ready) {
      console.warn(`[WebRTC] Ready gate timeout (${reason}), attempting connection anyway`)
    }
    pendingWebRTCReadyGate = false
  }

  async function connectWebRTCSerial(reason: string, codec: VideoMode = videoMode.value): Promise<boolean> {
    const current = captureVideoWork()
    if (!current()) return false

    if (webrtcConnectTask) return webrtcConnectTask
    const task = (async () => {
      await waitForWebRTCReadyGate(reason)
      if (!current()) return false
      return opts.webrtc.connect({
        codec,
        encoder: selectedEncoderBackend.value,
      })
    })()
    webrtcConnectTask = task
    try {
      const result = await task
      return current() && result
    } finally { if (webrtcConnectTask === task) webrtcConnectTask = null }
  }

  // ──────────────────────────────────────────
  // MJPEG handlers
  // ──────────────────────────────────────────
  function stopMjpegStream() {
    mjpegTransport.stop()
    mjpegFrameUrl.value = ''
  }

  async function startMjpegStream(generation: number) {
    const current = captureVideoWork()
    if (!current()) return

    stopMjpegStream()
    const url = httpStreamUrl.value
    if (!url || generation !== mjpegRefreshGeneration) return

    try {
      await mjpegTransport.start(url, (frameUrl) => {
        if (current() && generation === mjpegRefreshGeneration && isHttpMode.value) {
          mjpegFrameUrl.value = frameUrl
        }
      })
      if (!current()) return
    } catch (error) {
      if (!current()) return

      if (generation !== mjpegRefreshGeneration) return
      videoDebugLog('MJPEG fetch stream failed', {
        error: errorMessage(error, 'mjpeg_stream_failed'),
        generation,
      })
      isRefreshingVideo = false
      await handleVideoError()
    }
  }

  function handleVideoLoad() {
    if (isHttpMode.value) {
      mjpegTransport.frameRendered()
      transitionLifecycle({ type: 'frame_received' })
      mjpegFrameReceived.value = true
      isRefreshingVideo = false
      if (mjpegRefreshTimeoutId !== null) {
        clearTimeout(mjpegRefreshTimeoutId)
        mjpegRefreshTimeoutId = null
      }
      if (mjpegFirstFrameDeadlineId !== null) {
        clearTimeout(mjpegFirstFrameDeadlineId)
        mjpegFirstFrameDeadlineId = null
      }
      systemStore.setStreamOnline(true)
      syncVideoSourceSize()
    }
    if (!videoLoading.value) return
    if (retryTimeoutId !== null) {
      clearTimeout(retryTimeoutId); retryTimeoutId = null
    }
    if (gracePeriodTimeoutId !== null) {
      clearTimeout(gracePeriodTimeoutId); gracePeriodTimeoutId = null
    }
    videoLoading.value = false
    videoError.value = false
    videoErrorMessage.value = ''
    videoRestarting.value = false
    retryCount = 0
    consecutiveErrors = 0
    clearFrameOverlay()
    const container = opts.videoContainerRef.value
    if (container && typeof container.focus === 'function') container.focus()
  }

  /** Stop the browser-side MJPEG request when the source lifecycle has
   * already reported a structured failure. Retaining its URL would keep the
   * image element in a first-frame wait despite there being no viable source. */
  function handleVideoFeatureFault(fault: FeatureFault | null) {
    if (!fault || !isHttpMode.value) return
    stopMjpegStream()
    if (retryTimeoutId !== null) {
      clearTimeout(retryTimeoutId)
      retryTimeoutId = null
    }
    if (mjpegRefreshTimeoutId !== null) {
      clearTimeout(mjpegRefreshTimeoutId)
      mjpegRefreshTimeoutId = null
    }
    if (mjpegFirstFrameDeadlineId !== null) {
      clearTimeout(mjpegFirstFrameDeadlineId)
      mjpegFirstFrameDeadlineId = null
    }
    isRefreshingVideo = false
    mjpegTimestamp.value = 0
    mjpegFrameReceived.value = false
    videoLoading.value = false
    videoError.value = true
    videoErrorMessage.value = fault.message
    transitionLifecycle({ type: 'fault', message: fault.message })
    systemStore.setStreamOnline(false)
  }

  async function handleVideoError() {
    const current = captureVideoWork()
    if (!current()) return

    if (!isHttpMode.value) return
    if (opts.videoSession.localSwitching.value) return
    stopMjpegStream()

    // Ask the typed status API for the sanitized backend failure before
    // treating a fetch/decode failure as a transient MJPEG transport error.
    // This prevents a failed source connect from appearing as an infinite
    // "waiting for first frame" spinner.
    const status = await streamApi.status().catch(() => null)
    if (!current()) return
    if (status?.fault) {
      handleVideoFeatureFault(status.fault)
      return
    }

    if (isRefreshingVideo) return

    if (streamSignalState.value !== 'ok') {
      if (retryTimeoutId !== null) {
        clearTimeout(retryTimeoutId)
        retryTimeoutId = null
      }
      videoLoading.value = false
      mjpegFrameReceived.value = false
      return
    }

    consecutiveErrors++

    if (consecutiveErrors > MAX_CONSECUTIVE_ERRORS && gracePeriodTimeoutId !== null) {
      clearTimeout(gracePeriodTimeoutId); gracePeriodTimeoutId = null
      videoRestarting.value = false
    }
    if (videoRestarting.value || gracePeriodTimeoutId !== null) return
    if (retryTimeoutId !== null) { clearTimeout(retryTimeoutId); retryTimeoutId = null }

    videoLoading.value = true
    transitionLifecycle({ type: 'connect_requested' })
    mjpegFrameReceived.value = false
    retryCount++
    const delay = BASE_RETRY_DELAY * Math.pow(1.5, Math.min(retryCount - 1, 5))
    retryTimeoutId = window.setTimeout(() => {
      if (!current()) return
      retryTimeoutId = null
      refreshVideo()
    }, delay)
  }

  // ──────────────────────────────────────────
  // WebRTC Recovery
  // ──────────────────────────────────────────
  function scheduleWebRTCRecovery() {
    const current = captureVideoWork()
    if (!current()) return
    if (webrtcRecoveryTimerId !== null) {
      clearTimeout(webrtcRecoveryTimerId); webrtcRecoveryTimerId = null
    }
    if (webrtcRecoveryAttempts >= MAX_WEBRTC_RECOVERY_ATTEMPTS) {
      webrtcRecoveryAttempts = 0; return
    }
    const delay = Math.min(WEBRTC_RECOVERY_BASE_DELAY * Math.pow(2, webrtcRecoveryAttempts), 30000)
    webrtcRecoveryTimerId = window.setTimeout(async () => {
      if (!current()) return
      webrtcRecoveryTimerId = null
      webrtcRecoveryAttempts++
      if (isHttpMode.value || !videoError.value) { webrtcRecoveryAttempts = 0; return }
      try {
        await opts.webrtc.disconnect()
        if (!current()) return
        const ok = await connectWebRTCSerial('device-recovery')
        if (!current()) return
        if (ok) {
          videoError.value = false; videoErrorMessage.value = ''; webrtcRecoveryAttempts = 0
        } else { scheduleWebRTCRecovery() }
      } catch { if (current()) scheduleWebRTCRecovery() }
    }, delay)
  }

  function cancelWebRTCRecovery() {
    if (webrtcRecoveryTimerId !== null) {
      clearTimeout(webrtcRecoveryTimerId); webrtcRecoveryTimerId = null
    }
    webrtcRecoveryAttempts = 0
  }

  // ──────────────────────────────────────────
  // Stream event handlers
  // ──────────────────────────────────────────
  function handleStreamDeviceLost(data: StreamDeviceLostEventData) {
    if (isAudioStreamDeviceLostPayload(data)) return
    handleStreamStateChanged({ state: 'device_lost', reason: data.reason })
  }

  function handleStreamRecovered(_data: { device: string }) {
    cancelWebRTCRecovery()
    videoError.value = false
    videoErrorMessage.value = ''
    refreshVideo()
  }

  function handleStreamConfigChanging(_data: { reason?: string }) {
    if (retryTimeoutId !== null) { clearTimeout(retryTimeoutId); retryTimeoutId = null }
    if (gracePeriodTimeoutId !== null) { clearTimeout(gracePeriodTimeoutId); gracePeriodTimeoutId = null }
    videoRestarting.value = true
    transitionLifecycle({ type: 'switch_requested' })
    pendingWebRTCReadyGate = true
    videoLoading.value = true
    videoError.value = false
    retryCount = 0
    consecutiveErrors = 0
    backendFps.value = 0
  }

  async function handleStreamConfigApplied(_data: { device: string; resolution: [number, number]; fps: number }) {
    const current = captureVideoWork()
    if (!current()) return

    consecutiveErrors = 0
    gracePeriodTimeoutId = window.setTimeout(() => {
      gracePeriodTimeoutId = null; consecutiveErrors = 0
    }, GRACE_PERIOD)
    videoRestarting.value = true
    if (opts.videoSession.localSwitching.value) return
    if (!httpModes.value.includes(videoMode.value)) {
      await switchToWebRTC(videoMode.value)
      if (!current()) return
    } else {
      refreshVideo()
    }
    videoRestarting.value = false
  }

  function handleWebRTCReady(data: { codec: string; hardware: boolean; transition_id?: string }) {
    pendingWebRTCReadyGate = false
    opts.videoSession.onWebRTCReady(data)
  }

  function handleStreamModeReady(data: { transition_id: string; mode: string }) {
    opts.videoSession.onModeReady(data)
    if (httpModes.value.includes(data.mode)) pendingWebRTCReadyGate = false
    videoRestarting.value = false
  }

  function handleStreamModeSwitching(data: { transition_id: string; to_mode: string; from_mode: string }) {
    transitionLifecycle({ type: 'switch_requested' })
    if (!opts.videoSession.localSwitching.value) {
      videoRestarting.value = true
      videoLoading.value = true
      captureFrameOverlay().catch(() => {})
    }
    pendingWebRTCReadyGate = true
    opts.videoSession.onModeSwitching(data)
  }

  const streamSignalState = ref<'ok' | 'no_signal' | 'device_lost' | 'device_busy'>('ok')
  const streamSignalReason = ref<string | null>(null)
  const streamNextRetryMs = ref<number | null>(null)

  const mjpegConnectStage = computed<MjpegProgressStage | null>(() => {
    if (!isHttpMode.value) return null
    if (videoRestarting.value) return null
    if (videoError.value) return null
    if (mjpegTimestamp.value === 0) return 'requesting_stream'
    if (!mjpegFrameReceived.value) return 'waiting_first_frame'
    return null
  })

  const webTransportConnectStage = computed<WebTransportProgressStage | null>(() => {
    if (!isWebTransportMode.value) return null
    if (videoRestarting.value) return null
    if (opts.webtransport.state.value === 'connecting') return 'connecting_transport'
    if (opts.webtransport.state.value === 'connected' && opts.webtransport.stats.value.framesDecoded === 0) return 'waiting_first_frame'
    return null
  })

  const loadingMessage = computed(() => {
    if (isHttpMode.value) {
      if (videoRestarting.value) return t('console.videoRestarting')
      switch (mjpegConnectStage.value) {
        case 'connecting_websocket':
          return t('console.mjpegPhaseWebsocket')
        case 'requesting_stream':
          return t('console.mjpegPhaseStream')
        case 'waiting_first_frame':
          return t('console.mjpegPhaseFirstFrame')
        default:
          return t('console.connecting')
      }
    }

    if (isWebTransportMode.value) {
      if (videoRestarting.value) return t('console.videoRestarting')
      switch (webTransportConnectStage.value) {
        case 'connecting_transport':
          return t('console.connecting')
        case 'waiting_first_frame':
          return t('console.mjpegPhaseFirstFrame')
        default:
          return opts.webtransport.state.value === 'failed'
            ? t('console.connectionFailed')
            : t('console.connecting')
      }
    }

    switch (opts.webrtc.connectStage.value) {
      case 'fetching_ice_servers':
        return t('console.webrtcPhaseIceServers')
      case 'creating_peer_connection':
        return t('console.webrtcPhaseCreatePeer')
      case 'creating_offer':
        return t('console.webrtcPhaseCreateOffer')
      case 'waiting_server_answer':
        return t('console.webrtcPhaseWaitAnswer')
      case 'setting_remote_description':
        return t('console.webrtcPhaseSetRemote')
      case 'applying_ice_candidates':
        return t('console.webrtcPhaseApplyIce')
      case 'waiting_connection':
        return t('console.webrtcPhaseNegotiating')
      case 'connected':
        return t('console.webrtcConnected')
      case 'failed':
        return t('console.webrtcFailed')
      default:
        return videoRestarting.value ? t('console.videoRestarting') : t('console.connecting')
    }
  })

  const connectProgress = computed<{ current: number; total: number } | null>(() => {
    if (isHttpMode.value) {
      const stage = mjpegConnectStage.value
      if (!stage) return null
      return {
        current: mjpegProgressStages.indexOf(stage) + 1,
        total: mjpegProgressStages.length,
      }
    }

    if (isWebTransportMode.value) {
      const stage = webTransportConnectStage.value
      if (!stage) return null
      return {
        current: webTransportProgressStages.indexOf(stage) + 1,
        total: webTransportProgressStages.length,
      }
    }

    const idx = webrtcProgressStages.indexOf(opts.webrtc.connectStage.value as (typeof webrtcProgressStages)[number])
    if (idx < 0) return null

    return {
      current: idx + 1,
      total: webrtcProgressStages.length,
    }
  })

  function clearInactiveVideoSurface() {
    clearFrameOverlay()
    const cleanup: Record<VideoTransport, () => void> = {
      http: () => {
        stopMjpegStream()
        mjpegTimestamp.value = 0
        mjpegFrameReceived.value = false
        isRefreshingVideo = false
      },
      webrtc: () => {
        if (opts.webrtcVideoRef.value) opts.webrtcVideoRef.value.srcObject = null
      },
      webtransport: () => {
        const canvas = opts.webTransportCanvasRef.value
        if (canvas) canvas.getContext('2d')?.clearRect(0, 0, canvas.width, canvas.height)
      },
    }
    cleanup[activeTransport.value]()
  }

  const signalOverlayInfo = computed(() => {
    const reason = streamSignalReason.value
    const reasonDetail = reason ? t(`console.signal.reason.${reason}`) : ''
    if (streamSignalState.value === 'no_signal') {
      return {
        tone: 'info',
        title: t('console.signal.noSignal.title'),
        detail: reasonDetail || t('console.signal.noSignal.detail'),
        hint: streamNextRetryMs.value ? t('console.retryCount', { count: Math.ceil(streamNextRetryMs.value / 1000) }) : '',
      }
    }
    if (streamSignalState.value === 'device_busy') {
      return {
        tone: 'info',
        title: t('console.signal.deviceBusy.title'),
        detail: reasonDetail || t('console.signal.deviceBusy.detail'),
        hint: '',
      }
    }
    return {
      tone: 'error',
      title: t('console.signal.deviceLost.title'),
      detail: reasonDetail || t('console.signal.deviceLost.detail'),
      hint: '',
    }
  })

  type BackendStreamState = 'streaming' | 'ready' | 'uninitialized' | 'no_signal' | 'device_lost' | 'device_busy' | 'error'
  type VisibleSignalState = typeof streamSignalState.value

  const recoverableSignalStates: VisibleSignalState[] = ['no_signal', 'device_lost', 'device_busy']

  function isRecoverableSignalState(state: unknown): state is VisibleSignalState {
    return typeof state === 'string'
      && recoverableSignalStates.includes(state as VisibleSignalState)
  }

  async function recoverWebRtcStream() {
    const current = captureVideoWork()
    if (!current()) return

    await nextTick()
    if (!current()) return
    if (opts.webrtc.isConnected.value && !opts.webrtc.isConnecting.value) {
      await rebindWebRTCVideo()
      if (!current()) return
      videoLoading.value = false
      return
    }
    if (!opts.webrtc.isConnected.value && !opts.webrtc.isConnecting.value) {
      void connectWebRTCSerial('stream recovered').then(async (ok) => {
        if (!current()) return
        if (ok) {
          await rebindWebRTCVideo()
          if (!current()) return
          videoLoading.value = false
        } else if (webrtcRecoveryTimerId === null && webrtcRecoveryAttempts === 0) {
          scheduleWebRTCRecovery()
        }
      })
    }
  }

  const recoverStreamByTransport: Record<VideoTransport, () => void> = {
    http: refreshVideo,
    webrtc: () => { void recoverWebRtcStream() },
    webtransport: () => { videoLoading.value = false },
  }

  function enterStreaming(previous: VisibleSignalState) {
    transitionLifecycle({ type: 'signal', state: 'streaming' })
    cancelWebRTCRecovery()
    videoError.value = false
    videoErrorMessage.value = ''
    videoRestarting.value = false
    if (recoverableSignalStates.includes(previous)) recoverStreamByTransport[activeTransport.value]()
  }

  function enterNoSignal(reason: string | null) {
    streamSignalState.value = 'no_signal'
    transitionLifecycle({ type: 'signal', state: 'no_signal', reason })
    cancelWebRTCRecovery()
    videoRestarting.value = false
    videoLoading.value = false
    videoError.value = false
    videoErrorMessage.value = ''
    clearInactiveVideoSurface()
  }

  function enterDeviceBusy(previous: VisibleSignalState, reason: string | null) {
    streamSignalState.value = 'device_busy'
    transitionLifecycle({ type: 'signal', state: 'device_busy', reason })
    if (activeTransport.value === 'http') {
      clearInactiveVideoSurface()
      return
    }
    cancelWebRTCRecovery()
    videoRestarting.value = true
    videoLoading.value = true
    videoError.value = false
    videoErrorMessage.value = ''
    if (previous !== 'device_busy') void captureFrameOverlay()
  }

  function enterDeviceLost(reason: string | null) {
    streamSignalState.value = 'device_lost'
    transitionLifecycle({ type: 'signal', state: 'device_lost', reason })
    videoLoading.value = false
    clearInactiveVideoSurface()
    if (activeTransport.value !== 'http'
      && !isAudioDeviceLostStateReason(reason)
      && webrtcRecoveryTimerId === null
      && webrtcRecoveryAttempts === 0) {
      scheduleWebRTCRecovery()
    }
  }

  function enterStreamError() {
    const message = t('console.streamError')
    transitionLifecycle({ type: 'fault', message })
    videoError.value = true
    videoErrorMessage.value = message
  }

  const streamStateActions: Record<BackendStreamState, (previous: VisibleSignalState, reason: string | null) => void> = {
    streaming: previous => {
      streamSignalState.value = 'ok'
      enterStreaming(previous)
    },
    ready: previous => {
      streamSignalState.value = 'ok'
      enterStreaming(previous)
    },
    uninitialized: () => {
      streamSignalState.value = 'ok'
      transitionLifecycle({ type: 'connect_requested' })
    },
    no_signal: (_previous, reason) => enterNoSignal(reason),
    device_lost: (_previous, reason) => enterDeviceLost(reason),
    device_busy: (previous, reason) => enterDeviceBusy(previous, reason),
    error: () => enterStreamError(),
  }

  function isBackendStreamState(state: string): state is BackendStreamState {
    return Object.prototype.hasOwnProperty.call(streamStateActions, state)
  }

  function handleStreamStateChanged(data: { state: string; reason?: string | null; next_retry_ms?: number | null }) {
    const state = typeof data?.state === 'string' ? data.state : ''
    const reason = typeof data?.reason === 'string' && data.reason.length > 0 ? data.reason : null
    const nextRetry = typeof data?.next_retry_ms === 'number' && data.next_retry_ms > 0 ? data.next_retry_ms : null
    streamSignalReason.value = reason
    streamNextRetryMs.value = nextRetry
    if (!isBackendStreamState(state)) return
    streamStateActions[state](streamSignalState.value, reason)
  }

  async function handleAudioStateChanged(data: { streaming: boolean; device: string | null }) {
    const current = captureVideoWork()
    if (!current()) return

    if (!data.streaming) {
      opts.unifiedAudio.disconnect()
      return
    }
    if (!isHttpMode.value && opts.webrtc.isConnected.value) {
      if (!opts.webrtc.audioTrack.value) {
        await opts.webrtc.disconnect()
        if (!current()) return
        await new Promise(resolve => setTimeout(resolve, 300))
        if (!current()) return
        await connectWebRTCSerial('audio track refresh')
        if (!current()) return
      } else {
        const currentStream = opts.webrtcVideoRef.value?.srcObject as MediaStream | null
        if (currentStream && currentStream.getAudioTracks().length === 0) {
          currentStream.addTrack(opts.webrtc.audioTrack.value)
        }
      }
    }
    await opts.unifiedAudio.connect()
    if (!current()) return
  }

  // ──────────────────────────────────────────
  // Stream stats
  // ──────────────────────────────────────────
  function handleStreamStatsUpdate(data: { clients?: number; performance?: Record<string, number>; clients_stat?: Record<string, { fps: number }> }) {
    if (typeof data.clients === 'number') {
      systemStore.updateStreamClients(data.clients)
    }

    if (data.performance) backendPerf.value = data.performance

    if (!isHttpMode.value) {
      if (data.clients_stat && typeof data.clients_stat === 'object') {
        clientsStats.value = data.clients_stat
      }
      return
    }

    if (data.clients_stat && typeof data.clients_stat === 'object') {
      clientsStats.value = data.clients_stat
      const myStats = data.clients_stat[myClientId]
      if (myStats) {
        backendFps.value = myStats.fps || 0
      } else {
        const fpsList = Object.values(data.clients_stat)
          .map(s => s?.fps || 0)
          .filter(fps => fps > 0)
        backendFps.value = fpsList.length > 0 ? Math.min(...fpsList) : 0
      }
    } else {
      backendFps.value = 0
    }
  }

  async function refreshVideoBackendStats() {
    const current = captureVideoWork()
    if (!current()) return

    await videoConfigStore.refreshStream().catch(() => {})
    if (!current()) return
    try {
      const result = await streamApi.encoderSelfCheck()
      if (!current()) return
      selectedEncoderBackend.value = normalizeEncoderBackend(result.current_hardware_encoder)
    } catch {
      if (current()) selectedEncoderBackend.value = null
    }
  }

  function normalizeEncoderBackend(value: unknown): string | null {
    if (typeof value !== 'string') return null
    const encoder = value.trim()
    if (!encoder || encoder === 'None') return null
    if (encoder === 'nvenc' || encoder === 'nvh264enc' || encoder === 'nvh265enc' || encoder === 'nvav1enc') return 'nvenc'
    if (encoder === 'software' || encoder === 'x264enc' || encoder === 'x265enc' || encoder === 'vp8enc' || encoder === 'vp9enc' || encoder === 'av1enc') return 'software'
    return null
  }

  // ──────────────────────────────────────────
  // Initial mode restoration
  // ──────────────────────────────────────────
  function normalizeServerMode(mode: string | undefined): VideoMode | null {
    if (!mode) return null
    if (mode === 'webrtc') return 'h264'
    if (httpModes.value.includes(mode)) return mode
    if (['h264', 'h265', 'vp8', 'vp9', 'av1'].includes(mode)) return mode as VideoMode
    return null
  }

  async function restoreInitialMode(serverMode: VideoMode) {
    const current = captureVideoWork()
    if (!current()) return

    if (initialModeRestoreDone || initialModeRestoreInProgress) return
    initialModeRestoreInProgress = true
    try {
      initialDeviceInfoReceived = true
      if (serverMode !== videoMode.value) setVideoMode(serverMode)
      if (!httpModes.value.includes(serverMode)) {
        await switchToStreaming(serverMode)
        if (!current()) return
      } else if (mjpegTimestamp.value === 0) {
        refreshVideo()
      }
      initialModeRestoreDone = true
    } finally { if (current()) initialModeRestoreInProgress = false }
  }

  // ──────────────────────────────────────────
  // Device info (video parts)
  // ──────────────────────────────────────────
  interface DeviceInfoResult {
    serverMode: VideoMode | null
    isInitial: boolean
    isRestoring: boolean
    configChanging: boolean
  }

  function onDeviceInfo(data: DeviceInfoEvent): DeviceInfoResult {
    const result: DeviceInfoResult = {
      serverMode: null, isInitial: false, isRestoring: false, configChanging: false,
    }
    if (systemStore.stream?.error) {
      videoError.value = true
      videoErrorMessage.value = systemStore.stream.error
      transitionLifecycle({ type: 'fault', message: systemStore.stream.error })
    }

    if (data.video?.config_changing) { result.configChanging = true; return result }

    if (data.video?.stream_mode) {
      const serverMode = normalizeServerMode(data.video.stream_mode)
      if (!serverMode) return result
      result.serverMode = serverMode

      if (!initialDeviceInfoReceived) {
        initialDeviceInfoReceived = true
        if (!initialModeRestoreDone && !initialModeRestoreInProgress) {
          result.isInitial = true
          result.isRestoring = true
          restoreInitialMode(serverMode)
          return result
        }
      }

      if (initialModeRestoreInProgress) {
        result.isRestoring = true
        return result
      }

      if (serverMode !== videoMode.value) {
        syncToServerMode(serverMode)
      }
    }
    return result
  }

  // ──────────────────────────────────────────
  // Mode changed
  // ──────────────────────────────────────────
  function handleStreamModeChanged(data: { mode: string; previous_mode: string }) {
    const newMode = normalizeServerMode(data.mode)
    if (!newMode) return
    if (opts.videoSession.localSwitching.value) return
    if (newMode !== videoMode.value) syncToServerMode(newMode)
  }

  // ──────────────────────────────────────────
  // Video control
  // ──────────────────────────────────────────
  async function syncMjpegSignalState(generation: number) {
    const current = captureVideoWork()
    if (!current()) return

    while (
      current() && generation === mjpegRefreshGeneration
      && !mjpegFrameReceived.value
      && isHttpMode.value
      && mjpegTimestamp.value !== 0
    ) {
      const status = await streamApi.status().catch(() => null)
      if (!current()) return
      if (generation !== mjpegRefreshGeneration || mjpegFrameReceived.value || !isHttpMode.value) return
      if (status?.fault) {
        handleVideoFeatureFault(status.fault)
        return
      }
      if (status?.signal) {
        handleStreamStateChanged(status.signal)
        if (isRecoverableSignalState(status.signal.state)) {
          return
        }
        if (status.signal.state === 'streaming') return
      }
      await new Promise(resolve => setTimeout(resolve, 250))
      if (!current()) return
    }
  }

  function refreshVideo() {
    const current = captureVideoWork()
    if (!current()) return
    const generation = ++mjpegRefreshGeneration
    stopMjpegStream()
    transitionLifecycle({ type: 'connect_requested' })
    if (mjpegRefreshTimeoutId !== null) {
      clearTimeout(mjpegRefreshTimeoutId)
      mjpegRefreshTimeoutId = null
    }
    backendFps.value = 0
    videoError.value = false
    videoErrorMessage.value = ''
    mjpegFrameReceived.value = false
    streamSignalState.value = 'ok'
    streamSignalReason.value = null
    streamNextRetryMs.value = null
    isRefreshingVideo = true
    videoLoading.value = true
    mjpegTimestamp.value = Date.now()
    transitionLifecycle({ type: 'source_ready' })
    void startMjpegStream(generation)
    void syncMjpegSignalState(generation)
    if (mjpegFirstFrameDeadlineId !== null) clearTimeout(mjpegFirstFrameDeadlineId)
    mjpegFirstFrameDeadlineId = window.setTimeout(async () => {
      if (!current()) return
      mjpegFirstFrameDeadlineId = null
      if (generation !== mjpegRefreshGeneration || mjpegFrameReceived.value || !isHttpMode.value) return
      const status = await streamApi.status().catch(() => null)
      if (!current() || generation !== mjpegRefreshGeneration) return
      if (status?.fault) {
        handleVideoFeatureFault(status.fault)
        return
      }
      if (status?.signal) {
        handleStreamStateChanged(status.signal)
        if (isRecoverableSignalState(status.signal.state)) {
          return
        }
      }
      stopMjpegStream()
      mjpegTimestamp.value = 0
      videoLoading.value = false
      videoError.value = true
      videoErrorMessage.value = t('console.streamError')
      transitionLifecycle({ type: 'fault', message: videoErrorMessage.value })
      systemStore.setStreamOnline(false)
    }, MJPEG_FIRST_FRAME_DEADLINE_MS)
    mjpegRefreshTimeoutId = window.setTimeout(() => {
      if (!current() || generation !== mjpegRefreshGeneration) return
      mjpegRefreshTimeoutId = null
      isRefreshingVideo = false
      if (!mjpegFrameReceived.value && isHttpMode.value) {
        systemStore.setStreamOnline(false)
      }
    }, 1500)
  }

  function reloadPage() { window.location.reload() }

  async function resetVideoStream() {
    const current = captureVideoWork()
    if (!current()) return

    if (videoRestarting.value) return
    videoRestarting.value = true
    videoLoading.value = true
    transitionLifecycle({ type: 'switch_requested' })
    videoError.value = false
    videoErrorMessage.value = ''
    cancelWebRTCRecovery()
    if (webrtcReconnectTimeout !== null) { clearTimeout(webrtcReconnectTimeout); webrtcReconnectTimeout = null }
    try {
      await captureFrameOverlay()
      if (!current()) return
      await streamApi.reset().catch(() => null)
      if (!current()) return
      await opts.webrtc.disconnect().catch(() => {})
      if (!current()) return
      await opts.webtransport.disconnect().catch(() => {})
      if (!current()) return
      if (opts.webrtcVideoRef.value) opts.webrtcVideoRef.value.srcObject = null
      stopMjpegStream()
      mjpegFrameReceived.value = false
      backendFps.value = 0
      if (isHttpMode.value) {
        refreshVideo()
      } else if (isWebTransportMode.value) {
        await connectWebTransportOnly(videoMode.value)
      } else {
        await connectWebRTCOnly(videoMode.value)
      }
    } finally {
      if (current()) videoRestarting.value = false
    }
  }

  const httpStreamUrl = computed(() => {
    if (!captureVideoWork()()) return ''
    if (!httpModes.value.includes(videoMode.value)) return ''
    if (mjpegTimestamp.value === 0) return ''
    if (videoError.value) return ''
    if (streamSignalState.value !== 'ok') return ''
    return `${streamApi.getHttpStreamUrl(myClientId)}&t=${mjpegTimestamp.value}`
  })

  async function connectWebRTCOnly(codec: VideoMode = 'h264') {
    const current = captureVideoWork()
    if (!current()) return

    if (retryTimeoutId !== null) { clearTimeout(retryTimeoutId); retryTimeoutId = null }
    if (gracePeriodTimeoutId !== null) { clearTimeout(gracePeriodTimeoutId); gracePeriodTimeoutId = null }
    retryCount = 0; consecutiveErrors = 0
    stopMjpegStream()
    mjpegTimestamp.value = 0
    setActiveTransport('webrtc')
    await opts.webtransport.disconnect().catch(() => {})
    if (!current()) return
    videoLoading.value = true; videoError.value = false; videoErrorMessage.value = ''
    transitionLifecycle({ type: 'connect_requested' })
    try {
      const success = await connectWebRTCSerial('connectWebRTCOnly', codec)
      if (!current()) return
      if (success) {
        await rebindWebRTCVideo()
        if (!current()) return
        videoLoading.value = false
        setVideoMode(codec)
        opts.unifiedAudio.switchMode('webrtc')
      } else { throw new Error(currentWebRTCError('WebRTC connection failed')) }
    } catch (e) {
      if (!current()) return

      markWebRTCFailure(t('console.webrtcFailed'), errorMessage(e, currentWebRTCError('WebRTC connection failed')))
    }
  }

  async function shouldUseWebTransport(codec: VideoMode): Promise<boolean> {
    const current = captureVideoWork()
    if (!current()) return false

    if (transportPreference.value !== 'webtransport') return false
    const capability = webTransportCapabilities.value ?? await opts.webtransport.refreshCapabilities().catch(() => null)
    if (!current()) return false
    if (!capability?.enabled || !capability.url) return false
    if (!capability.codecs.some(c => c.id === codec)) return false
    const supported = opts.webtransport.browserSupported.value && await opts.webtransport.isCodecSupported(codec)
    if (!current()) return false
    return supported
  }

  async function connectWebTransportOnly(codec: VideoMode = 'h264', options: { fallbackToWebRTC?: boolean } = {}) {
    const current = captureVideoWork()
    if (!current()) return

    if (retryTimeoutId !== null) { clearTimeout(retryTimeoutId); retryTimeoutId = null }
    if (gracePeriodTimeoutId !== null) { clearTimeout(gracePeriodTimeoutId); gracePeriodTimeoutId = null }
    retryCount = 0; consecutiveErrors = 0
    stopMjpegStream()
    mjpegTimestamp.value = 0
    setActiveTransport('webtransport')
    if (opts.webrtc.isConnected.value || opts.webrtc.isConnecting.value) await opts.webrtc.disconnect()
    if (!current()) return
    if (opts.webrtcVideoRef.value) opts.webrtcVideoRef.value.srcObject = null
    videoLoading.value = true; videoError.value = false; videoErrorMessage.value = ''
    transitionLifecycle({ type: 'connect_requested' })
    await nextTick()
    if (!current()) return
    opts.webtransport.setCanvas(opts.webTransportCanvasRef.value)
    try {
      await opts.webtransport.connect(codec, selectedEncoderBackend.value)
      if (!current()) return
      setVideoMode(codec)
      opts.unifiedAudio.switchMode('ws')
    } catch (webTransportError) {
      if (!current()) return

      if (isSupersededWebTransportConnect(webTransportError)) return
      const wt = errorMessage(webTransportError, 'WebTransport connection failed')
      videoLoading.value = false
      videoError.value = true
      videoErrorMessage.value = wt
      transitionLifecycle({ type: 'fault', message: wt })
      systemStore.setStreamOnline(false)
      if (!options.fallbackToWebRTC) {
        throw webTransportError
      }
      await connectWebRTCOnly(codec).catch((webrtcError) => {
        const wrtc = errorMessage(webrtcError, currentWebRTCError('WebRTC connection failed'))
        throw new Error(`WebTransport: ${wt}; WebRTC fallback: ${wrtc}`)
      })
      if (!current()) return
    }
  }

  async function rebindWebRTCVideo(): Promise<boolean> {
    const current = captureVideoWork()
    if (!current()) return false
    if (!opts.webrtcVideoRef.value) return false
    opts.webrtcVideoRef.value.srcObject = null
    await nextTick()
    const video = opts.webrtcVideoRef.value
    if (!current() || !video || !opts.webrtc.videoTrack.value) return false
    const stream = opts.webrtc.getMediaStream()
    if (!stream) return false
    video.srcObject = stream
    try { await video.play() } catch { /* autoplay can require a user gesture */ }
    if (!current() || opts.webrtcVideoRef.value !== video) return false
    const frameReceived = await waitForVideoFirstFrame(video, 2000)
    if (!current() || opts.webrtcVideoRef.value !== video) return false
    if (frameReceived) {
      transitionLifecycle({ type: 'frame_received' })
      clearFrameOverlay()
    }
    return frameReceived
  }

  async function switchToWebRTC(codec: VideoMode = 'h264') {
    const current = captureVideoWork()
    if (!current()) return

    if (retryTimeoutId !== null) { clearTimeout(retryTimeoutId); retryTimeoutId = null }
    if (gracePeriodTimeoutId !== null) { clearTimeout(gracePeriodTimeoutId); gracePeriodTimeoutId = null }
    retryCount = 0; consecutiveErrors = 0
    stopMjpegStream()
    mjpegTimestamp.value = 0
    setActiveTransport('webrtc')
    await opts.webtransport.disconnect().catch(() => {})
    if (!current()) return
    videoLoading.value = true; videoError.value = false; videoErrorMessage.value = ''
    transitionLifecycle({ type: 'switch_requested' })
    pendingWebRTCReadyGate = false

    try {
      if (opts.webrtc.isConnected.value && videoMode.value === codec) {
        await rebindWebRTCVideo()
        if (!current()) return
        videoLoading.value = false
        opts.unifiedAudio.switchMode('webrtc')
        return
      }
      const MAX_ATTEMPTS = 2
      const RETRY_DELAYS = [250]
      let success = false
      for (let attempt = 0; attempt < MAX_ATTEMPTS && !success; attempt++) {
        if (attempt > 0) {
          await new Promise(resolve => setTimeout(resolve, RETRY_DELAYS[attempt - 1] ?? 250))
          if (!current()) return
        }
        success = await connectWebRTCSerial('switchToWebRTC', codec)
        if (!current()) return
      }
      if (success) {
        await rebindWebRTCVideo()
        if (!current()) return
        videoLoading.value = false
        opts.unifiedAudio.switchMode('webrtc')
      } else { throw new Error(currentWebRTCError('WebRTC connection failed')) }
    } catch (e) {
      if (!current()) return

      markWebRTCFailure(t('console.webrtcFailed'), errorMessage(e, currentWebRTCError('WebRTC connection failed')))
    }
  }

  async function switchToStreaming(codec: VideoMode = 'h264') {
    const current = captureVideoWork()
    if (!current()) return

    const useWebTransport = await shouldUseWebTransport(codec)
    if (!current()) return
    if (useWebTransport) {
      await connectWebTransportOnly(codec, { fallbackToWebRTC: true })
      return
    }
    await switchToWebRTC(codec)
  }

  async function switchToHttp(mode: VideoMode = 'mjpeg') {
    const current = captureVideoWork()
    if (!current()) return

    videoLoading.value = true; videoError.value = false; videoErrorMessage.value = ''
    pendingWebRTCReadyGate = false
    try {
      const modeResp = await streamApi.setMode(mode)
      if (!current()) return
      if (modeResp.transition_id) {
        opts.videoSession.registerTransition(modeResp.transition_id)
        await opts.videoSession.waitForModeReady(modeResp.transition_id, 5000)
        if (!current()) return
      }
    } catch (e) {
      if (!current()) return
      console.error('Failed to switch to HTTP mode:', e)
    }
    if (opts.webrtc.isConnected.value || opts.webrtc.isConnecting.value) {
      await opts.webrtc.disconnect()
      if (!current()) return
    }
    await opts.webtransport.disconnect().catch(() => {})
    if (!current()) return
    setActiveTransport('http')
    if (opts.webrtcVideoRef.value) opts.webrtcVideoRef.value.srcObject = null
    opts.unifiedAudio.switchMode('ws')
    refreshVideo()
  }

  function syncToServerMode(mode: VideoMode) {
    if (!captureVideoWork()()) return
    if (opts.videoSession.localSwitching.value || opts.videoSession.backendSwitching.value) return
    if (mode === videoMode.value) return
    setVideoMode(mode)
    if (!httpModes.value.includes(mode)) switchToStreaming(mode)
    else refreshVideo()
  }

  async function handleVideoModeChange(mode: VideoMode) {
    const current = captureVideoWork()
    if (!current()) return

    if (mode === videoMode.value) return
    if (!opts.videoSession.tryStartLocalSwitch()) return
    try {
      await captureFrameOverlay()
      if (!current()) return
      if (!httpModes.value.includes(mode)) {
        stopMjpegStream()
        mjpegTimestamp.value = 0
        await new Promise(resolve => setTimeout(resolve, 50))
        if (!current()) return
      }
      setVideoMode(mode)
      if (!httpModes.value.includes(mode)) await switchToStreaming(mode)
      else await switchToHttp(mode)
    } finally { if (current()) opts.videoSession.endLocalSwitch() }
  }

  const isModeSwitching = opts.videoSession.localSwitching

  // ──────────────────────────────────────────
  // Watchers
  // ──────────────────────────────────────────
  function setupWatchers() {
    if (stopHandles.length > 0) return

    // A multipart MJPEG response can change intrinsic dimensions without
    // dispatching another element `load` event. Sampling the DOM property is
    // cheap and gives MJPEG the same live resolution semantics as video.resize
    // and WebCodecs statistics.
    if (intrinsicSizeTimerId === null) {
      intrinsicSizeTimerId = window.setInterval(syncVideoSourceSize, 250)
    }

    stopHandles.push(watch(() => opts.webrtc.videoTrack.value, async (track) => {
      const current = captureVideoWork()
      if (!current()) return
      if (track && opts.webrtcVideoRef.value && !isHttpMode.value) {
        await rebindWebRTCVideo()
      }
    }))

    stopHandles.push(watch(() => opts.webrtc.audioTrack.value, async (track) => {
      if (!captureVideoWork()()) return
      if (track && opts.webrtcVideoRef.value && !isHttpMode.value) {
        const currentStream = opts.webrtcVideoRef.value.srcObject as MediaStream | null
        if (currentStream && currentStream.getAudioTracks().length === 0) {
          currentStream.addTrack(track)
        }
      }
    }))

    stopHandles.push(watch(opts.webrtc.stats, (stats) => {
      if (!captureVideoWork()()) return
      const freshFrame = stats.framesDecoded > lastWebRtcDecodedFrames
      lastWebRtcDecodedFrames = stats.framesDecoded
      if (isHttpMode.value || isWebTransportMode.value) return
      if (stats.frameWidth && stats.frameHeight) {
        updateVideoSourceSize(stats.frameWidth, stats.frameHeight)
      }
      if (!freshFrame) return
      transitionLifecycle({ type: 'frame_received' })
      backendFps.value = Math.round(stats.framesPerSecond || backendFps.value || 0)
      systemStore.setStreamOnline(true)
      videoLoading.value = false
      videoError.value = false
      videoErrorMessage.value = ''
    }, { deep: true }))

    stopHandles.push(watch(opts.webtransport.stats, (stats) => {
      if (!captureVideoWork()()) return
      const freshFrame = stats.framesDecoded > lastWebTransportDecodedFrames
      lastWebTransportDecodedFrames = stats.framesDecoded
      if (!isWebTransportMode.value) return
      if (stats.frameWidth && stats.frameHeight) {
        updateVideoSourceSize(stats.frameWidth, stats.frameHeight)
      }
      if (!freshFrame) return
      transitionLifecycle({ type: 'frame_received' })
      systemStore.setStreamOnline(true)
      videoLoading.value = false
      videoError.value = false
      videoErrorMessage.value = ''
      clearFrameOverlay()
    }, { deep: true }))

    stopHandles.push(watch(opts.webrtcVideoRef, (el) => {
      if (!captureVideoWork()()) return
      opts.unifiedAudio.setWebRTCElement(el)
      if (el) nextTick(syncVideoSourceSize)
    }, { immediate: true }))

    stopHandles.push(watch(opts.webTransportCanvasRef, (el) => {
      if (!captureVideoWork()()) return
      opts.webtransport.setCanvas(el)
      if (el) nextTick(syncVideoSourceSize)
    }, { immediate: true }))

    stopHandles.push(watch(() => opts.webtransport.state.value, (newState) => {
      if (!captureVideoWork()()) return
      if (!isWebTransportMode.value) return
      if (newState === 'connected') {
        videoError.value = false
        videoErrorMessage.value = ''
      } else if (newState === 'failed') {
        videoLoading.value = false
        videoError.value = true
        videoErrorMessage.value = opts.webtransport.error.value || 'WebTransport connection failed'
        transitionLifecycle({ type: 'fault', message: videoErrorMessage.value })
        systemStore.setStreamOnline(false)
      }
    }))

    stopHandles.push(watch(() => opts.webrtc.state.value, (newState, oldState) => {
      if (!captureVideoWork()()) return
      const reconnectTimeoutMs = 2000
      if (newState === 'connected' && oldState !== 'connected') {
        webrtcReconnectFailures = 0
        if (!isHttpMode.value) {
          nextTick(() => { rebindWebRTCVideo() })
        }
      } else if (newState === 'disconnected' && oldState !== 'disconnected') {
        if (shouldSuppressAutoReconnect()) return
        webrtcReconnectFailures++
        if (webrtcReconnectFailures >= 2) {
          markWebRTCFailure(t('console.webrtcFailed'))
          return
        }
        const current = captureVideoWork()
        webrtcReconnectTimeout = setTimeout(async () => {
          if (!current()) return
          webrtcReconnectTimeout = null
          if (shouldSuppressAutoReconnect()) return
          const ok = await connectWebRTCSerial('state-watcher-reconnect')
          if (!ok && webrtcReconnectFailures < 2) {
            webrtcReconnectFailures++
          }
        }, reconnectTimeoutMs)
      } else if (newState === 'failed') {
        if (shouldSuppressAutoReconnect()) return
        const video = opts.webrtcVideoRef.value
        const hasRenderedVideo = !!video && video.videoWidth > 0 && video.videoHeight > 0 && video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA
        const hasDecodedFrames = opts.webrtc.stats.value.framesDecoded > 0 || opts.webrtc.stats.value.bytesReceived > 0
        if (hasRenderedVideo || hasDecodedFrames) {
          videoDebugLog('Ignoring WebRTC failed state watcher because video is active', {
            readyState: video?.readyState,
            videoWidth: video?.videoWidth,
            videoHeight: video?.videoHeight,
            framesDecoded: opts.webrtc.stats.value.framesDecoded,
            bytesReceived: opts.webrtc.stats.value.bytesReceived,
          })
          videoError.value = false
          videoErrorMessage.value = ''
          videoLoading.value = false
          systemStore.setStreamOnline(true)
          return
        }
        webrtcReconnectFailures++
        if (webrtcReconnectFailures > 3) {
          console.error('[WebRTC] Max reconnect failures exceeded')
          markWebRTCFailure(t('console.webrtcFailed'))
          return
        }
        const retryDelay = Math.min(1000 * Math.pow(2, webrtcReconnectFailures - 1), 15000)
        const current = captureVideoWork()
        webrtcReconnectTimeout = setTimeout(async () => {
          if (!current()) return
          webrtcReconnectTimeout = null
          if (shouldSuppressAutoReconnect()) return
          await opts.webrtc.disconnect()
          if (!current()) return
          const ok = await connectWebRTCSerial('state-watcher-failed')
          if (!current()) return
          if (!ok) { webrtcReconnectFailures++ }
        }, retryDelay)
      }
    }))
  }

  function suspendVideoWork() {
    videoWorkScope.reset()
    webrtcConnectTask = null
    pendingWebRTCReadyGate = false
    initialModeRestoreInProgress = false
    isRefreshingVideo = false
    opts.videoSession.reset()
    if (retryTimeoutId !== null) { clearTimeout(retryTimeoutId); retryTimeoutId = null }
    if (gracePeriodTimeoutId !== null) { clearTimeout(gracePeriodTimeoutId); gracePeriodTimeoutId = null }
    if (webrtcRecoveryTimerId !== null) { clearTimeout(webrtcRecoveryTimerId); webrtcRecoveryTimerId = null }
    if (webrtcReconnectTimeout !== null) { clearTimeout(webrtcReconnectTimeout); webrtcReconnectTimeout = null }
    if (mjpegRefreshTimeoutId !== null) { clearTimeout(mjpegRefreshTimeoutId); mjpegRefreshTimeoutId = null }
    if (mjpegFirstFrameDeadlineId !== null) { clearTimeout(mjpegFirstFrameDeadlineId); mjpegFirstFrameDeadlineId = null }
    stopMjpegStream()
  }

  function cleanup() {
    videoDisposed = true
    stopVideoActivationWatch()
    suspendVideoWork()
    if (intrinsicSizeTimerId !== null) { clearInterval(intrinsicSizeTimerId); intrinsicSizeTimerId = null }
    webrtcRecoveryAttempts = 0
    opts.webtransport.disconnect().catch(() => {})
    transitionLifecycle({ type: 'stopped' })
    retryCount = 0
    consecutiveErrors = 0
    initialDeviceInfoReceived = false
    initialModeRestoreDone = false
    initialModeRestoreInProgress = false
    videoSourceSize.value = null
    while (stopHandles.length) {
      stopHandles.pop()?.()
    }
  }

  return {
    // State
    videoMode,
    httpModes,
    activeTransport,
    isHttpMode,
    isWebTransportMode,
    backendFps,
    mjpegFrameReceived,
    mjpegTimestamp,
    videoLoading,
    videoError,
    videoErrorMessage,
    videoRestarting,
    backendPerf,
    selectedEncoderBackend,
    clientsStats,
    frameOverlayUrl,
    mjpegFrameUrl,
    httpStreamUrl,
    streamSignalState,
    streamSignalReason,
    streamNextRetryMs,
    lifecycle,
    lifecyclePresentation: computed(() => presentVideoLifecycle(lifecycle.value)),
    signalOverlayInfo,
    loadingMessage,
    connectProgress,
    videoSourceSize,
    myClientId,
    isModeSwitching,
    // Video control
    refreshVideo,
    reloadPage,
    resetVideoStream,
    connectWebRTCOnly,
    connectWebTransportOnly,
    rebindWebRTCVideo,
    switchToWebRTC,
    switchToStreaming,
    switchToHttp,
    syncToServerMode,
    handleVideoModeChange,
    refreshVideoBackendStats,
    restoreInitialMode,
    normalizeServerMode,
    // Event handlers
    handleVideoLoad,
    syncVideoSourceSize,
    handleVideoError,
    handleVideoFeatureFault,
    handleStreamDeviceLost,
    handleStreamRecovered,
    handleStreamConfigChanging,
    handleStreamConfigApplied,
    handleWebRTCReady,
    handleStreamModeReady,
    handleStreamModeSwitching,
    handleStreamStateChanged,
    handleStreamStatsUpdate,
    handleAudioStateChanged,
    onDeviceInfo,
    handleStreamModeChanged,
    captureFrameOverlay,
    clearFrameOverlay,
    // Setup
    setupWatchers,
    cleanup,
  }
}
