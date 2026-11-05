<script setup lang="ts">
import { ref, computed, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { toast } from 'vue-sonner'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Separator } from '@/components/ui/separator'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { Monitor, Settings, Zap, Scale, Image, Loader2 } from 'lucide-vue-next'
import HelpTooltip from '@/components/HelpTooltip.vue'
import {
  configApi,
  streamApi,
  type VideoCodecInfo,
  type EncoderBackendInfo,
  type BitratePreset,
  type StreamConstraintsResponse,
  type VideoColorDepthCapabilitiesResponse,
  type VideoColorDepthProfile,
} from '@/api'
import { formatFpsLabel, toConfigFps } from '@/lib/fps'
import { useVideoConfigStore } from '@/stores/videoConfig'
import { useVideoStore } from './store'
import {
  availableColorPreferences,
  colorDepthProfilesForPreference,
  profileForPreferenceChange,
  resolveColorPreference,
  type ColorPerformancePreference,
} from './colorDepthControls'
import { useRoute, useRouter } from 'vue-router'

export type VideoMode = string

interface VideoDevice {
  path: string
  formats: Array<{
    format: string
    description: string
    resolutions: Array<{ width: number; height: number; fps: number[] }>
  }>
}

const props = defineProps<{
  open: boolean
  videoMode: VideoMode
}>()

const emit = defineEmits<{
  (e: 'update:open', value: boolean): void
  (e: 'update:videoMode', value: VideoMode): void
  (e: 'update:transport', value: 'webrtc' | 'webtransport'): void
}>()

const { t } = useI18n()
const videoConfigStore = useVideoConfigStore()
const videoStore = useVideoStore()
const router = useRouter()
const route = useRoute()

const devices = ref<VideoDevice[]>([])
const loadingDevices = ref(false)
const selectedFormat = ref('')
const selectedResolution = ref('')
const selectedFps = ref(30)
const applyingCapture = ref(false)

const codecs = ref<VideoCodecInfo[]>([])
const loadingCodecs = ref(false)
const loadingWebTransportCapabilities = ref(false)
const webCodecsSupported = ref<Set<string>>(new Set())

// Backend list
const backends = ref<EncoderBackendInfo[]>([])
const constraints = ref<StreamConstraintsResponse | null>(null)
const colorDepthProfiles = ref<VideoColorDepthProfile[]>([])
const selectedColorDepthProfile = ref<string>('')
const colorPerformancePreference = ref<ColorPerformancePreference>('video')
const loadingColorDepthProfiles = ref(false)
const applyingColorDepth = ref(false)
const colorPerformancePreferences = computed(() =>
  availableColorPreferences(colorDepthProfiles.value),
)
const colorDepthOptions = computed(() =>
  colorDepthProfilesForPreference(colorDepthProfiles.value, colorPerformancePreference.value),
)
const currentEncoderBackend = computed(() => videoConfigStore.stream?.encoder || 'auto')
const isRtspEnabled = computed(() => constraints.value?.sources?.rtsp === true)
const isRustdeskEnabled = computed(() => constraints.value?.sources?.rustdesk === true)
const isRtspCodecLocked = computed(() => isRtspEnabled.value)
const isRustdeskWebrtcLocked = computed(() => !isRtspEnabled.value && isRustdeskEnabled.value)
const codecLockSources = computed(() => {
  if (isRtspCodecLocked.value) {
    return isRustdeskEnabled.value ? 'RTSP/RustDesk' : 'RTSP'
  }
  if (isRustdeskWebrtcLocked.value) return 'RustDesk'
  return ''
})
const codecLockMessage = computed(() => {
  if (!codecLockSources.value) return ''
  return t('actionbar.multiSourceCodecLocked', { sources: codecLockSources.value })
})
const isCodecLocked = computed(() => !!codecLockMessage.value)

const isCodecOptionDisabled = (codecId: string): boolean => {
  if (!isBrowserSupported(codecId)) return true
  if (isRustdeskWebrtcLocked.value) {
    const codec = codecs.value.find(c => c.id === codecId)
    if (codec?.protocol === 'http') return true
  }
  return false
}

