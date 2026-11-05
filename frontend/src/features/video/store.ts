import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { streamApi, type VideoTransport, type WebTransportCapabilitiesResponse } from '@/api'

export type VideoMode = string

function storedTransportPreference(): Exclude<VideoTransport, 'http'> {
  return localStorage.getItem('videoTransportPreference') === 'webtransport'
    ? 'webtransport'
    : 'webrtc'
}

export const useVideoStore = defineStore('feature-video', () => {
  const videoMode = ref<VideoMode>(
    (localStorage.getItem('videoMode') as VideoMode) || 'mjpeg'
  )
  const httpModes = ref<string[]>(['mjpeg'])
  const transportPreference = ref<Exclude<VideoTransport, 'http'>>(storedTransportPreference())
  const activeTransport = ref<VideoTransport>('http')
  const webTransportCapabilities = ref<WebTransportCapabilitiesResponse | null>(null)
  const backendFps = ref(0)
  const mjpegFrameReceived = ref(false)
  const mjpegTimestamp = ref(0)
  const videoLoading = ref(true)
  const videoError = ref(false)
  const videoErrorMessage = ref('')
  const videoRestarting = ref(false)
  const backendPerf = ref<Record<string, number> | null>(null)
  const selectedEncoderBackend = ref<string | null>(null)
  const clientsStats = ref<Record<string, { id?: string; fps: number; connected_secs?: number }>>({})

  const isHttpMode = computed(() => httpModes.value.includes(videoMode.value))
  const isWebTransportMode = computed(() => !isHttpMode.value && activeTransport.value === 'webtransport')

  function setVideoMode(mode: VideoMode) {
    videoMode.value = mode
    localStorage.setItem('videoMode', mode)
  }

  function setTransportPreference(transport: Exclude<VideoTransport, 'http'>) {
    transportPreference.value = transport
    localStorage.setItem('videoTransportPreference', transport)
  }

  function setActiveTransport(transport: VideoTransport) {
    activeTransport.value = transport
  }

  async function fetchConstraints() {
    try {
      const resp = await streamApi.getConstraints()
      httpModes.value = resp.http_modes ?? ['mjpeg']
      webTransportCapabilities.value = await streamApi.getWebTransportCapabilities().catch((e) => {
        const message = e instanceof Error && e.message ? e.message : String(e)
        return {
          success: true,
          transport: 'webtransport',
          decoder: 'webcodecs',
          enabled: false,
          configured: false,
          url: null,
          requires_secure_context: true,
          codecs: [],
          encoders: [],
          reason: `capabilities_request_failed: ${message}`,
        } satisfies WebTransportCapabilitiesResponse
      })
    } catch {
      httpModes.value = ['mjpeg']
    }
  }

  function refreshMjpeg() {
    backendFps.value = 0
    videoError.value = false
    videoErrorMessage.value = ''
    mjpegFrameReceived.value = false
    videoLoading.value = true
    mjpegTimestamp.value = Date.now()
  }

  function clearVideoError() {
    videoError.value = false
    videoErrorMessage.value = ''
  }

  function reset() {
    videoMode.value = (localStorage.getItem('videoMode') as VideoMode) || 'mjpeg'
    httpModes.value = ['mjpeg']
    transportPreference.value = storedTransportPreference()
    activeTransport.value = 'http'
    webTransportCapabilities.value = null
    backendFps.value = 0
    mjpegFrameReceived.value = false
    mjpegTimestamp.value = 0
    videoLoading.value = true
    videoError.value = false
    videoErrorMessage.value = ''
    videoRestarting.value = false
    backendPerf.value = null
    selectedEncoderBackend.value = null
    clientsStats.value = {}
  }

  return {
    videoMode,
    httpModes,
    transportPreference,
    activeTransport,
    webTransportCapabilities,
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
    isHttpMode,
    isWebTransportMode,
    setVideoMode,
    setTransportPreference,
    setActiveTransport,
    fetchConstraints,
    refreshMjpeg,
    clearVideoError,
    reset,
  }
})
