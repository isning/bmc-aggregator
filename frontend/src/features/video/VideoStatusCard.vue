<script setup lang="ts">
import { computed } from 'vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import { useSystemStore } from '@/stores/system'
import { useWebRTC } from '@/composables/useWebRTC'
import { useWebSocket } from '@/composables/useWebSocket'
import { useVideoStore } from './store'
import StatusCard from '@/components/StatusCard.vue'
import { formatFpsValue } from '@/lib/fps'
import { formatFeatureBackend } from '@/lib/featureDisplay'
import { formatVideoDeviceLabel } from '@/lib/video-device-label'

const { t } = useI18n()
const systemStore = useSystemStore()
const webrtc = useWebRTC()
const { networkError: wsNetworkError } = useWebSocket()
const {
  videoMode, isHttpMode, backendFps, mjpegFrameReceived,
  videoLoading, videoError, videoErrorMessage,
} = storeToRefs(useVideoStore())

defineProps<{
  compact?: boolean
}>()

function getResolutionShortName(width: number, height: number): string {
  if (height === 2160 || (height === 2160 && width === 4096)) return '4K'
  if (height === 1440) return '2K'
  if (height === 1080) return '1080p'
  if (height === 720) return '720p'
  if (height === 768) return '768p'
  if (height === 600) return '600p'
  if (height === 1024 && width === 1280) return '1024p'
  if (height === 960) return '960p'
  return `${height}p`
}

const isMjpegPaused = computed(() => {
  if (!isHttpMode.value) return false
  const stream = systemStore.stream
  if (!stream) return false
  return stream.online === false
})

const videoStatus = computed<'connected' | 'connecting' | 'disconnected' | 'error'>(() => {
  if (wsNetworkError.value) return 'connecting'
  if (videoError.value) return 'error'
  if (videoLoading.value) return 'connecting'
  if (!isHttpMode.value) {
    if (webrtc.isConnecting.value) return 'connecting'
    if (webrtc.isConnected.value) return 'connected'
  }
  if (isHttpMode.value && mjpegFrameReceived.value) return 'connected'
  if (systemStore.stream?.online) return 'connected'
  return 'disconnected'
})

const videoQuickInfo = computed(() => {
  const stream = systemStore.stream
  if (!stream?.resolution) return ''
  const resShort = getResolutionShortName(stream.resolution[0], stream.resolution[1])
  if (isMjpegPaused.value) return `${resShort} ${t('statusCard.paused')}`
  return `${resShort} ${formatFpsValue(backendFps.value)}fps`
})

const videoBackend = computed(() => formatFeatureBackend(systemStore.getFeatureBackend('video')))

const videoDetails = computed(() => {
  const stream = systemStore.stream
  if (!stream) return []
  const receivedFps = backendFps.value
  const paused = isMjpegPaused.value

  const inputFmt = stream.format || 'MJPEG'
  const outputFmt = isHttpMode.value
    ? videoMode.value.toUpperCase()
    : `${videoMode.value.toUpperCase()} (WebRTC)`
  const formatDisplay = inputFmt === outputFmt ? inputFmt : `${inputFmt} → ${outputFmt}`

  const targetFpsValue = formatFpsValue(stream.targetFps ?? 0)
  const actualFpsValue = paused ? t('statusCard.paused') : formatFpsValue(receivedFps)
  const actualStatus: 'ok' | 'warning' | 'error' | undefined = paused
    ? undefined
    : receivedFps > 5 ? 'ok'
      : receivedFps > 0 ? 'warning'
        : 'error'

  const details = [
    ...(videoBackend.value
      ? [{ label: t('statusCard.transport'), value: videoBackend.value }]
      : []),
    ...(stream.device
      ? [{
          label: t('statusCard.source'),
          value: formatVideoDeviceLabel({ path: stream.device }),
        }]
      : []),
    { label: t('statusCard.format'), value: formatDisplay },
    { label: t('statusCard.resolution'), value: stream.resolution ? `${stream.resolution[0]}x${stream.resolution[1]}` : '-' },
    { label: t('statusCard.fpsTarget'), value: targetFpsValue },
    { label: t('statusCard.fpsActual'), value: actualFpsValue, status: actualStatus },
  ]

  if (isHttpMode.value && !paused && receivedFps > 0 && receivedFps < (stream.targetFps ?? 0)) {
    details.push({
      label: '',
      value: t('statusCard.fpsStaticHint'),
    })
  }

  return details
})
</script>

<template>
  <StatusCard
    :title="t('statusCard.video')"
    type="video"
    :status="videoStatus"
    :quick-info="videoQuickInfo"
    :error-message="videoErrorMessage"
    :details="videoDetails"
    :compact="compact"
  />
</template>