const activeTransport = computed(() => videoStore.activeTransport)
const webTransportEnabled = computed(() => {
  const caps = videoStore.webTransportCapabilities
  return !!caps?.enabled && !!caps.url
})
const webTransportBrowserSupport = computed(() => {
  if (typeof window === 'undefined') {
    return { supported: false, reason: 'WebTransport is only available in a browser' }
  }
  if (!window.isSecureContext) {
    return { supported: false, reason: 'WebTransport requires a secure context' }
  }
  if (!('WebTransport' in window)) {
    return { supported: false, reason: 'This browser does not support WebTransport' }
  }
  if (!('VideoDecoder' in window)) {
    return { supported: false, reason: 'This browser does not support WebCodecs' }
  }
  return { supported: true, reason: '' }
})
const isCurrentModeStreaming = computed(() => currentProtocol.value !== 'http')
const isCurrentModeWebTransportCodec = computed(() => {
  const caps = videoStore.webTransportCapabilities
  if (caps?.codecs?.length) return caps.codecs.some(codec => codec.id === props.videoMode)
  return ['h264', 'h265', 'vp8', 'vp9', 'av1'].includes(props.videoMode)
})
const canUseWebTransportForCurrentMode = computed(() => {
  if (!isCurrentModeStreaming.value) return false
  if (!isCurrentModeWebTransportCodec.value) return false
  if (!webTransportBrowserSupport.value.supported) return false
  if (!webTransportEnabled.value) return false
  return webCodecsSupported.value.has(props.videoMode)
})
function formatWebTransportBackendReason(reason?: string | null): string {
  switch (reason) {
    case 'webtransport_not_enabled':
    case 'webtransport_not_enabled_in_config':
      return 'WebTransport is disabled in server.webtransport.enabled'
    case 'webtransport_server_not_started':
      return 'WebTransport server has not started yet'
    case 'webtransport_runtime_disabled':
      return 'WebTransport runtime is disabled'
    case 'webtransport_url_unavailable':
      return 'WebTransport server did not publish a connection URL'
    case 'webtransport_not_started':
      return 'WebTransport server is not running'
    default:
      return reason || 'Backend did not provide a reason'
  }
}
const webTransportDisabledReason = computed(() => {
  if (!isCurrentModeStreaming.value) return 'WebTransport is only available for streaming codecs'
  if (!isCurrentModeWebTransportCodec.value) return 'This codec is not available for WebTransport'
  if (!webTransportBrowserSupport.value.supported) return webTransportBrowserSupport.value.reason
  if (!webTransportEnabled.value) {
    const caps = videoStore.webTransportCapabilities
    if (!caps) {
      return loadingWebTransportCapabilities.value
        ? 'Loading backend WebTransport capabilities...'
        : 'Backend WebTransport capabilities request has not completed'
    }
    const detail = formatWebTransportBackendReason(caps.reason)
    const state = `configured=${caps.configured}, enabled=${caps.enabled}, url=${caps.url ? 'present' : 'missing'}`
    return `${detail} (${state})`
  }
  if (!webCodecsSupported.value.has(props.videoMode)) {
    return `This browser cannot decode ${props.videoMode.toUpperCase()} with WebCodecs`
  }
  return ''
})

// Browser supported codecs (WebRTC receive capabilities)
const browserSupportedCodecs = ref<Set<string>>(new Set())

// Check browser WebRTC codec support
function detectBrowserCodecSupport() {
  const supported = new Set<string>()

  // HTTP protocol codecs are always supported (HTTP streaming, no WebRTC)
  for (const c of codecs.value) {
    if (c.protocol === 'http') {
      supported.add(c.id)
    }
  }

  // Check WebRTC receive capabilities
  if (typeof RTCRtpReceiver !== 'undefined' && RTCRtpReceiver.getCapabilities) {
    const capabilities = RTCRtpReceiver.getCapabilities('video')
    if (capabilities?.codecs) {
      for (const codec of capabilities.codecs) {
        const mimeType = codec.mimeType.toLowerCase()
        if (mimeType.includes('h264') || mimeType.includes('avc')) {
          supported.add('h264')
        }
        if (mimeType.includes('h265') || mimeType.includes('hevc')) {
          supported.add('h265')
        }
        if (mimeType.includes('vp8')) {
          supported.add('vp8')
        }
        if (mimeType.includes('vp9')) {
          supported.add('vp9')
        }
        if (mimeType.includes('av1')) {
          supported.add('av1')
        }
      }
    }
  } else {
    supported.add('h264')
    supported.add('vp8')
    supported.add('vp9')
  }

  browserSupportedCodecs.value = supported
  detectWebCodecsSupport()
  console.info('[VideoConfig] Browser supported codecs:', Array.from(supported))
}

