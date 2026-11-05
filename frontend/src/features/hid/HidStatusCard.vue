<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useSystemStore } from '@/stores/system'
import { useHidConfigStore } from '@/stores/hidConfig'
import { useHidEventBus } from './eventBus'
import StatusCard from '@/components/StatusCard.vue'
import { formatFeatureBackend } from '@/lib/featureDisplay'

const { t } = useI18n()
const systemStore = useSystemStore()
const hidConfigStore = useHidConfigStore()
const hidBus = useHidEventBus()

defineProps<{
  compact?: boolean
}>()

const mouseMode = computed<'absolute' | 'relative'>(() =>
  hidConfigStore.hid?.mouse_absolute ? 'absolute' : 'relative'
)

const hoverAlign = computed<'start' | 'end'>(() => {
  return systemStore.isFeatureAvailable('msd') ? 'start' : 'end'
})

const hidStatus = computed<'connected' | 'connecting' | 'disconnected' | 'error'>(() => {
  const hid = systemStore.hid
  if (hid?.errorCode === 'udc_not_configured') return 'disconnected'
  if (hid?.error) return 'error'

  if (hidBus.networkError.value) return 'connecting'

  if (hidBus.hidUnavailable.value) return 'disconnected'

  if (!hidBus.connected.value) return 'disconnected'

  if (hid?.available && hid.online) return 'connected'
  if (hid?.available && hid.initialized) return 'connecting'
  return 'disconnected'
})

const hidQuickInfo = computed(() => {
  const hid = systemStore.hid
  if (!hid?.available) return ''
  return mouseMode.value === 'absolute' ? t('statusCard.absolute') : t('statusCard.relative')
})

function extractCh9329Command(reason?: string | null): string | null {
  if (!reason) return null
  const match = reason.match(/cmd 0x([0-9a-f]{2})/i)
  const cmd = match?.[1]
  return cmd ? `0x${cmd.toUpperCase()}` : null
}

function hidErrorHint(errorCode?: string | null, backend?: string | null, reason?: string | null): string {
  const ch9329Command = extractCh9329Command(reason)

  switch (errorCode) {
    case 'udc_not_configured':
      return t('hid.errorHints.udcNotConfigured')
    case 'disabled':
      return t('hid.errorHints.disabled')
    case 'enoent':
      return t('hid.errorHints.hidDeviceMissing')
    case 'not_opened':
      return t('hid.errorHints.notOpened')
    case 'port_not_found':
      return t('hid.errorHints.portNotFound')
    case 'invalid_config':
      return t('hid.errorHints.invalidConfig')
    case 'no_response':
      return t(ch9329Command ? 'hid.errorHints.noResponseWithCmd' : 'hid.errorHints.noResponse', {
        cmd: ch9329Command ?? '',
      })
    case 'protocol_error':
    case 'invalid_response':
      return t('hid.errorHints.protocolError')
    case 'enxio':
    case 'enodev':
      return t('hid.errorHints.deviceDisconnected')
    case 'eio':
    case 'epipe':
    case 'eshutdown':
    case 'io_error':
    case 'write_failed':
    case 'read_failed':
      if (backend === 'ch9329') return t('hid.errorHints.ch9329IoError')
      return t('hid.errorHints.ioError')
    case 'serial_error':
      return t('hid.errorHints.serialError')
    case 'init_failed':
      return t('hid.errorHints.initFailed')
    case 'shutdown':
      return t('hid.errorHints.shutdown')
    default:
      return ''
  }
}

function buildHidErrorMessage(reason?: string | null, errorCode?: string | null, backend?: string | null): string {
  if (!reason && !errorCode) return ''
  const hint = hidErrorHint(errorCode, backend, reason)
  if (hint) return hint
  if (reason) return reason
  return hint || t('common.error')
}

const hidErrorMessage = computed(() => {
  const hid = systemStore.hid
  return buildHidErrorMessage(hid?.error, hid?.errorCode, hid?.backend)
})

const hidDetails = computed(() => {
  const hid = systemStore.hid
  if (!hid) return []

  type Det = { label: string; value: string; status?: 'ok' | 'warning' | 'error' }
  const errorMessage = buildHidErrorMessage(hid.error, hid.errorCode, hid.backend)
  const hidErrorStatus: 'ok' | 'warning' | 'error' =
    hid.errorCode === 'udc_not_configured' ? 'warning' : 'error'

  const details: Det[] = []

  const backendStr = formatFeatureBackend(systemStore.getFeatureBackend('hid') || hid.backend)
  if (backendStr) {
    details.push({ label: t('statusCard.transport'), value: backendStr })
  }
  if (hid.device) {
    details.push({ label: t('statusCard.source'), value: hid.device })
  }

  if (errorMessage) {
    const codeSuffix = hid.errorCode ? ` (${hid.errorCode})` : ''
    details.push({ label: t('common.error'), value: `${errorMessage}${codeSuffix}`, status: hidErrorStatus })
  } else if (hid.online) {
    details.push({ label: t('statusCard.currentMode'), value: mouseMode.value === 'absolute' ? t('statusCard.absolute') : t('statusCard.relative'), status: 'ok' })
    if (hid.keyboardLedsEnabled) {
      details.push({
        label: t('statusCard.keyboardLeds'),
        value: `Caps:${hid.ledState.capsLock ? t('common.on') : t('common.off')} Num:${hid.ledState.numLock ? t('common.on') : t('common.off')} Scroll:${hid.ledState.scrollLock ? t('common.on') : t('common.off')}`,
        status: 'ok',
      })
    }
  }

  let channelValue: string
  let channelStatus: 'ok' | 'warning' | 'error' | undefined
  channelValue = 'WebSocket'
  channelStatus = hidBus.connected.value ? 'ok' : 'warning'
  if (hidBus.networkError.value) {
    channelValue += ` (${hidBus.networkErrorMessage.value ?? t('statusCard.networkError')})`
    channelStatus = 'warning'
  } else if (hidBus.hidUnavailable.value) {
    channelValue += ` (${hidBus.networkErrorMessage.value ?? t('statusCard.hidUnavailable')})`
    channelStatus = 'warning'
  } else if (!hidBus.connected.value) {
    channelValue += ` (${t('statusCard.disconnected')})`
    channelStatus = 'warning'
  }
  details.push({ label: t('statusCard.channel'), value: channelValue, status: channelStatus })

  if (hidBus.lastSendError.value) {
    details.push({
      label: t('statusCard.lastError'),
      value: hidBus.lastSendError.value,
      status: 'warning',
    })
  }

  if (hidBus.droppedEvents.value > 0) {
    details.push({
      label: t('statusCard.droppedEvents'),
      value: String(hidBus.droppedEvents.value),
      status: 'warning',
    })
  }

  return details
})
</script>

<template>
  <StatusCard
    :title="t('statusCard.hid')"
    type="hid"
    :status="hidStatus"
    :quick-info="hidQuickInfo"
    :error-message="hidErrorMessage"
    :details="hidDetails"
    :hover-align="hoverAlign"
    :compact="compact"
  />
</template>
