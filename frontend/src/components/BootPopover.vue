<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { bootApi, type BootMode, type BootOptionEntry } from '@/api'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Label } from '@/components/ui/label'
import { Select, SelectContent, SelectGroup, SelectItem, SelectLabel, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { CalendarClock, Repeat2 } from 'lucide-vue-next'
import { useFeatureDataSubscription } from '@/feature-data/resource'
import { bootStatusResource } from '@/feature-data/resources'
import { bootEmptyState } from '@/features/boot/presentation'

const { t } = useI18n()
const props = defineProps<{ active?: boolean }>()
const active = computed(() => props.active ?? true)
useFeatureDataSubscription(bootStatusResource, active)

const loading = computed(() => bootStatusResource.phase.value === 'loading' || bootStatusResource.phase.value === 'idle')
const loadFailed = computed(() => bootStatusResource.phase.value === 'error')
const entries = computed<BootOptionEntry[]>(() => bootStatusResource.data.value?.entries ?? [])
const entryGroups = computed(() => {
  const groups = new Map<string, BootOptionEntry[]>()
  for (const entry of entries.value) {
    const group = entry.group?.trim() || entry.provider
    const groupedEntries = groups.get(group)
    if (groupedEntries) groupedEntries.push(entry)
    else groups.set(group, [entry])
  }
  return Array.from(groups, ([label, groupedEntries]) => ({ label, entries: groupedEntries }))
})
const entryId = ref('')
const mode = ref<BootMode>('once')
const saving = ref(false)
const selectedEntry = computed(() => entries.value.find(entry => entry.id === entryId.value))
const selectedModes = computed(() => selectedEntry.value?.supported_modes ?? [])
const selectionAvailable = computed(() => !!selectedEntry.value?.enabled && !!selectedEntry.value?.selectable)
const providerFaults = computed(() => (bootStatusResource.data.value?.providers ?? []).filter(provider => provider.fault))
const emptyState = computed(() => bootEmptyState(bootStatusResource.data.value))

watch(bootStatusResource.data, status => {
  if (!status) return
  entryId.value = status.selection?.entry_id ?? entryId.value
  if (!entryId.value || !entries.value.some(entry => entry.id === entryId.value)) {
    entryId.value = entries.value.find(entry => entry.enabled && entry.selectable)?.id ?? entries.value[0]?.id ?? ''
  }
  const selected = entries.value.find(entry => entry.id === entryId.value)
  const nextMode = status.selection?.mode ?? selected?.supported_modes[0]
  if (nextMode && selected?.supported_modes.includes(nextMode)) mode.value = nextMode
}, { immediate: true })

watch(selectedEntry, entry => {
  if (!entry) return
  if (!entry.supported_modes.includes(mode.value)) mode.value = entry.supported_modes[0] ?? 'once'
})

async function save() {
  if (!selectedEntry.value) return
  saving.value = true
  try {
    await bootStatusResource.mutate(mutationId => bootApi.set(entryId.value, mode.value, mutationId))
  } finally {
    saving.value = false
  }
}

function handleModeChange(value: string | number) {
  if (value === 'once' || value === 'persistent') mode.value = value
}
</script>

<template>
  <div class="space-y-3 p-3">
    <div v-if="loading" class="flex h-24 items-center justify-center text-xs text-muted-foreground">
      {{ t('common.loading') }}
    </div>
    <div v-else-if="loadFailed" class="rounded-md border border-amber-500/30 bg-amber-500/5 p-3">
      <p class="text-xs font-medium">{{ t('atx.bootUnavailable') }}</p>
      <p class="mt-1 text-xs leading-5 text-muted-foreground">{{ t('atx.bootLoadFailed') }}</p>
    </div>
    <template v-else>
      <div v-if="entries.length" class="space-y-1.5">
        <Label for="boot-entry" class="text-xs">{{ t('atx.bootSelection') }}</Label>
        <Select v-model="entryId">
          <SelectTrigger id="boot-entry" class="h-8 text-xs">
            <SelectValue>
              <span class="flex min-w-0 items-center gap-2">
                <span class="min-w-0 truncate">{{ selectedEntry?.label || selectedEntry?.id }}</span>
                <span v-if="selectedEntry?.badges?.length" class="flex shrink-0 gap-1">
                  <Badge v-for="badge in selectedEntry.badges" :key="badge" as="span" variant="outline" class="h-4 px-1.5 text-[9px] font-medium leading-none text-muted-foreground">
                    {{ badge }}
                  </Badge>
                </span>
              </span>
            </SelectValue>
          </SelectTrigger>
          <SelectContent>
            <SelectGroup v-for="group in entryGroups" :key="group.label">
              <SelectLabel class="px-2 pb-1 pt-1.5 text-[10px] font-medium uppercase text-muted-foreground">
                {{ group.label }}
              </SelectLabel>
              <SelectItem v-for="entry in group.entries" :key="entry.id" :value="entry.id" :text-value="entry.label || entry.id" :disabled="!entry.enabled || !entry.selectable">
                <span class="inline-flex max-w-full min-w-0 items-center gap-2 pr-5">
                  <span class="min-w-0 truncate">{{ entry.label || entry.id }}</span>
                  <span v-if="entry.badges?.length" class="flex shrink-0 gap-1">
                    <Badge v-for="badge in entry.badges" :key="badge" as="span" variant="outline" class="h-4 px-1.5 text-[9px] font-medium leading-none text-muted-foreground">
                      {{ badge }}
                    </Badge>
                  </span>
                </span>
              </SelectItem>
            </SelectGroup>
          </SelectContent>
        </Select>
      </div>
      <p v-else-if="emptyState === 'discovering'" class="rounded-md border border-border bg-muted/40 px-2.5 py-2 text-[11px] leading-4 text-muted-foreground">
        {{ t('atx.bootEntryDiscovering') }}
      </p>
      <p v-else-if="emptyState === 'unavailable'" class="rounded-md border border-amber-500/30 bg-amber-500/5 px-2.5 py-2 text-[11px] leading-4 text-muted-foreground">
        {{ t('atx.bootEntriesUnavailable') }}
      </p>
      <p v-else class="rounded-md border border-border bg-muted/40 px-2.5 py-2 text-[11px] leading-4 text-muted-foreground">
        {{ t('atx.bootEntriesEmpty') }}
      </p>
      <p v-for="provider in providerFaults" :key="provider.provider" class="rounded-md border border-amber-500/20 bg-amber-500/5 px-2.5 py-2 text-[11px] leading-4 text-muted-foreground">
        {{ provider.fault?.message }}
      </p>
      <div v-if="selectedEntry" class="space-y-1.5">
        <Label class="text-xs">{{ t('atx.bootDuration') }}</Label>
        <Tabs v-if="selectedModes.length" :model-value="mode" @update:model-value="handleModeChange">
          <TabsList class="grid h-14 w-full rounded-lg p-1" :class="selectedModes.length > 1 ? 'grid-cols-2' : 'grid-cols-1'">
            <TabsTrigger v-for="item in selectedModes" :key="item" :value="item" class="h-full gap-2 px-2 text-left data-[state=active]:border-border data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm dark:data-[state=active]:bg-background">
              <CalendarClock v-if="item === 'once'" class="size-4 shrink-0" />
              <Repeat2 v-else class="size-4 shrink-0" />
              <span class="flex min-w-0 flex-col items-start leading-tight">
                <span class="text-xs font-medium">{{ item === 'once' ? t('atx.bootOnce') : t('atx.bootPersistent') }}</span>
                <span class="text-[9px] font-normal text-muted-foreground">{{ item === 'once' ? t('atx.bootOnceHint') : t('atx.bootPersistentHint') }}</span>
              </span>
            </TabsTrigger>
          </TabsList>
        </Tabs>
      </div>
      <Button size="sm" class="h-8 w-full text-xs" :disabled="saving || !selectionAvailable" @click="save">{{ t('common.save') }}</Button>
    </template>
  </div>
</template>