async function detectWebCodecsSupport() {
  const supported = new Set<string>()
  const codecStrings: Record<string, string> = {
    h264: 'avc1.42E01F',
    h265: 'hev1.1.6.L93.B0',
    vp8: 'vp8',
    vp9: 'vp09.00.10.08',
    av1: 'av01.0.05M.08',
  }
  const videoDecoder = (window as Window & { VideoDecoder?: { isConfigSupported?: (config: { codec: string }) => Promise<{ supported: boolean }> } }).VideoDecoder
  if (!videoDecoder?.isConfigSupported || !window.isSecureContext) {
    webCodecsSupported.value = supported
    return
  }
  const isConfigSupported = videoDecoder.isConfigSupported
  if (!isConfigSupported) return
  await Promise.all(Object.entries(codecStrings).map(async ([id, codec]) => {
    const result = await isConfigSupported({ codec }).catch(() => ({ supported: false }))
    if (result.supported) supported.add(id)
  }))
  webCodecsSupported.value = supported
}

// Check if a codec is supported by browser
const isBrowserSupported = (codecId: string): boolean => {
  const codec = codecs.value.find(c => c.id === codecId)
  if (codec?.protocol === 'http') return true
  return browserSupportedCodecs.value.has(codecId)
}

const currentProtocol = computed<'http' | 'webrtc' | 'webtransport' | null>(() => {
  const codec = codecs.value.find(c => c.id === props.videoMode)
  return codec?.protocol ?? null
})

// Translate backend name for display
const translateBackendName = (backend: string | undefined): string => {
  if (!backend) return ''
  // Translate known backend names
  const lowerBackend = backend.toLowerCase()
  if (lowerBackend === 'software') {
    return t('actionbar.backendSoftware')
  }
  if (lowerBackend === 'nvenc') {
    return 'NVENC'
  }
  if (lowerBackend === 'auto') {
    return t('actionbar.backendAuto')
  }
  // Hardware backends (VAAPI, V4L2 M2M, etc.) keep original names
  return backend
}

const selectedBitratePreset = ref<'Speed' | 'Balanced' | 'Quality'>('Balanced')
const applyingBitrate = ref(false)

const buttonText = computed(() => t('actionbar.videoConfig'))
const configuredDevicePath = computed(() => videoConfigStore.video?.device || '')
const configuredDevice = computed(() =>
  devices.value.find(device => device.path === configuredDevicePath.value),
)
const availableFormats = computed(() => configuredDevice.value?.formats ?? [])
const availableResolutions = computed(() =>
  availableFormats.value.find(format => format.format === selectedFormat.value)?.resolutions ?? [],
)
const availableFps = computed(() =>
  availableResolutions.value.find(
    resolution => `${resolution.width}x${resolution.height}` === selectedResolution.value,
  )?.fps ?? [],
)

async function loadCaptureDevices() {
  loadingDevices.value = true
  try {
    devices.value = (await configApi.listDevices()).video
  } catch {
    devices.value = []
  } finally {
    loadingDevices.value = false
  }
}

function initializeCaptureSettings() {
  const config = videoConfigStore.video
  if (!config) return
  selectedFormat.value = config.format || ''
  selectedResolution.value = `${config.width}x${config.height}`
  selectedFps.value = config.fps
}

function handleFormatChange(value: unknown) {
  if (typeof value !== 'string') return
  selectedFormat.value = value
  const resolution = availableFormats.value.find(format => format.format === value)?.resolutions[0]
  if (!resolution) return
  selectedResolution.value = `${resolution.width}x${resolution.height}`
  selectedFps.value = resolution.fps[0] ?? 30
}

function handleResolutionChange(value: unknown) {
  if (typeof value !== 'string') return
  selectedResolution.value = value
  const resolution = availableResolutions.value.find(
    item => `${item.width}x${item.height}` === value,
  )
  selectedFps.value = resolution?.fps[0] ?? 30
}

function handleFpsChange(value: unknown) {
  if (typeof value !== 'string' && typeof value !== 'number') return
  selectedFps.value = Number(value)
}

