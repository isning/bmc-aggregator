<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useSystemStore } from '@/stores/system'
import StatusCard from '@/components/StatusCard.vue'

const { t } = useI18n()
const systemStore = useSystemStore()

defineProps<{
  compact?: boolean
}>()

const msdStatus = computed<'connected' | 'connecting' | 'disconnected' | 'error'>(() => {
  const msd = systemStore.msd
  if (!msd?.available) return 'disconnected'
  if (msd.error) return 'error'
  if (msd.attachments.length > 0) return 'connected'
  return 'disconnected'
})

const msdQuickInfo = computed(() => {
  const msd = systemStore.msd
  if (!msd?.available) return ''
  if (msd.attachments.length === 0) return t('statusCard.msdStandby')
  return `${msd.attachments.length} ${t('statusCard.connected')}`
})

const msdErrorMessage = computed(() => {
  return systemStore.msd?.error || ''
})

function mediaLabel(): string | null {
  const media = systemStore.msd?.attachments[0]?.media
  if (!media) return null
  return media.location === 'remote' ? media.url : media.image_id
}

const msdDetails = computed(() => {
  const msd = systemStore.msd
  if (!msd) return []

  type Det = { label: string; value: string; status?: 'ok' | 'warning' | 'error' }
  const details: Det[] = []

  if (msd.attachments.length === 0) {
    details.push({
      label: t('statusCard.msdStatus'),
      value: t('statusCard.msdStandby'),
      status: undefined,
    })
  } else {
    details.push({
      label: t('statusCard.msdStatus'),
      value: t('statusCard.connected'),
      status: 'ok',
    })
  }

  const mode = msd.attachments[0]?.mode
  const modeDisplay = !mode
    ? '-'
    : mode === 'image'
      ? t('statusCard.msdImageMode')
      : t('statusCard.msdDriveMode')
  details.push({
    label: t('statusCard.currentMode'),
    value: modeDisplay,
    status: mode ? 'ok' : undefined,
  })

  if (mode === 'image') {
    details.push({
      label: t('statusCard.msdCurrentImage'),
      value: mediaLabel() || t('statusCard.msdNoImage'),
    })
  }

  return details
})
</script>

<template>
  <StatusCard
    :title="t('statusCard.msd')"
    type="msd"
    :status="msdStatus"
    :quick-info="msdQuickInfo"
    :error-message="msdErrorMessage"
    :details="msdDetails"
    hover-align="end"
    :compact="compact"
  />
</template>
