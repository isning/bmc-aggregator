<script setup lang="ts">
import { computed } from 'vue'
import { featureRegistry } from './registry'

defineProps<{
  compact?: boolean
}>()

const cards = computed(() => featureRegistry.getStatusCards())
</script>

<template>
  <template v-for="(card, idx) in cards" :key="idx">
    <component :is="card" :compact="compact" />
  </template>
</template>