async function applyCaptureSettings() {
  const device = configuredDevicePath.value
  const [width, height] = selectedResolution.value.split('x').map(Number)
  if (!device || !selectedFormat.value || !Number.isFinite(width) || !Number.isFinite(height)) return

  applyingCapture.value = true
  try {
    await videoConfigStore.updateVideo({
      device,
      format: selectedFormat.value,
      width,
      height,
      fps: toConfigFps(selectedFps.value),
    })
  } finally {
    applyingCapture.value = false
  }
}

// Available codecs for selection (filtered by backend support and enriched with backend info)
const availableCodecs = computed(() => {
  const allAvailable = codecs.value.filter(c => c.available)

  // Auto mode: show all available with their best (hardware-preferred) backend
  if (currentEncoderBackend.value === 'auto') {
    return allAvailable
  }

  // Specific backend: filter by supported formats and override backend info
  const backend = backends.value.find(b => b.id === currentEncoderBackend.value)
  if (!backend) return allAvailable

  const backendFiltered = allAvailable
    .filter(codec => {
      // HTTP protocol codecs are always available (don't require encoder)
      if (codec.protocol === 'http') return true
      // Check if codec format is supported by the configured backend
      return backend.supported_formats.includes(codec.id)
    })
    .map(codec => {
      // HTTP protocol codecs keep original info
      if (codec.protocol === 'http') return codec

      // Override backend info for WebRTC codecs based on selected backend
      return {
        ...codec,
        hardware: backend.is_hardware,
        backend: backend.name,
      }
    })

  const allowed = constraints.value?.allowed_codecs
  if (!allowed || allowed.length === 0) {
    return backendFiltered
  }

  return backendFiltered.filter(codec => allowed.includes(codec.id))
})

const selectedCodecInfo = computed(() => {
  const codec = availableCodecs.value.find(c => c.id === props.videoMode)
  return codec || null
})

// Load available codecs and backends
async function loadCodecs() {
  loadingCodecs.value = true
  try {
    const result = await streamApi.getCodecs()
    codecs.value = result.codecs
    backends.value = result.backends || []
  } catch (e) {
    console.info('[VideoConfig] Failed to load codecs')
    codecs.value = [
      { id: 'mjpeg', name: 'MJPEG / HTTP', protocol: 'http', transports: ['http'], hardware: false, backend: 'software', available: true },
      { id: 'webp', name: 'WebP / HTTP', protocol: 'http', transports: ['http'], hardware: false, backend: 'software', available: true },
      { id: 'h264', name: 'H.264 / WebRTC', protocol: 'webrtc', transports: ['webrtc', 'webtransport'], hardware: false, backend: 'software', available: true },
      { id: 'vp8', name: 'VP8 / WebRTC', protocol: 'webrtc', transports: ['webrtc', 'webtransport'], hardware: false, backend: 'software', available: true },
    ]
  } finally {
    loadingCodecs.value = false
  }
}

function handleTransportPreferenceChange(value: unknown) {
  if (value !== 'webrtc' && value !== 'webtransport') return
  if (value === 'webtransport' && !canUseWebTransportForCurrentMode.value) {
    toast.warning(webTransportDisabledReason.value || 'WebTransport is not available')
    return
  }
  videoStore.setTransportPreference(value)
  if (currentProtocol.value !== 'http' && (activeTransport.value !== value || value === 'webtransport')) {
    emit('update:transport', value)
  }
}

async function loadConstraints() {
  try {
    constraints.value = await streamApi.getConstraints()
  } catch {
    constraints.value = null
  }
}

async function loadColorDepthProfiles() {
  loadingColorDepthProfiles.value = true
  try {
    const result = await streamApi.getColorDepthProfiles()
    syncColorDepthCapabilities(result)
  } catch {
    colorDepthProfiles.value = []
    selectedColorDepthProfile.value = ''
  } finally {
    loadingColorDepthProfiles.value = false
  }
}

