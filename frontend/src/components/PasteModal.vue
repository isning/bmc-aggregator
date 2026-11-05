<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import { Label } from '@/components/ui/label'
import { Progress } from '@/components/ui/progress'
import { CornerDownLeft, Square, AlertCircle } from 'lucide-vue-next'
import { charToKey, analyzeText } from '@/lib/charToHid'
import { hidApi } from '@/api'
import { canonicalKeyToHidUsage, keys } from '@/lib/keyboardMappings'

const emit = defineEmits<{
  (e: 'close'): void
}>()

const { t } = useI18n()

const text = ref('')
const textareaRef = ref<InstanceType<typeof Textarea> | null>(null)
const isPasting = ref(false)
const progress = ref(0)
const currentChar = ref(0)
const totalChars = ref(0)
const typingDelay = ref(10)

const textAnalysis = computed(() => {
  if (!text.value) return null
  return analyzeText(text.value)
})

const hasUntypableChars = computed(() => {
  return textAnalysis.value && textAnalysis.value.untypable > 0
})

onMounted(() => {
  setTimeout(() => {
    textareaRef.value?.focus()
  }, 100)
})

onUnmounted(() => {
  void resetHidSilently()
})

/**
 * Sleep utility function
 */
function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms))
}

async function resetHidSilently() {
  try {
    await hidApi.reset({ toastOnError: false })
  } catch {
  }
}

/**
 * Main paste function - builds a batch of HID events and sends via batch endpoint
 */
async function handlePaste() {
  const textToType = text.value
  if (!textToType.trim()) return

  isPasting.value = true
  progress.value = 0
  currentChar.value = 0
  totalChars.value = textToType.length

  try {
    const chars = [...textToType]
    const totalLength = chars.length
    const events: Array<{ action: string; params: Record<string, unknown> }> = []

    for (let i = 0; i < totalLength; i++) {
      const char = chars[i]
      if (char === undefined) continue

      if (char === '\r' && i + 1 < totalLength && chars[i + 1] === '\n') {
        continue
      }

      const mapping = charToKey(char)
      if (mapping) {
        const scancode = canonicalKeyToHidUsage(mapping.key)
        const shiftScancode = canonicalKeyToHidUsage(keys.ShiftLeft)
        if (mapping.shift) {
          events.push({ action: 'keyboard', params: { scancode: shiftScancode, pressed: true } })
        }
        events.push(
          { action: 'keyboard', params: { scancode, pressed: true } },
          { action: 'keyboard', params: { scancode, pressed: false } },
        )
        if (mapping.shift) {
          events.push({ action: 'keyboard', params: { scancode: shiftScancode, pressed: false } })
        }
      }
    }

    progress.value = 50
    currentChar.value = Math.round(totalLength / 2)
    await hidApi.batch(events, typingDelay.value)
    progress.value = 100
    currentChar.value = totalLength

    await sleep(200)
    text.value = ''
    emit('close')
  } catch (error) {
    console.error('[Paste] Error during paste operation:', error)
  } finally {
    await resetHidSilently()
    isPasting.value = false
    progress.value = 0
    currentChar.value = 0
    totalChars.value = 0
  }
}

function handleKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
    e.preventDefault()
    if (!isPasting.value) {
      handlePaste()
    }
  }
  if (e.key === 'Escape') {
    e.preventDefault()
    if (isPasting.value) {
      void resetHidSilently()
    } else {
      emit('close')
    }
  }
  e.stopPropagation()
}
</script>

<template>
  <div class="p-4 space-y-4">
    <div class="space-y-1">
      <h3 class="font-semibold text-sm">{{ t('paste.title') }}</h3>
      <p class="text-xs text-muted-foreground">{{ t('paste.description') }}</p>
    </div>

    <div class="space-y-2">
      <Label for="paste-text">{{ t('paste.label') }}</Label>
      <Textarea
        id="paste-text"
        ref="textareaRef"
        v-model="text"
        :placeholder="t('paste.placeholder')"
        class="min-h-[120px] resize-none font-mono text-sm"
        :disabled="isPasting"
        @keydown="handleKeydown"
        @keyup.stop
      />
    </div>

    <!-- Warning for untypable characters -->
    <div v-if="hasUntypableChars && !isPasting" class="flex items-start gap-2 p-2 rounded-md bg-amber-500/10 text-amber-600 dark:text-amber-400">
      <AlertCircle class="h-4 w-4 shrink-0 mt-0.5" />
      <div class="text-xs">
        <p class="font-medium">{{ t('paste.untypableWarning') }}</p>
        <p class="text-muted-foreground mt-0.5">
          {{ t('paste.untypableChars', { chars: textAnalysis?.untypableChars.slice(0, 5).map(c => c === '\n' ? '\\n' : c === '\r' ? '\\r' : c === '\t' ? '\\t' : c).join(', ') }) }}
          <span v-if="textAnalysis && textAnalysis.untypableChars.length > 5">...</span>
        </p>
      </div>
    </div>

    <!-- Progress indicator during paste -->
    <div v-if="isPasting" class="space-y-2">
      <div class="flex items-center justify-between text-xs text-muted-foreground">
        <span>{{ t('paste.typing') }}</span>
        <span>{{ currentChar }} / {{ totalChars }}</span>
      </div>
      <Progress :model-value="progress" class="h-2" />
    </div>

    <div class="flex items-center justify-between">
      <p v-if="!isPasting" class="text-xs text-muted-foreground">
        {{ t('paste.hint') }}
      </p>
      <p v-else class="text-xs text-muted-foreground">
        {{ t('paste.escToCancel') }}
      </p>
      <div class="flex gap-2">
        <Button v-if="!isPasting" variant="ghost" size="sm" @click="emit('close')">
          {{ t('common.cancel') }}
        </Button>
        <Button v-else variant="ghost" size="sm" @click="resetHidSilently">
          <Square class="h-3 w-3 mr-1.5 fill-current" />
          {{ t('paste.stop') }}
        </Button>
        <Button
          size="sm"
          :disabled="!text.trim() || isPasting"
          @click="handlePaste"
        >
          <CornerDownLeft class="h-4 w-4 mr-1.5" />
          {{ t('paste.confirm') }}
        </Button>
      </div>
    </div>
  </div>
</template>
