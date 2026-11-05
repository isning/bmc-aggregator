<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { identifyApi, type IdentifyMode } from '@/api'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useFeatureDataSubscription } from '@/feature-data/resource'
import { identifyStatusResource } from '@/feature-data/resources'

const { t } = useI18n()
const props = defineProps<{ active?: boolean }>()
const active = computed(() => props.active ?? true)
useFeatureDataSubscription(identifyStatusResource, active)
const available = computed(() => identifyStatusResource.data.value !== null)
const loading = computed(() => identifyStatusResource.phase.value === 'loading' || identifyStatusResource.phase.value === 'idle')
const mode = computed<IdentifyMode>(() => identifyStatusResource.data.value?.identify.mode ?? 'off')
const writable = computed(() => identifyStatusResource.data.value?.capabilities.writable ?? false)
const saving = ref(false)

async function setMode(next: IdentifyMode) {
  saving.value = true
  try {
    await identifyStatusResource.mutate(mutationId =>
      identifyApi.set(next, next === 'temporary' ? 15 : undefined, mutationId),
    )
  } finally {
    saving.value = false
  }
}

</script>

<template>
  <div class="space-y-3 p-3">
    <div v-if="loading" class="flex h-24 items-center justify-center text-xs text-muted-foreground">
      {{ t('common.loading') }}
    </div>
    <template v-else>
    <div class="flex items-center justify-between rounded-md bg-muted/50 p-2">
      <span class="text-xs text-muted-foreground">{{ t('atx.uidState') }}</span>
      <Badge variant="outline" class="text-xs">{{ mode }}</Badge>
    </div>
    <div v-if="available && writable" class="grid grid-cols-3 gap-1">
      <Button size="sm" class="h-8 text-xs" :variant="mode === 'off' ? 'secondary' : 'outline'" :disabled="saving" @click="setMode('off')">{{ t('common.off') }}</Button>
      <Button size="sm" class="h-8 text-xs" :variant="mode === 'temporary' ? 'secondary' : 'outline'" :disabled="saving" @click="setMode('temporary')">15s</Button>
      <Button size="sm" class="h-8 text-xs" :variant="mode === 'indefinite' ? 'secondary' : 'outline'" :disabled="saving" @click="setMode('indefinite')">{{ t('atx.uidOn') }}</Button>
    </div>
    <p v-else-if="available" class="text-xs leading-5 text-muted-foreground">{{ t('atx.uidReadOnly') }}</p>
    <p v-else class="text-xs text-muted-foreground">{{ t('common.unavailable') }}</p>
    </template>
  </div>
</template>