function syncColorDepthCapabilities(result: VideoColorDepthCapabilitiesResponse) {
  colorDepthProfiles.value = result.supported ? result.profiles : []
  const activeProfile = colorDepthProfiles.value.find(
    profile => profile.id === result.active_profile_id,
  )
  selectedColorDepthProfile.value = activeProfile?.id ?? ''
  const requestedPreference = activeProfile
    ? (activeProfile.variant ? 'video' : 'performance')
    : colorPerformancePreference.value
  const resolvedPreference = resolveColorPreference(
    colorDepthProfiles.value,
    requestedPreference,
  )
  if (resolvedPreference) colorPerformancePreference.value = resolvedPreference
}

function colorDepthLabel(profile: VideoColorDepthProfile): string {
  return `${profile.depth_bpp} bpp`
}

async function applyColorDepthProfile(profile: VideoColorDepthProfile): Promise<boolean> {
  applyingColorDepth.value = true
  try {
    await streamApi.setColorDepthProfile(profile.id)
    selectedColorDepthProfile.value = profile.id
    colorPerformancePreference.value = profile.variant ? 'video' : 'performance'
    try {
      syncColorDepthCapabilities(await streamApi.getColorDepthProfiles())
    } catch {
      // The completed mutation is authoritative if the follow-up read fails.
    }
    toast.success('Remote color depth updated')
    return true
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    toast.error(`Could not update remote color depth: ${message}`)
    try {
      syncColorDepthCapabilities(await streamApi.getColorDepthProfiles())
    } catch {
      // Preserve the last source-confirmed state when recovery readback fails.
    }
    return false
  } finally {
    applyingColorDepth.value = false
  }
}

async function handleColorDepthChange(value: unknown) {
  if (typeof value !== 'string') return
  const profile = colorDepthOptions.value.find(candidate => candidate.id === value)
  if (profile) await applyColorDepthProfile(profile)
}

async function handleColorPreferenceChange(value: unknown) {
  if (value !== 'performance' && value !== 'video') return
  if (!colorPerformancePreferences.value.includes(value)) return
  const profile = profileForPreferenceChange(
    colorDepthProfiles.value,
    selectedColorDepthProfile.value,
    value,
  )
  if (!profile) return
  if (profile.id === selectedColorDepthProfile.value) {
    colorPerformancePreference.value = value
    return
  }
  await applyColorDepthProfile(profile)
}

async function loadWebTransportCapabilities() {
  loadingWebTransportCapabilities.value = true
  try {
    videoStore.webTransportCapabilities = await streamApi.getWebTransportCapabilities()
  } catch (e) {
    const message = e instanceof Error && e.message ? e.message : String(e)
    videoStore.webTransportCapabilities = {
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
    }
  } finally {
    loadingWebTransportCapabilities.value = false
  }
}

function goToSettings() {
  router.push({ name: 'DeviceSettings', params: { bmcId: route.params.bmcId }, query: { section: 'features' } })
}

function handleVideoModeChange(mode: unknown) {
  if (typeof mode !== 'string') return

  if (isRtspCodecLocked.value) {
    toast.warning(codecLockMessage.value)
    return
  }

  if (isRustdeskWebrtcLocked.value) {
    const c = codecs.value.find(c => c.id === mode)
    if (c?.protocol === 'http') {
      toast.warning(codecLockMessage.value)
      return
    }
  }

  if (constraints.value?.allowed_codecs?.length && !constraints.value.allowed_codecs.includes(mode)) {
    toast.error(constraints.value.reason || t('actionbar.selectMode'))
    return
  }

  emit('update:videoMode', mode as VideoMode)
}

async function applyBitratePreset(preset: 'Speed' | 'Balanced' | 'Quality') {
  if (applyingBitrate.value) return
  applyingBitrate.value = true
  try {
    const bitratePreset: BitratePreset = { type: preset }
    await streamApi.setBitratePreset(bitratePreset)
  } catch (e) {
    console.info('[VideoConfig] Failed to apply bitrate preset:', e)
  } finally {
    applyingBitrate.value = false
  }
}

function handleBitratePresetChange(preset: 'Speed' | 'Balanced' | 'Quality') {
  selectedBitratePreset.value = preset
  if (currentProtocol.value !== 'http') {
    applyBitratePreset(preset)
  }
}

