<script setup lang="ts">
import { ref, computed, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { toast } from 'vue-sonner'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'
import { Slider } from '@/components/ui/slider'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { MousePointer, Move } from 'lucide-vue-next'
import HelpTooltip from '@/components/HelpTooltip.vue'
import { useHidConfigStore } from '@/stores/hidConfig'

const props = defineProps<{
  open: boolean
  mouseMode?: 'absolute' | 'relative'
}>()

const emit = defineEmits<{
  (e: 'update:open', value: boolean): void
  (e: 'update:mouseMode', value: 'absolute' | 'relative'): void
}>()

const { t } = useI18n()
const hidConfigStore = useHidConfigStore()

const DEFAULT_MOUSE_MOVE_SEND_INTERVAL_MS = 16

function clampMouseMoveSendIntervalMs(ms: number): number {
  if (!Number.isFinite(ms)) return DEFAULT_MOUSE_MOVE_SEND_INTERVAL_MS
  return Math.max(0, Math.min(1000, Math.floor(ms)))
}

function loadMouseMoveSendIntervalFromStorage(): number {
  const raw = localStorage.getItem('hidMouseThrottle')
  const parsed = raw === null ? NaN : Number(raw)
  return clampMouseMoveSendIntervalMs(
    Number.isFinite(parsed) ? parsed : DEFAULT_MOUSE_MOVE_SEND_INTERVAL_MS
  )
}

const mouseThrottle = ref<number>(
  loadMouseMoveSendIntervalFromStorage()
)
const showCursor = ref<boolean>(
  localStorage.getItem('hidShowCursor') !== 'false' // default true
)

watch(showCursor, (newValue, oldValue) => {
  if (newValue !== oldValue) {
    localStorage.setItem('hidShowCursor', newValue ? 'true' : 'false')
    window.dispatchEvent(new CustomEvent('hidCursorVisibilityChanged', {
      detail: { visible: newValue }
    }))
  }
})

const buttonText = computed(() => t('actionbar.hidConfig'))

function initializeFromCurrent() {
  mouseThrottle.value = loadMouseMoveSendIntervalFromStorage()

  const storedCursor = localStorage.getItem('hidShowCursor') !== 'false'
  showCursor.value = storedCursor

}

function toggleMouseMode() {
  const newMode = props.mouseMode === 'absolute' ? 'relative' : 'absolute'
  emit('update:mouseMode', newMode)

  // Update backend config
  hidConfigStore.updateHid({
    mouse_absolute: newMode === 'absolute',
  }).catch(_e => {
    console.info('[HidConfig] Failed to update mouse mode')
    toast.error(t('config.updateFailed'))
  })
}

function handleThrottleChange(value: number[] | undefined) {
  if (!value || value.length === 0 || value[0] === undefined) return
  const throttleValue = clampMouseMoveSendIntervalMs(value[0])
  mouseThrottle.value = throttleValue
  localStorage.setItem('hidMouseThrottle', String(throttleValue))
  window.dispatchEvent(new CustomEvent('hidMouseSendIntervalChanged', {
    detail: { intervalMs: throttleValue },
  }))
}

watch(() => props.open, (isOpen) => {
  if (!isOpen) return

  hidConfigStore.refreshHid()
    .then(() => {
      initializeFromCurrent()
    })
    .catch(() => {
      initializeFromCurrent()
    })
})
</script>

<template>
  <Popover :open="open" @update:open="emit('update:open', $event)">
    <PopoverTrigger as-child>
      <Button variant="ghost" size="sm" class="h-7 w-7 sm:h-8 sm:w-auto p-0 sm:px-2 sm:gap-1.5 text-xs">
        <MousePointer v-if="mouseMode === 'absolute'" class="h-3.5 w-3.5 sm:h-4 sm:w-4" />
        <Move v-else class="h-3.5 w-3.5 sm:h-4 sm:w-4" />
        <span class="hidden sm:inline">{{ buttonText }}</span>
      </Button>
    </PopoverTrigger>
    <PopoverContent class="w-[min(320px,92vw)] p-3" align="start">
      <div class="space-y-3">
        <h4 class="text-sm font-medium">{{ t('actionbar.hidConfig') }}</h4>

        <Separator />

        <!-- Mouse Settings (Real-time) -->
        <div class="space-y-3">
          <h5 class="text-xs font-medium text-muted-foreground">{{ t('actionbar.mouseSettings') }}</h5>

          <!-- Positioning Mode -->
          <div class="space-y-2">
            <div class="flex items-center gap-1">
              <Label class="text-xs text-muted-foreground">{{ t('actionbar.positioningMode') }}</Label>
              <HelpTooltip :content="mouseMode === 'absolute' ? t('help.absoluteMode') : t('help.relativeMode')" icon-size="sm" />
            </div>
            <div class="flex gap-2">
              <Button
                :variant="mouseMode === 'absolute' ? 'default' : 'outline'"
                size="sm"
                class="flex-1 h-8 text-xs"
                @click="toggleMouseMode"
              >
                <MousePointer class="h-3.5 w-3.5 mr-1" />
                {{ t('actionbar.absolute') }}
              </Button>
              <Button
                :variant="mouseMode === 'relative' ? 'default' : 'outline'"
                size="sm"
                class="flex-1 h-8 text-xs"
                @click="toggleMouseMode"
              >
                <Move class="h-3.5 w-3.5 mr-1" />
                {{ t('actionbar.relative') }}
              </Button>
            </div>
          </div>

          <!-- Event Throttle -->
          <div class="space-y-2">
            <div class="flex justify-between items-center">
              <div class="flex items-center gap-1">
                <Label class="text-xs text-muted-foreground">{{ t('actionbar.sendInterval') }}</Label>
                <HelpTooltip :content="t('help.mouseThrottle')" icon-size="sm" />
              </div>
              <span class="text-xs font-mono">{{ mouseThrottle }}ms</span>
            </div>
            <Slider
              :model-value="[mouseThrottle]"
              @update:model-value="handleThrottleChange"
              :min="0"
              :max="1000"
              :step="1"
              class="py-2"
            />
            <div class="flex justify-between text-xs text-muted-foreground">
              <span>0ms</span>
              <span>1000ms</span>
            </div>
          </div>

          <!-- Show Cursor -->
          <div class="flex items-center justify-between">
            <Label class="text-xs text-muted-foreground">{{ t('actionbar.showCursor') }}</Label>
            <Switch v-model="showCursor" />
          </div>
        </div>

      </div>
    </PopoverContent>
  </Popover>
</template>
