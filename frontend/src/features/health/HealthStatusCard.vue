<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusCard, { type StatusDetail } from '@/components/StatusCard.vue'
import { healthStatusResource } from '@/feature-data/resources'
import { useFeatureDataSubscription } from '@/feature-data/resource'
import { formatFeatureBackend } from '@/lib/featureDisplay'

defineProps<{ compact?: boolean }>()
const { t } = useI18n()

useFeatureDataSubscription(healthStatusResource, ref(true))

const data = computed(() => healthStatusResource.data.value)
const overall = computed(() => data.value?.health?.overall_status ?? null)
const status = computed(() => {
  if (healthStatusResource.phase.value === 'error') return 'error'
  if (['loading', 'refreshing'].includes(healthStatusResource.phase.value)) return 'connecting'
  if (!data.value?.online) return 'disconnected'
  return overall.value?.toLowerCase() === 'critical' ? 'error' : 'connected'
})
function reading(sensor: NonNullable<NonNullable<typeof data.value>['health']>['sensors'][number]) {
  if (sensor.reading === null) return sensor.status || t('featureData.unknown')
  return `${sensor.reading}${sensor.unit ? ` ${sensor.unit}` : ''}`
}
const details = computed<StatusDetail[]>(() => {
  const backend = formatFeatureBackend(data.value?.backend)
  const values: StatusDetail[] = data.value?.health?.sensors.map(sensor => ({
    label: sensor.name || sensor.id,
    value: reading(sensor),
    status: sensor.status?.toLowerCase() === 'critical' ? 'error' : undefined,
  })) ?? []
  if (overall.value) values.unshift({ label: t('featureData.overall'), value: overall.value })
  if (backend) values.unshift({ label: t('statusCard.transport'), value: backend })
  return values
})
</script>

<template>
  <StatusCard
    :title="t('featureData.health')"
    type="health"
    :status="status"
    :quick-info="overall || t('featureData.sensorCount', { count: data?.health?.sensors.length ?? 0 })"
    :error-message="healthStatusResource.error.value ? String(healthStatusResource.error.value) : ''"
    :details="details"
    :compact="compact"
  />
</template>
