<script setup lang="ts">
import { ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { toast } from 'vue-sonner'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Separator } from '@/components/ui/separator'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Power, RotateCcw, CircleDot, Wifi, Send, ChevronDown, Loader2 } from 'lucide-vue-next'
import { powerApi, type PowerAction } from '@/api'
import { currentBmcIdFromLocation } from '@/api/request'
import { powerConfigApi } from '@/api/config'
import { useFeatureDataSubscription } from '@/feature-data/resource'
import { powerStatusResource } from '@/feature-data/resources'

const props = defineProps<{ active?: boolean }>()
const ownerId = currentBmcIdFromLocation()
const active = computed(() => props.active ?? true)
useFeatureDataSubscription(powerStatusResource, active)

const emit = defineEmits<{
  (e: 'close'): void
  (e: 'powerShort'): void
  (e: 'powerLong'): void
  (e: 'reset'): void
  (e: 'wol', macAddress: string): void
}>()

const { t } = useI18n()

const powerState = computed<'on' | 'off' | 'unknown'>(() => powerStatusResource.data.value?.power_status ?? 'unknown')
const supportedActions = computed<PowerAction[]>(() => powerStatusResource.data.value?.capabilities?.actions ?? [])
// Decouple action data from dialog visibility to prevent race conditions
type UiPowerAction = 'on' | 'off' | 'reset' | 'graceful_shutdown' | 'graceful_restart'
const pendingAction = ref<UiPowerAction | null>(null)
const confirmDialogOpen = ref(false)
const powerActionPending = ref(false)

const wolMacAddress = ref('')
const wolHistory = ref<string[]>([])
const wolSending = ref(false)
const wolLoadingHistory = ref(false)
const wolExpanded = ref(false)

const powerStateColor = computed(() => {
  switch (powerState.value) {
    case 'on': return 'bg-green-500'
    case 'off': return 'bg-slate-400'
    default: return 'bg-yellow-500'
  }
})

const powerStateText = computed(() => {
  switch (powerState.value) {
    case 'on': return t('atx.stateOn')
    case 'off': return t('atx.stateOff')
    default: return t('atx.stateUnknown')
  }
})

function requestAction(action: UiPowerAction) {
  pendingAction.value = action
  confirmDialogOpen.value = true
}

async function handleAction() {
  if (!pendingAction.value || powerActionPending.value) return
  if (ownerId === null || ownerId !== currentBmcIdFromLocation()) {
    confirmDialogOpen.value = false
    pendingAction.value = null
    return
  }
  const action = pendingAction.value
  powerActionPending.value = true
  try {
    await powerStatusResource.mutate(mutationId => powerApi.power(action, mutationId))
    if (ownerId !== currentBmcIdFromLocation()) return
    confirmDialogOpen.value = false
    pendingAction.value = null
    toast.success(t('atx.powerCommandAccepted'))
  } catch {
    // request() presents the typed backend fault; keep the dialog open for retry.
  } finally {
    powerActionPending.value = false
  }
}

function handleConfirmDialogOpenChange(open: boolean) {
  if (!open && powerActionPending.value) return
  confirmDialogOpen.value = open
  if (!open) pendingAction.value = null
}

function cancelAction() {
  if (powerActionPending.value) return
  confirmDialogOpen.value = false
  pendingAction.value = null
}

const confirmTitle = computed(() => {
  switch (pendingAction.value) {
    case 'on': return t('atx.confirmPowerOnTitle')
    case 'off': return t('atx.confirmLongTitle')
    case 'reset': return t('atx.confirmResetTitle')
    case 'graceful_shutdown': return t('atx.confirmGracefulShutdownTitle')
    case 'graceful_restart': return t('atx.confirmGracefulResetTitle')
    default: return ''
  }
})

const confirmDescription = computed(() => {
  switch (pendingAction.value) {
    case 'on': return t('atx.confirmPowerOnDesc')
    case 'off': return t('atx.confirmLongDesc')
    case 'reset': return t('atx.confirmResetDesc')
    case 'graceful_shutdown': return t('atx.confirmGracefulShutdownDesc')
    case 'graceful_restart': return t('atx.confirmGracefulResetDesc')
    default: return ''
  }
})
const isValidMac = computed(() => {
  const mac = wolMacAddress.value.trim()
  const macRegex = /^([0-9A-Fa-f]{2}[:-]){5}([0-9A-Fa-f]{2})$|^([0-9A-Fa-f]{12})$/
  return macRegex.test(mac)
})

function sendWol() {
  if (!isValidMac.value || ownerId === null || ownerId !== currentBmcIdFromLocation()) return
  wolSending.value = true

  let mac = wolMacAddress.value.trim().toUpperCase()
  if (mac.length === 12) {
    mac = mac.match(/.{2}/g)!.join(':')
  } else {
    mac = mac.replace(/-/g, ':')
  }

  emit('wol', mac)

  wolHistory.value = [mac, ...wolHistory.value.filter(item => item !== mac)].slice(0, 5)
  setTimeout(() => {
    loadWolHistory().catch(() => {})
  }, 1200)

  setTimeout(() => {
    wolSending.value = false
  }, 1000)
}

function selectFromHistory(mac: string) {
  wolMacAddress.value = mac
}

async function loadWolHistory() {
  wolLoadingHistory.value = true
  try {
    const response = await powerConfigApi.getWolHistory(5)
    wolHistory.value = response.history.map(item => item.mac_address)
  } catch {
    wolHistory.value = []
  } finally {
    wolLoadingHistory.value = false
  }
}