watch(() => props.open, (isOpen) => {
  if (!isOpen) return

  // Detect browser codec support on first open
  if (browserSupportedCodecs.value.size === 0) {
    detectBrowserCodecSupport()
  }
  // Load codecs and backends on first open
  if (codecs.value.length === 0) {
    loadCodecs()
  }

  loadConstraints()
  loadWebTransportCapabilities()
  loadColorDepthProfiles()
  Promise.all([
    videoConfigStore.refreshVideo(),
    videoConfigStore.refreshStream(),
    loadCaptureDevices(),
  ]).then(initializeCaptureSettings).catch(initializeCaptureSettings)
})
</script>

<template>
  <Popover :open="open" @update:open="emit('update:open', $event)">
    <PopoverTrigger as-child>
      <Button variant="ghost" size="sm" class="h-7 w-7 sm:h-8 sm:w-auto p-0 sm:px-2 sm:gap-1.5 text-xs">
        <Monitor class="h-3.5 w-3.5 sm:h-4 sm:w-4" />
        <span class="hidden sm:inline">{{ buttonText }}</span>
      </Button>
    </PopoverTrigger>
    <PopoverContent class="w-[min(320px,92vw)] p-3" align="start">
      <div class="space-y-3">
        <h4 class="text-sm font-medium">{{ t('actionbar.videoConfig') }}</h4>

        <Separator />

        <!-- Stream Settings Section -->
        <div class="space-y-3">
          <h5 class="text-xs font-medium text-muted-foreground">{{ t('actionbar.streamSettings') }}</h5>

          <!-- Mode Selection -->
          <div class="space-y-2">
            <Label class="text-xs">{{ t('actionbar.videoMode') }}</Label>
            <Select
              :model-value="props.videoMode"
              @update:model-value="handleVideoModeChange"
              :disabled="loadingCodecs || availableCodecs.length === 0 || isRtspCodecLocked"
            >
              <SelectTrigger class="h-8 text-xs">
                <div v-if="selectedCodecInfo" class="flex items-center gap-1.5 truncate">
                  <span class="truncate">{{ selectedCodecInfo.name }}</span>
                  <span
                    v-if="selectedCodecInfo.backend && selectedCodecInfo.protocol !== 'http'"
                    class="text-[10px] px-1 py-0.5 rounded shrink-0"
                    :class="selectedCodecInfo.hardware
                      ? 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300'
                      : 'bg-orange-100 text-orange-700 dark:bg-orange-900 dark:text-orange-300'"
                  >
                    {{ translateBackendName(selectedCodecInfo.backend) }}
                  </span>
                </div>
                <span v-else class="text-muted-foreground">{{ loadingCodecs ? t('common.loading') : t('actionbar.selectMode') }}</span>
              </SelectTrigger>
              <SelectContent>
                <SelectItem
                  v-for="codec in availableCodecs"
                  :key="codec.id"
                  :value="codec.id"
                  :disabled="isCodecOptionDisabled(codec.id)"
                  :class="['text-xs', { 'opacity-50': isCodecOptionDisabled(codec.id) }]"
                >
                  <div class="flex items-center gap-2">
                    <span>{{ codec.name }}</span>
                    <!-- Show backend badge for WebRTC codecs -->
                    <span
                      v-if="codec.backend && codec.protocol !== 'http'"
                      class="text-[10px] px-1.5 py-0.5 rounded"
                      :class="codec.hardware
                        ? 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300'
                        : 'bg-orange-100 text-orange-700 dark:bg-orange-900 dark:text-orange-300'"
                    >
                      {{ translateBackendName(codec.backend) }}
                    </span>
                    <span
                      v-if="!isBrowserSupported(codec.id)"
                      class="text-[10px] text-muted-foreground"
                    >
                      ({{ t('actionbar.browserUnsupported') }})
                    </span>
                  </div>
                </SelectItem>
              </SelectContent>
            </Select>
            <p v-if="currentProtocol !== 'http'" class="text-xs text-muted-foreground">
              {{ t('actionbar.webrtcHint') }}
            </p>
            <div v-if="currentProtocol !== 'http'" class="space-y-1.5">
              <Label class="text-xs">Transport</Label>
              <div class="grid grid-cols-2 gap-1.5">
                <Button
                  size="sm"
                  variant="outline"
                  :aria-pressed="activeTransport === 'webrtc'"
                  :class="activeTransport === 'webrtc' && 'border-primary bg-primary/10'"
                  class="h-7 text-xs"
                  @click="handleTransportPreferenceChange('webrtc')"
                >
                  WebRTC
                </Button>
                <TooltipProvider :delay-duration="200">
                  <Tooltip>
                    <TooltipTrigger as-child>
                      <span class="block">
                        <Button
                          size="sm"
                          variant="outline"
                          :aria-pressed="activeTransport === 'webtransport'"
                          :class="activeTransport === 'webtransport' && 'border-primary bg-primary/10'"
                          class="h-7 w-full text-xs"
                          :disabled="!canUseWebTransportForCurrentMode"
                          @click="handleTransportPreferenceChange('webtransport')"
                        >
                          WebTransport
                        </Button>
                      </span>
                    </TooltipTrigger>
                    <TooltipContent v-if="!canUseWebTransportForCurrentMode" side="bottom" class="max-w-[260px] text-xs">
                      <p>{{ webTransportDisabledReason }}</p>
                    </TooltipContent>
                  </Tooltip>
                </TooltipProvider>
              </div>
              <p v-if="!canUseWebTransportForCurrentMode && webTransportDisabledReason" class="text-xs text-muted-foreground">
                {{ webTransportDisabledReason }}
              </p>
            </div>
            <p v-if="isCodecLocked" class="text-xs text-amber-600 dark:text-amber-400">
              {{ codecLockMessage }}
            </p>
          </div>

          <!-- Bitrate Preset - Only shown for WebRTC modes -->
          <div v-if="currentProtocol !== 'http'" class="space-y-2">
            <div class="flex items-center gap-1">
              <Label class="text-xs">{{ t('actionbar.bitratePreset') }}</Label>
              <HelpTooltip :content="t('help.videoBitratePreset')" icon-size="sm" />
            </div>
            <div class="grid grid-cols-3 gap-1.5">
              <Button
                variant="outline"
                size="sm"
                :class="[
                  'h-auto py-1.5 px-2 flex flex-col items-center gap-0.5',
                  selectedBitratePreset === 'Speed' && 'border-primary bg-primary/10'
                ]"
                :disabled="applyingBitrate"
                @click="handleBitratePresetChange('Speed')"
              >
                <Zap class="h-3.5 w-3.5" />
                <span class="text-[10px] font-medium">{{ t('actionbar.bitrateSpeed') }}</span>
              </Button>
              <Button
                variant="outline"
                size="sm"
                :class="[
                  'h-auto py-1.5 px-2 flex flex-col items-center gap-0.5',
                  selectedBitratePreset === 'Balanced' && 'border-primary bg-primary/10'
                ]"
                :disabled="applyingBitrate"
                @click="handleBitratePresetChange('Balanced')"
              >
                <Scale class="h-3.5 w-3.5" />
                <span class="text-[10px] font-medium">{{ t('actionbar.bitrateBalanced') }}</span>
              </Button>
              <Button
                variant="outline"
                size="sm"
                :class="[
                  'h-auto py-1.5 px-2 flex flex-col items-center gap-0.5',
                  selectedBitratePreset === 'Quality' && 'border-primary bg-primary/10'
                ]"
                :disabled="applyingBitrate"
                @click="handleBitratePresetChange('Quality')"
              >
                <Image class="h-3.5 w-3.5" />
                <span class="text-[10px] font-medium">{{ t('actionbar.bitrateQuality') }}</span>
              </Button>
            </div>
          </div>

          <Button
            variant="ghost"
            size="sm"
            class="w-full h-7 text-xs text-muted-foreground hover:text-foreground justify-start px-0"
            @click="goToSettings"
          >
            <Settings class="h-3.5 w-3.5 mr-1.5" />
            {{ t('actionbar.changeEncoderBackend') }}
          </Button>

          <div v-if="colorDepthProfiles.length > 0" class="space-y-2">
            <Label class="text-xs">Remote video performance / color depth</Label>
            <Tabs
              :model-value="colorPerformancePreference"
              aria-label="Remote video performance"
              @update:model-value="handleColorPreferenceChange"
            >
              <TabsList class="grid h-9 w-full grid-cols-2 rounded-lg p-1">
                <TabsTrigger
                  value="performance"
                  class="h-full gap-1.5 text-xs font-medium data-[state=active]:border-border data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm dark:data-[state=active]:bg-background"
                  :disabled="
                    loadingColorDepthProfiles
                    || applyingColorDepth
                    || !colorPerformancePreferences.includes('performance')
                  "
                >
                  <Zap class="h-3.5 w-3.5" />
                  Performance
                </TabsTrigger>
                <TabsTrigger
                  value="video"
                  class="h-full gap-1.5 text-xs font-medium data-[state=active]:border-border data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm dark:data-[state=active]:bg-background"
                  :disabled="
                    loadingColorDepthProfiles
                    || applyingColorDepth
                    || !colorPerformancePreferences.includes('video')
                  "
                >
                  <Image class="h-3.5 w-3.5" />
                  Max Video
                </TabsTrigger>
              </TabsList>
            </Tabs>
            <Select
              v-if="colorDepthOptions.length > 1"
              :model-value="selectedColorDepthProfile"
              :disabled="loadingColorDepthProfiles || applyingColorDepth"
              @update:model-value="handleColorDepthChange"
            >
              <SelectTrigger class="h-8 text-xs">
                <SelectValue placeholder="Select target color depth" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem
                  v-for="profile in colorDepthOptions"
                  :key="profile.id"
                  :value="profile.id"
                  class="text-xs"
                >
                  {{ colorDepthLabel(profile) }}
                </SelectItem>
              </SelectContent>
            </Select>
            <div
              v-else-if="colorDepthOptions[0]"
              class="flex h-8 items-center justify-between rounded-md border border-input bg-muted/30 px-3 text-xs"
            >
              <span class="text-muted-foreground">Target color depth</span>
              <span class="font-medium">{{ colorDepthLabel(colorDepthOptions[0]) }}</span>
            </div>
            <p class="text-[11px] text-muted-foreground">
              <template v-if="colorDepthProfiles.length === 1">
                This device exposes one verified console profile.
              </template>
              <template v-else>
                Only official iDRAC combinations are available. It applies to this console
                session only and is not persisted in device configuration.
              </template>
            </p>
          </div>
        </div>

        <template v-if="configuredDevice && availableFormats.length > 0">
          <Separator />
          <div class="space-y-3">
            <h5 class="text-xs font-medium text-muted-foreground">{{ t('settings.videoSettings') }}</h5>

            <div class="space-y-2">
              <Label class="text-xs">{{ t('actionbar.videoFormat') }}</Label>
              <Select :model-value="selectedFormat" @update:model-value="handleFormatChange">
                <SelectTrigger class="h-8 text-xs">
                  <SelectValue :placeholder="t('actionbar.selectFormat')" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem
                    v-for="format in availableFormats"
                    :key="format.format"
                    :value="format.format"
                    class="text-xs"
                  >
                    {{ format.description }}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div class="space-y-2">
              <Label class="text-xs">{{ t('actionbar.videoResolution') }}</Label>
              <Select
                :model-value="selectedResolution"
                :disabled="availableResolutions.length === 0"
                @update:model-value="handleResolutionChange"
              >
                <SelectTrigger class="h-8 text-xs">
                  <SelectValue :placeholder="t('actionbar.selectResolution')" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem
                    v-for="resolution in availableResolutions"
                    :key="`${resolution.width}x${resolution.height}`"
                    :value="`${resolution.width}x${resolution.height}`"
                    class="text-xs"
                  >
                    {{ resolution.width }} x {{ resolution.height }}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div class="space-y-2">
              <Label class="text-xs">{{ t('actionbar.videoFps') }}</Label>
              <Select
                :model-value="String(selectedFps)"
                :disabled="availableFps.length === 0"
                @update:model-value="handleFpsChange"
              >
                <SelectTrigger class="h-8 text-xs">
                  <SelectValue :placeholder="t('actionbar.selectFps')" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem v-for="fps in availableFps" :key="fps" :value="String(fps)" class="text-xs">
                    {{ formatFpsLabel(fps) }}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>

            <Button
              class="h-8 w-full text-xs"
              :disabled="loadingDevices || applyingCapture || !selectedFormat || !selectedResolution"
              @click="applyCaptureSettings"
            >
              <Loader2 v-if="applyingCapture" class="mr-1.5 h-3.5 w-3.5 animate-spin" />
              {{ applyingCapture ? t('actionbar.applying') : t('common.apply') }}
            </Button>
          </div>
        </template>

      </div>
    </PopoverContent>
  </Popover>
</template>
