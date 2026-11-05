<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusCard, { type StatusDetail } from '@/components/StatusCard.vue'
import { firmwareStatusResource } from '@/feature-data/resources'
import { useFeatureDataSubscription } from '@/feature-data/resource'
import { formatFeatureBackend } from '@/lib/featureDisplay'

defineProps<{ compact?: boolean }>()
const { t } = useI18n()

useFeatureDataSubscription(firmwareStatusResource, ref(true))

const data = computed(() => firmwareStatusResource.data.value)
const status = computed(() => {
  if (firmwareStatusResource.phase.value === 'error') return 'error'
  if (['loading', 'refreshing'].includes(firmwareStatusResource.phase.value)) return 'connecting'
  return data.value?.online ? 'connected' : 'disconnected'
})
const details = computed<StatusDetail[]>(() => {
  const records = data.value?.inventory ?? []
  const values = records.map(record => ({
    label: record.name || record.id,
    value: record.version || t('featureData.unknown'),
    status: record.status?.toLowerCase() === 'critical' ? 'error' as const : undefined,
  }))
  const backend = formatFeatureBackend(data.value?.backend)
  return backend
    ? [{ label: t('statusCard.transport'), value: backend }, ...values]
    : values
})
</script>

<template>
  <StatusCard
    :title="t('featureData.firmware')"
    type="firmware"
    :status="status"
    :quick-info="t('featureData.componentCount', { count: data?.inventory.length ?? 0 })"
    :error-message="firmwareStatusResource.error.value ? String(firmwareStatusResource.error.value) : ''"
    :details="details"
    :compact="compact"
  />
</template>