function toggleWol() {
  wolExpanded.value = !wolExpanded.value
  if (wolExpanded.value) loadWolHistory().catch(() => {})
}
</script>

<template>
  <div class="p-3 space-y-3">
    <div class="flex items-center justify-between rounded-md bg-muted/50 p-2">
      <span class="text-xs text-muted-foreground">{{ t('atx.powerState') }}</span>
      <Badge variant="outline" class="gap-1.5 text-xs">
        <span :class="['h-2 w-2 rounded-full', powerStateColor]" />
        {{ powerStateText }}
      </Badge>
    </div>

    <div class="space-y-1.5">
          <Button
            v-if="supportedActions.includes('on')"
            variant="outline"
            size="sm"
            class="w-full justify-start gap-2 h-8 text-xs"
            :disabled="powerState === 'on'"
            @click="requestAction('on')"
          >
            <Power class="h-3.5 w-3.5" />
            {{ t('atx.powerOn') }}
          </Button>

          <Button
            v-if="supportedActions.includes('off')"
            variant="outline"
            size="sm"
            class="w-full justify-start gap-2 h-8 text-xs text-orange-600 hover:text-orange-700 hover:bg-orange-50 dark:hover:bg-orange-950"
            :disabled="powerState === 'off'"
            @click="requestAction('off')"
          >
            <CircleDot class="h-3.5 w-3.5" />
            {{ t('atx.forcePowerOff') }}
          </Button>

          <Button
            v-if="supportedActions.includes('reset')"
            variant="outline"
            size="sm"
            class="w-full justify-start gap-2 h-8 text-xs text-red-600 hover:text-red-700 hover:bg-red-50 dark:hover:bg-red-950"
            @click="requestAction('reset')"
          >
            <RotateCcw class="h-3.5 w-3.5" />
            {{ t('atx.reset') }}
          </Button>

          <Button
            v-if="supportedActions.includes('graceful_shutdown')"
            variant="outline"
            size="sm"
            class="w-full justify-start gap-2 h-8 text-xs text-blue-600 hover:text-blue-700 hover:bg-blue-50 dark:hover:bg-blue-950"
            @click="requestAction('graceful_shutdown')"
          >
            <Power class="h-3.5 w-3.5" />
            {{ t('atx.gracefulShutdown') }}
          </Button>

          <Button
            v-if="supportedActions.includes('graceful_restart')"
            variant="outline"
            size="sm"
            class="w-full justify-start gap-2 h-8 text-xs text-blue-600 hover:text-blue-700 hover:bg-blue-50 dark:hover:bg-blue-950"
            @click="requestAction('graceful_restart')"
          >
            <RotateCcw class="h-3.5 w-3.5" />
            {{ t('atx.gracefulReset') }}
          </Button>
    </div>

    <Separator />
    <Button variant="ghost" size="sm" class="h-8 w-full justify-between px-2 text-xs" @click="toggleWol">
      <span class="flex items-center gap-2"><Wifi class="h-3.5 w-3.5" />{{ t('atx.wol') }}</span>
      <ChevronDown class="h-3.5 w-3.5 transition-transform" :class="wolExpanded && 'rotate-180'" />
    </Button>
    <div v-if="wolExpanded" class="space-y-3">
        <div class="space-y-2">
          <Label for="mac-address" class="text-xs">{{ t('atx.macAddress') }}</Label>
          <div class="flex gap-2">
            <Input
              id="mac-address"
              v-model="wolMacAddress"
              placeholder="AA:BB:CC:DD:EE:FF"
              class="h-8 text-xs font-mono"
              @keyup.enter="sendWol"
            />
            <Button
              size="sm"
              class="h-8 px-3"
              :disabled="!isValidMac || wolSending"
              @click="sendWol"
            >
              <Send class="h-3.5 w-3.5" />
            </Button>
          </div>
          <p v-if="wolMacAddress && !isValidMac" class="text-xs text-destructive">
            {{ t('atx.invalidMac') }}
          </p>
        </div>

        <p v-if="wolLoadingHistory" class="text-xs text-muted-foreground">
          {{ t('common.loading') }}
        </p>

        <div v-if="wolHistory.length > 0" class="space-y-2">
          <Separator />
          <Label class="text-xs text-muted-foreground">{{ t('atx.recentMac') }}</Label>
          <div class="space-y-1">
            <button
              v-for="mac in wolHistory"
              :key="mac"
              class="w-full text-left px-2 py-1.5 rounded text-xs font-mono hover:bg-muted transition-colors"
              @click="selectFromHistory(mac)"
            >
              {{ mac }}
            </button>
          </div>
        </div>
    </div>
  </div>

  <!-- Confirm Dialog -->
  <AlertDialog :open="confirmDialogOpen" @update:open="handleConfirmDialogOpenChange">
    <AlertDialogContent>
      <AlertDialogHeader>
        <AlertDialogTitle>{{ confirmTitle }}</AlertDialogTitle>
        <AlertDialogDescription>{{ confirmDescription }}</AlertDialogDescription>
      </AlertDialogHeader>
      <AlertDialogFooter>
        <Button type="button" variant="outline" :disabled="powerActionPending" @click="cancelAction">
          {{ t('common.cancel') }}
        </Button>
        <Button type="button" :disabled="powerActionPending" :aria-busy="powerActionPending" @click="handleAction">
          <Loader2 v-if="powerActionPending" class="mr-2 h-4 w-4 animate-spin" />
          {{ powerActionPending ? t('common.loading') : t('common.confirm') }}
        </Button>
      </AlertDialogFooter>
    </AlertDialogContent>
  </AlertDialog>
</template>
