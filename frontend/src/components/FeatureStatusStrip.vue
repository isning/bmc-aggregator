<script setup lang="ts">
import { computed, onMounted, onUnmounted } from 'vue'
import { useSystemStore } from '@/stores/system'
import { featureRegistry } from '@/features'
import type { FeatureStatusInfo } from '@/api/config'

const props = withDefaults(defineProps<{
  compact?: boolean
  limit?: number
}>(), {
  compact: false,
  limit: undefined,
})

const systemStore = useSystemStore()

onMounted(() => {
  systemStore.startFeatureStatusPolling()
})

onUnmounted(() => {
  systemStore.stopFeatureStatusPolling()
})

const visibleFeatures = computed(() => {
  const features = (systemStore.featureStatuses as FeatureStatusInfo[])
    .filter(feature => feature.available && featureRegistry.getStatusCard(feature.id))
    .map(feature => ({
      id: feature.id,
      component: featureRegistry.getStatusCard(feature.id),
    }))
  return typeof props.limit === 'number' ? features.slice(0, props.limit) : features
})
</script>

<template>
  <template v-for="feature in visibleFeatures" :key="feature.id">
    <component :is="feature.component" :compact="compact" />
  </template>
</template>
