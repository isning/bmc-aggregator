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

function translateAudioQuality(quality: string | undefined): string {
  if (!quality) return t('common.unknown')
  const qualityLower = quality.toLowerCase()
  if (qualityLower === 'voice') return t('actionbar.qualityVoice')
  if (qualityLower === 'balanced') return t('actionbar.qualityBalanced')
  if (qualityLower === 'high') return t('actionbar.qualityHigh')
  return quality
}

const audioStatus = computed<'connected' | 'connecting' | 'disconnected' | 'error'>(() => {
  const audio = systemStore.audio
  if (!audio?.available) return 'disconnected'
  if (audio.error) return 'error'
  if (audio.streaming) return 'connected'
  return 'disconnected'
})

const audioQuickInfo = computed(() => {
  const audio = systemStore.audio
  if (!audio?.available) return ''
  if (audio.streaming) return translateAudioQuality(audio.quality)
  return t('statusCard.off')
})

const audioErrorMessage = computed(() => {
  return systemStore.audio?.error || ''
})

const audioDetails = computed(() => {
  const audio = systemStore.audio
  if (!audio) return []

  return [
    { label: t('statusCard.device'), value: audio.device || t('statusCard.defaultDevice') },
    { label: t('statusCard.quality'), value: translateAudioQuality(audio.quality) },
    { label: t('statusCard.streaming'), value: audio.streaming ? t('common.yes') : t('common.no'), status: audio.streaming ? ('ok' as const) : undefined },
  ]
})
</script>

<template>
  <StatusCard
    v-if="systemStore.audio?.available"
    :title="t('statusCard.audio')"
    type="audio"
    :status="audioStatus"
    :quick-info="audioQuickInfo"
    :error-message="audioErrorMessage"
    :details="audioDetails"
    :compact="compact"
  />
</template>
