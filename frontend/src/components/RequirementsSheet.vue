<script setup lang="ts">
import { computed, onActivated, onDeactivated, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { toast } from 'vue-sonner'
import {
  AlertCircle,
  Check,
  ChevronRight,
  CircleDashed,
  Loader2,
  RefreshCw,
  RotateCcw,
  ShieldCheck,
  Wrench,
} from 'lucide-vue-next'
import { systemApi, type BackendRequirement } from '@/api/config'
import { currentBmcIdFromLocation } from '@/api/request'
import { Button } from '@/components/ui/button'

const { t } = useI18n()
const requirements = ref<BackendRequirement[]>([])
const loading = ref(true)
const loadFailed = ref(false)
const selected = ref<BackendRequirement | null>(null)
const remediating = ref(false)
let refreshTimer: number | undefined
let disposed = false
let activated = true
let refreshSerial = 0
const ownerId = currentBmcIdFromLocation()
const isCurrent = () => !disposed && activated && ownerId === currentBmcIdFromLocation()

const summary = computed(() => ({
  actionRequired: requirements.value.filter(item => item.state === 'action_required').length,
  pending: requirements.value.filter(item => item.state === 'pending').length,
  satisfied: requirements.value.filter(item => item.state === 'satisfied').length,
}))

const statusMeta = (state: BackendRequirement['state']) => ({
  satisfied: { icon: Check, class: 'border-emerald-500/25 bg-emerald-500/8 text-emerald-600 dark:text-emerald-400', textClass: 'text-emerald-600 dark:text-emerald-400', label: 'requirements.state.satisfied' },
  action_required: { icon: AlertCircle, class: 'border-amber-500/25 bg-amber-500/8 text-amber-700 dark:text-amber-400', textClass: 'text-amber-700 dark:text-amber-400', label: 'requirements.state.actionRequired' },
  pending: { icon: RotateCcw, class: 'border-blue-500/25 bg-blue-500/8 text-blue-700 dark:text-blue-400', textClass: 'text-blue-700 dark:text-blue-400', label: 'requirements.state.pending' },
  unavailable: { icon: AlertCircle, class: 'border-rose-500/25 bg-rose-500/8 text-rose-700 dark:text-rose-400', textClass: 'text-rose-700 dark:text-rose-400', label: 'requirements.state.unavailable' },
  unknown: { icon: CircleDashed, class: 'border-border bg-muted/40 text-muted-foreground', textClass: 'text-muted-foreground', label: 'requirements.state.unknown' },
})[state]

async function refresh() {
  if (!isCurrent()) return
  const serial = ++refreshSerial
  loading.value = true
  try {
    const response = await systemApi.requirements()
    if (!isCurrent() || serial !== refreshSerial) return
    requirements.value = response.requirements
    loadFailed.value = false
  } catch {
    if (isCurrent() && serial === refreshSerial) loadFailed.value = true
  } finally {
    if (isCurrent() && serial === refreshSerial) {
      loading.value = false
      window.clearTimeout(refreshTimer)
      refreshTimer = requirements.value.some(item => item.state === 'unknown')
        ? window.setTimeout(() => void refresh(), 2000)
        : undefined
    }
  }
}

async function remediate() {
  if (!isCurrent() || remediating.value || !selected.value?.remediation) return
  remediating.value = true
  try {
    const result = await systemApi.remediateRequirement(
      selected.value.feature,
      selected.value.id,
      selected.value.remediation.action,
    )
    if (!isCurrent()) return
    closeConfirmation()
    await refresh()
    if (!isCurrent()) return
    toast.success(t('requirements.remediationAccepted'), {
      description: t(result.result.restart_requirement === 'next_host_post'
        ? 'requirements.restart.nextHostPost'
        : 'requirements.restart.none'),
    })
  } catch {
    // The API boundary reports remediation errors to the operator.
  } finally {
    remediating.value = false
  }
}

function openConfirmation(item: BackendRequirement) {
  selected.value = item
}

function closeConfirmation() {
  selected.value = null
}

onMounted(() => void refresh())
onActivated(() => {
  if (!activated) {
    activated = true
    void refresh()
  }
})
onDeactivated(() => {
  activated = false
  ++refreshSerial
  window.clearTimeout(refreshTimer)
})
onUnmounted(() => {
  disposed = true
  ++refreshSerial
  window.clearTimeout(refreshTimer)
})
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col">
    <template v-if="selected">
      <div class="flex min-h-0 flex-1 flex-col">
        <div class="border-b px-5 pb-4">
          <div class="flex items-center gap-3 pr-8">
            <div class="flex h-9 w-9 shrink-0 items-center justify-center border border-amber-500/25 bg-amber-500/8 text-amber-700 dark:text-amber-400">
              <Wrench class="h-4 w-4" />
            </div>
            <div class="min-w-0 flex-1">
              <h3 class="text-sm font-semibold">{{ t('requirements.confirmTitle') }}</h3>
              <p class="text-xs text-muted-foreground">{{ t(selected.title_key) }}</p>
            </div>
          </div>
        </div>

        <div class="min-h-0 flex-1 overflow-y-auto px-5 py-5">
          <div class="border p-4">
            <p class="text-sm font-medium">{{ t(selected.title_key) }}</p>
            <p class="mt-2 text-xs leading-5 text-muted-foreground">{{ t(selected.message_key) }}</p>
          </div>
          <div v-if="selected.remediation?.restart_requirement === 'next_host_post'" class="mt-4 border border-blue-500/25 bg-blue-500/5 px-3 py-3 text-xs leading-5 text-muted-foreground">
            {{ t('requirements.restart.nextHostPost') }}
          </div>
        </div>

        <div class="flex justify-end gap-2 border-t px-5 py-4">
          <Button variant="outline" :disabled="remediating" @click="closeConfirmation">{{ t('common.cancel') }}</Button>
          <Button :disabled="remediating" @click="remediate">
            <Loader2 v-if="remediating" class="mr-2 h-4 w-4 animate-spin" />
            {{ remediating ? t('common.loading') : t('common.confirm') }}
          </Button>
        </div>
      </div>
    </template>

    <template v-else>
      <div class="border-b px-5 pb-4">
      <div class="flex items-center gap-3">
        <div class="flex h-9 w-9 items-center justify-center border bg-muted/35">
          <ShieldCheck class="h-4 w-4 text-foreground" />
        </div>
        <div class="min-w-0 flex-1">
          <p class="text-sm font-semibold">{{ t('requirements.title') }}</p>
          <p class="text-xs text-muted-foreground">{{ t('requirements.description') }}</p>
        </div>
        <div class="flex shrink-0 items-center gap-1 pr-8">
          <Button variant="ghost" size="icon" class="h-8 w-8" :disabled="loading" :aria-label="t('common.refresh')" @click="refresh">
            <RefreshCw :class="['h-4 w-4', loading && 'animate-spin']" />
          </Button>
        </div>
      </div>

      <div v-if="requirements.length" class="mt-4 grid grid-cols-3 border">
        <div class="px-3 py-2.5">
          <p class="text-lg font-semibold tabular-nums">{{ summary.actionRequired }}</p>
          <p class="text-[10px] uppercase text-muted-foreground">{{ t('requirements.state.actionRequired') }}</p>
        </div>
        <div class="border-x px-3 py-2.5">
          <p class="text-lg font-semibold tabular-nums">{{ summary.pending }}</p>
          <p class="text-[10px] uppercase text-muted-foreground">{{ t('requirements.state.pending') }}</p>
        </div>
        <div class="px-3 py-2.5">
          <p class="text-lg font-semibold tabular-nums">{{ summary.satisfied }}</p>
          <p class="text-[10px] uppercase text-muted-foreground">{{ t('requirements.state.satisfied') }}</p>
        </div>
      </div>
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto px-5 py-4">
      <div v-if="loading && !requirements.length" class="flex h-32 items-center justify-center text-muted-foreground">
        <Loader2 class="mr-2 h-4 w-4 animate-spin" />
        <span class="text-xs">{{ t('requirements.checking') }}</span>
      </div>
      <div v-else-if="loadFailed" class="border border-rose-500/25 bg-rose-500/5 p-4">
        <p class="text-sm font-medium">{{ t('requirements.loadFailed') }}</p>
        <Button variant="outline" size="sm" class="mt-3" @click="refresh">{{ t('common.retry') }}</Button>
      </div>
      <div v-else-if="!requirements.length" class="flex h-36 flex-col items-center justify-center border border-dashed text-center">
        <ShieldCheck class="mb-2 h-5 w-5 text-muted-foreground" />
        <p class="text-sm font-medium">{{ t('requirements.none') }}</p>
        <p class="mt-1 max-w-[280px] text-xs text-muted-foreground">{{ t('requirements.noneDescription') }}</p>
      </div>
      <div v-else class="divide-y border">
        <div v-for="item in requirements" :key="`${item.feature}:${item.id}`" class="p-3.5">
          <div class="flex items-start gap-3">
            <div :class="['mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center border', statusMeta(item.state).class]">
              <component :is="statusMeta(item.state).icon" class="h-3.5 w-3.5" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
                <p class="text-sm font-medium">{{ t(item.title_key) }}</p>
                <span class="border px-1.5 py-0.5 text-[9px] uppercase text-muted-foreground">{{ item.feature }}</span>
              </div>
              <p class="mt-1 text-xs leading-5 text-muted-foreground">{{ t(item.message_key) }}</p>
              <div class="mt-2 flex items-center justify-between gap-3">
                <span :class="['text-[10px] font-medium uppercase', statusMeta(item.state).textClass]">
                  {{ t(statusMeta(item.state).label) }}
                </span>
                <Button
                  v-if="item.remediation"
                  variant="outline"
                  size="sm"
                  class="h-7 gap-1.5 text-xs"
                  @click="openConfirmation(item)"
                >
                  <Wrench class="h-3.5 w-3.5" />
                  {{ t('requirements.fix') }}
                  <ChevronRight class="h-3.5 w-3.5" />
                </Button>
              </div>
              <p v-if="item.remediation?.restart_requirement === 'next_host_post'" class="mt-2 border-l-2 border-blue-500/50 pl-2 text-[11px] leading-4 text-muted-foreground">
                {{ t('requirements.restart.nextHostPost') }}
              </p>
            </div>
          </div>
        </div>
      </div>
      </div>
    </template>
  </div>
</template>
