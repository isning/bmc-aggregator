<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted, onDeactivated, nextTick } from 'vue'
import { useI18n } from 'vue-i18n'
import Keyboard from 'simple-keyboard'
import 'simple-keyboard/build/css/index.css'
import {
  Keyboard as KeyboardIcon,
  PanelBottomClose,
  PictureInPicture2,
  X,
} from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import DraggableWindowFrame from '@/components/DraggableWindowFrame.vue'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useHidEventBus } from '@/features/hid'
import type { WindowDimensions, WindowPosition } from '@/lib/floatingWindowGeometry'
import { CanonicalKey } from '@/types/generated'
import {
  keys,
  consumerKeys,
  latchingKeys,
  modifiers,
  type KeyName,
  type ConsumerKeyName,
} from '@/lib/keyboardMappings'
import {
  type KeyboardOsType,
  osBottomRows,
  mediaKeys,
  mediaKeyLabels,
} from '@/lib/keyboardLayouts'

const props = defineProps<{
  visible: boolean
  attached?: boolean
  capsLock?: boolean
  pressedKeys?: CanonicalKey[]
  consumerEnabled?: boolean
  captureKeyboard: () => {
    current: () => boolean
    send: (key: CanonicalKey, press: boolean) => Promise<boolean>
  }
}>()

const emit = defineEmits<{
  (e: 'update:visible', value: boolean): void
  (e: 'update:attached', value: boolean): void
}>()

const hidBus = useHidEventBus()
const { t } = useI18n()

const isAttached = ref(props.attached ?? true)
const selectedOs = ref<KeyboardOsType>('windows')

const mainKeyboard = ref<Keyboard | null>(null)

const pressedModifiers = ref<number>(0)
const keysDown = ref<CanonicalKey[]>([])

const isShiftActive = computed(() => {
  return (pressedModifiers.value & 0x22) !== 0
})

const areLettersUppercase = computed(() => {
  return Boolean(props.capsLock) !== isShiftActive.value
})

const layoutName = computed(() => {
  return isShiftActive.value ? 'shift' : 'default'
})

const keyNamesForDownKeys = computed(() => {
  const activeModifierMask = pressedModifiers.value || 0
  const modifierNames = Object.entries(modifiers)
    .filter(([_, mask]) => (activeModifierMask & mask) !== 0)
    .map(([name]) => name)

  return Array.from(new Set([
    ...modifierNames,
    ...(props.pressedKeys ?? []),
    ...keysDown.value,
    ...(props.capsLock ? ['CapsLock'] : []),
  ]))
})

const position = ref<WindowPosition>({ x: 100, y: 100 })
const viewportSize = ref<WindowDimensions>({ width: 1, height: 1 })

const keyboardId = ref(`kb-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`)

const getBottomRow = () =>
  [...osBottomRows[selectedOs.value], 'ArrowLeft', 'ArrowDown', 'ArrowRight'].join(' ')

const keyboardLayout = {
  main: {
    default: [
      'CtrlAltDelete AltMetaEscape CtrlAltBackspace',
      'Escape F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12 PrintScreen ScrollLock Pause',
      'Backquote Digit1 Digit2 Digit3 Digit4 Digit5 Digit6 Digit7 Digit8 Digit9 Digit0 Minus Equal Backspace Insert Home PageUp',
      'Tab KeyQ KeyW KeyE KeyR KeyT KeyY KeyU KeyI KeyO KeyP BracketLeft BracketRight Backslash Delete End PageDown',
      'CapsLock KeyA KeyS KeyD KeyF KeyG KeyH KeyJ KeyK KeyL Semicolon Quote Enter',
      'ShiftLeft KeyZ KeyX KeyC KeyV KeyB KeyN KeyM Comma Period Slash ShiftRight ArrowUp',
      'ControlLeft MetaLeft AltLeft Space AltRight MetaRight ContextMenu ControlRight ArrowLeft ArrowDown ArrowRight',
    ],
    shift: [
      'CtrlAltDelete AltMetaEscape CtrlAltBackspace',
      'Escape F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12 PrintScreen ScrollLock Pause',
      '(Backquote) (Digit1) (Digit2) (Digit3) (Digit4) (Digit5) (Digit6) (Digit7) (Digit8) (Digit9) (Digit0) (Minus) (Equal) Backspace Insert Home PageUp',
      'Tab (KeyQ) (KeyW) (KeyE) (KeyR) (KeyT) (KeyY) (KeyU) (KeyI) (KeyO) (KeyP) (BracketLeft) (BracketRight) (Backslash) Delete End PageDown',
      'CapsLock (KeyA) (KeyS) (KeyD) (KeyF) (KeyG) (KeyH) (KeyJ) (KeyK) (KeyL) (Semicolon) (Quote) Enter',
      'ShiftLeft (KeyZ) (KeyX) (KeyC) (KeyV) (KeyB) (KeyN) (KeyM) (Comma) (Period) (Slash) ShiftRight ArrowUp',
      'ControlLeft MetaLeft AltLeft Space AltRight MetaRight ContextMenu ControlRight ArrowLeft ArrowDown ArrowRight',
    ],
  },
}

const compactMainLayout = {
  default: [
    'Escape Insert Delete Home End PageUp PageDown PrintScreen',
    'Backquote Digit1 Digit2 Digit3 Digit4 Digit5 Digit6 Digit7 Digit8 Digit9 Digit0 Minus Equal Backspace',
    'Tab KeyQ KeyW KeyE KeyR KeyT KeyY KeyU KeyI KeyO KeyP BracketLeft BracketRight Backslash',
    'CapsLock KeyA KeyS KeyD KeyF KeyG KeyH KeyJ KeyK KeyL Semicolon Quote Enter',
    'ShiftLeft KeyZ KeyX KeyC KeyV KeyB KeyN KeyM Comma Period Slash ShiftRight ArrowUp',
    'ControlLeft MetaLeft AltLeft Space AltRight MetaRight ContextMenu ControlRight ArrowLeft ArrowDown ArrowRight',
  ],
  shift: [
    'Escape Insert Delete Home End PageUp PageDown PrintScreen',
    '(Backquote) (Digit1) (Digit2) (Digit3) (Digit4) (Digit5) (Digit6) (Digit7) (Digit8) (Digit9) (Digit0) (Minus) (Equal) Backspace',
    'Tab (KeyQ) (KeyW) (KeyE) (KeyR) (KeyT) (KeyY) (KeyU) (KeyI) (KeyO) (KeyP) (BracketLeft) (BracketRight) (Backslash)',
    'CapsLock (KeyA) (KeyS) (KeyD) (KeyF) (KeyG) (KeyH) (KeyJ) (KeyK) (KeyL) (Semicolon) (Quote) Enter',
    'ShiftLeft (KeyZ) (KeyX) (KeyC) (KeyV) (KeyB) (KeyN) (KeyM) (Comma) (Period) (Slash) ShiftRight ArrowUp',
    'ControlLeft MetaLeft AltLeft Space AltRight MetaRight ContextMenu ControlRight ArrowLeft ArrowDown ArrowRight',
  ],
}

const isCompactLayout = ref(false)
let compactLayoutMedia: MediaQueryList | null = null
let compactLayoutListener: ((event: MediaQueryListEvent) => void) | null = null
let viewportResizeObserver: ResizeObserver | null = null

function setCompactLayout(active: boolean) {
  if (isCompactLayout.value === active) return
  isCompactLayout.value = active
  updateKeyboardLayout()
}

const keyDisplayMap = computed<Record<string, string>>(() => {
  const metaLabel = selectedOs.value === 'windows' ? '⊞ Win'
    : selectedOs.value === 'mac' ? '⌘ Cmd' : 'Meta'

  return {
    CtrlAltDelete: 'Ctrl+Alt+Del',
    AltMetaEscape: 'Alt+Meta+Esc',
    CtrlAltBackspace: 'Ctrl+Alt+Bksp',

    ControlLeft: 'Ctrl',
    ControlRight: 'Ctrl',
    ShiftLeft: 'Shift',
    ShiftRight: 'Shift',
    AltLeft: 'Alt',
    AltRight: 'AltGr',
    MetaLeft: metaLabel,
    MetaRight: metaLabel,
    ContextMenu: 'Menu',

    Escape: 'Esc',
    Backspace: '⌫',
    Tab: 'Tab',
    CapsLock: 'Caps',
    Enter: 'Enter',
    Space: ' ',

    Insert: 'Ins',
    Delete: 'Del',
    Home: 'Home',
    End: 'End',
    PageUp: 'PgUp',
    PageDown: 'PgDn',

    ArrowUp: '↑',
    ArrowDown: '↓',
    ArrowLeft: '←',
    ArrowRight: '→',

    PrintScreen: 'PrtSc',
    ScrollLock: 'ScrLk',
    Pause: 'Pause',

    F1: 'F1', F2: 'F2', F3: 'F3', F4: 'F4',
    F5: 'F5', F6: 'F6', F7: 'F7', F8: 'F8',
    F9: 'F9', F10: 'F10', F11: 'F11', F12: 'F12',

    KeyA: areLettersUppercase.value ? 'A' : 'a',
    KeyB: areLettersUppercase.value ? 'B' : 'b',
    KeyC: areLettersUppercase.value ? 'C' : 'c',
    KeyD: areLettersUppercase.value ? 'D' : 'd',
    KeyE: areLettersUppercase.value ? 'E' : 'e',
    KeyF: areLettersUppercase.value ? 'F' : 'f',
    KeyG: areLettersUppercase.value ? 'G' : 'g',
    KeyH: areLettersUppercase.value ? 'H' : 'h',
    KeyI: areLettersUppercase.value ? 'I' : 'i',
    KeyJ: areLettersUppercase.value ? 'J' : 'j',
    KeyK: areLettersUppercase.value ? 'K' : 'k',
    KeyL: areLettersUppercase.value ? 'L' : 'l',
    KeyM: areLettersUppercase.value ? 'M' : 'm',
    KeyN: areLettersUppercase.value ? 'N' : 'n',
    KeyO: areLettersUppercase.value ? 'O' : 'o',
    KeyP: areLettersUppercase.value ? 'P' : 'p',
    KeyQ: areLettersUppercase.value ? 'Q' : 'q',
    KeyR: areLettersUppercase.value ? 'R' : 'r',
    KeyS: areLettersUppercase.value ? 'S' : 's',
    KeyT: areLettersUppercase.value ? 'T' : 't',
    KeyU: areLettersUppercase.value ? 'U' : 'u',
    KeyV: areLettersUppercase.value ? 'V' : 'v',
    KeyW: areLettersUppercase.value ? 'W' : 'w',
    KeyX: areLettersUppercase.value ? 'X' : 'x',
    KeyY: areLettersUppercase.value ? 'Y' : 'y',
    KeyZ: areLettersUppercase.value ? 'Z' : 'z',

    '(KeyA)': areLettersUppercase.value ? 'A' : 'a',
    '(KeyB)': areLettersUppercase.value ? 'B' : 'b',
    '(KeyC)': areLettersUppercase.value ? 'C' : 'c',
    '(KeyD)': areLettersUppercase.value ? 'D' : 'd',
    '(KeyE)': areLettersUppercase.value ? 'E' : 'e',
    '(KeyF)': areLettersUppercase.value ? 'F' : 'f',
    '(KeyG)': areLettersUppercase.value ? 'G' : 'g',
    '(KeyH)': areLettersUppercase.value ? 'H' : 'h',
    '(KeyI)': areLettersUppercase.value ? 'I' : 'i',
    '(KeyJ)': areLettersUppercase.value ? 'J' : 'j',
    '(KeyK)': areLettersUppercase.value ? 'K' : 'k',
    '(KeyL)': areLettersUppercase.value ? 'L' : 'l',
    '(KeyM)': areLettersUppercase.value ? 'M' : 'm',
    '(KeyN)': areLettersUppercase.value ? 'N' : 'n',
    '(KeyO)': areLettersUppercase.value ? 'O' : 'o',
    '(KeyP)': areLettersUppercase.value ? 'P' : 'p',
    '(KeyQ)': areLettersUppercase.value ? 'Q' : 'q',
    '(KeyR)': areLettersUppercase.value ? 'R' : 'r',
    '(KeyS)': areLettersUppercase.value ? 'S' : 's',
    '(KeyT)': areLettersUppercase.value ? 'T' : 't',
    '(KeyU)': areLettersUppercase.value ? 'U' : 'u',
    '(KeyV)': areLettersUppercase.value ? 'V' : 'v',
    '(KeyW)': areLettersUppercase.value ? 'W' : 'w',
    '(KeyX)': areLettersUppercase.value ? 'X' : 'x',
    '(KeyY)': areLettersUppercase.value ? 'Y' : 'y',
    '(KeyZ)': areLettersUppercase.value ? 'Z' : 'z',

    Digit1: '1', Digit2: '2', Digit3: '3', Digit4: '4', Digit5: '5',
    Digit6: '6', Digit7: '7', Digit8: '8', Digit9: '9', Digit0: '0',

    '(Digit1)': '!', '(Digit2)': '@', '(Digit3)': '#', '(Digit4)': '$', '(Digit5)': '%',
    '(Digit6)': '^', '(Digit7)': '&', '(Digit8)': '*', '(Digit9)': '(', '(Digit0)': ')',

    Minus: '-', '(Minus)': '_',
    Equal: '=', '(Equal)': '+',
    BracketLeft: '[', '(BracketLeft)': '{',
    BracketRight: ']', '(BracketRight)': '}',
    Backslash: '\\', '(Backslash)': '|',
    Semicolon: ';', '(Semicolon)': ':',
    Quote: "'", '(Quote)': '"',
    Comma: ',', '(Comma)': '<',
    Period: '.', '(Period)': '>',
    Slash: '/', '(Slash)': '?',
    Backquote: '`', '(Backquote)': '~',
  }
})

async function onMediaKeyPress(key: string) {
  if (key in consumerKeys) {
    const usage = consumerKeys[key as ConsumerKeyName]
    try {
      await hidBus.sendConsumer({ usage })
    } catch (err) {
      console.error('[VirtualKeyboard] Media key send failed:', err)
    }
  }
}

function switchOsLayout(os: unknown) {
  if (os !== 'windows' && os !== 'mac' && os !== 'android') return
  selectedOs.value = os
  localStorage.setItem('vkb-os-layout', os)
  updateKeyboardLayout()
}

function updateKeyboardLayout() {
  const bottomRow = getBottomRow()
  const baseLayout = isCompactLayout.value ? compactMainLayout : keyboardLayout.main
  const newLayout = {
    ...baseLayout,
    default: [
      ...baseLayout.default.slice(0, -1),
      bottomRow,
    ],
    shift: [
      ...baseLayout.shift.slice(0, -1),
      bottomRow,
    ],
  }
  mainKeyboard.value?.setOptions({ layout: newLayout, display: keyDisplayMap.value })
  updateKeyboardButtonTheme()
}

async function onKeyDown(key: string) {
  const keyboard = props.captureKeyboard()
  const sendKeyPress = keyboard.send
  if (!keyboard.current()) return
  if (key === 'CtrlAltDelete') {
    await executeMacro([
      { keys: ['Delete'], modifiers: ['ControlLeft', 'AltLeft'] },
    ], keyboard)
    return
  }

  if (key === 'AltMetaEscape') {
    await executeMacro([
      { keys: ['Escape'], modifiers: ['AltLeft', 'MetaLeft'] },
    ], keyboard)
    return
  }

  if (key === 'CtrlAltBackspace') {
    await executeMacro([
      { keys: ['Backspace'], modifiers: ['ControlLeft', 'AltLeft'] },
    ], keyboard)
    return
  }

  const cleanKey = key.replace(/[()]/g, '')

  if (!(cleanKey in keys)) {
    console.warn(`[VirtualKeyboard] Unknown key: ${cleanKey}`)
    return
  }

  const keyCode = keys[cleanKey as KeyName]

  if (latchingKeys.some(latchingKey => latchingKey === keyCode)) {
    if (!await sendKeyPress(keyCode, true)) return
    setTimeout(() => {
      if (!keyboard.current()) return
      sendKeyPress(keyCode, false)
    }, 100)
    return
  }

  const mask = modifiers[keyCode] ?? 0
  if (mask !== 0) {
    const isCurrentlyDown = (pressedModifiers.value & mask) !== 0

    if (isCurrentlyDown) {
      const nextMask = pressedModifiers.value & ~mask
      pressedModifiers.value = nextMask
      await sendKeyPress(keyCode, false)
    } else {
      const nextMask = pressedModifiers.value | mask
      pressedModifiers.value = nextMask
      await sendKeyPress(keyCode, true)
    }
    updateKeyboardButtonTheme()
    return
  }

  keysDown.value.push(keyCode)
  if (!await sendKeyPress(keyCode, true)) return
  updateKeyboardButtonTheme()
  setTimeout(async () => {
    if (!keyboard.current()) return
    keysDown.value = keysDown.value.filter(k => k !== keyCode)
    await sendKeyPress(keyCode, false)
    updateKeyboardButtonTheme()
  }, 50)
}

async function onKeyUp() {
}

interface MacroStep {
  keys: string[]
  modifiers: string[]
}

async function executeMacro(steps: MacroStep[], keyboard: ReturnType<typeof props.captureKeyboard>) {
  const inherited = new Set(props.pressedKeys ?? [])
  const held = new Set<CanonicalKey>()
  const sendKeyPress = async (key: CanonicalKey, press: boolean) => {
    if (inherited.has(key)) return true
    if (!await keyboard.send(key, press)) return false
    if (press) held.add(key)
    else held.delete(key)
    return true
  }
  try {
    for (const step of steps) {
      if (!keyboard.current()) return
      for (const mod of step.modifiers) {
        if (mod in keys) {
          if (!await sendKeyPress(keys[mod as KeyName], true)) return
        }
      }

      for (const key of step.keys) {
        if (key in keys) {
          if (!await sendKeyPress(keys[key as KeyName], true)) return
        }
      }

      await new Promise(resolve => setTimeout(resolve, 50))
      if (!keyboard.current()) return

      for (const key of step.keys) {
        if (key in keys) {
          if (!await sendKeyPress(keys[key as KeyName], false)) return
        }
      }

      for (const mod of step.modifiers) {
        if (mod in keys) {
          if (!await sendKeyPress(keys[mod as KeyName], false)) return
        }
      }
    }
  } finally {
    for (const key of [...held].reverse()) {
      if (!keyboard.current()) break
      await keyboard.send(key, false)
    }
  }
}

function updateKeyboardButtonTheme() {
  const downKeys = keyNamesForDownKeys.value.join(' ')
  const buttonTheme = [
    {
      class: 'combination-key',
      buttons: 'CtrlAltDelete AltMetaEscape CtrlAltBackspace',
    },
    {
      class: 'down-key',
      buttons: downKeys,
    },
  ]

  mainKeyboard.value?.setOptions({ buttonTheme })
}

watch([layoutName, () => props.capsLock], ([name]) => {
  mainKeyboard.value?.setOptions({
    layoutName: name,
    display: keyDisplayMap.value,
  })
  updateKeyboardButtonTheme()
})

function initKeyboards() {
  const id = keyboardId.value

  const mainEl = document.querySelector(`#${id}-main`)

  if (!mainEl) {
    console.warn('[VirtualKeyboard] DOM elements not ready, retrying...', id)
    setTimeout(initKeyboards, 50)
    return
  }

  mainKeyboard.value = new Keyboard(mainEl, {
    layout: isCompactLayout.value ? compactMainLayout : keyboardLayout.main,
    layoutName: layoutName.value,
    display: keyDisplayMap.value,
    theme: 'hg-theme-default hg-layout-default vkb-keyboard',
    onKeyPress: onKeyDown,
    onKeyReleased: onKeyUp,
    buttonTheme: [
      {
        class: 'combination-key',
        buttons: 'CtrlAltDelete AltMetaEscape CtrlAltBackspace',
      },
    ],
    disableButtonHold: true,
    preventMouseDownDefault: true,
    preventMouseUpDefault: true,
    stopMouseDownPropagation: true,
    stopMouseUpPropagation: true,
  })

  updateKeyboardLayout()
  console.log('[VirtualKeyboard] Keyboards initialized:', id)
}

function destroyKeyboards() {
  mainKeyboard.value?.destroy()
  mainKeyboard.value = null
}

async function toggleAttached() {
  destroyKeyboards()
  isAttached.value = !isAttached.value
  emit('update:attached', isAttached.value)

  await nextTick()
  await nextTick() // Extra tick for Teleport

  setTimeout(() => {
    initKeyboards()
  }, 100)
}

function close() {
  emit('update:visible', false)
}

watch(() => props.visible, async (visible) => {
  console.log('[VirtualKeyboard] Visibility changed:', visible, 'attached:', isAttached.value, 'id:', keyboardId.value)
  if (visible) {
    await nextTick()
    initKeyboards()
  } else {
    destroyKeyboards()
  }
}, { immediate: true })

watch(() => props.attached, (value) => {
  if (value !== undefined) {
    isAttached.value = value
  }
})

onMounted(() => {
  const updateViewportSize = () => {
    viewportSize.value = {
      width: document.documentElement.clientWidth,
      height: document.documentElement.clientHeight,
    }
  }
  updateViewportSize()
  viewportResizeObserver = new ResizeObserver(updateViewportSize)
  viewportResizeObserver.observe(document.documentElement)

  const savedOs = localStorage.getItem('vkb-os-layout') as KeyboardOsType | null
  if (savedOs && ['windows', 'mac', 'android'].includes(savedOs)) {
    selectedOs.value = savedOs
  }

  if (window.matchMedia) {
    compactLayoutMedia = window.matchMedia('(max-width: 640px)')
    setCompactLayout(compactLayoutMedia.matches)
    compactLayoutListener = (event: MediaQueryListEvent) => {
      setCompactLayout(event.matches)
    }
    compactLayoutMedia.addEventListener('change', compactLayoutListener)
  }

})

onDeactivated(() => {
  keysDown.value = []
  pressedModifiers.value = 0
  updateKeyboardButtonTheme()
})

onUnmounted(() => {
  if (compactLayoutMedia && compactLayoutListener) {
    compactLayoutMedia.removeEventListener('change', compactLayoutListener)
  }
  viewportResizeObserver?.disconnect()
  viewportResizeObserver = null
  destroyKeyboards()
})
</script>

<template>
  <Transition name="keyboard-fade">
    <DraggableWindowFrame
      v-if="visible"
      v-model="position"
      :id="keyboardId"
      :workspace-size="viewportSize"
      :window-title="t('virtualKeyboard.title')"
      :positioning="isAttached ? 'relative' : 'fixed'"
      :disabled="isAttached"
      class="vkb"
      :class="{
        'vkb--attached': isAttached,
        'vkb--floating': !isAttached,
      }"
    >
      <template #icon>
        <KeyboardIcon :size="14" />
      </template>
      <template #actions>
        <TooltipProvider :delay-duration="250">
          <ToggleGroup
            type="single"
            variant="outline"
            size="sm"
            :model-value="selectedOs"
            class="h-7 gap-0 rounded-md bg-muted p-0.5"
            @update:model-value="switchOsLayout"
          >
            <ToggleGroupItem
              v-for="os in (['windows', 'mac', 'android'] as KeyboardOsType[])"
              :key="os"
              :value="os"
              class="h-6 border-0 px-2 text-[10px] shadow-none data-[state=on]:bg-background data-[state=on]:shadow-xs"
            >
              {{ os === 'windows' ? 'Win' : os === 'mac' ? 'Mac' : 'Android' }}
            </ToggleGroupItem>
          </ToggleGroup>
          <span class="vkb-header-separator" aria-hidden="true" />
          <Tooltip>
            <TooltipTrigger as-child>
              <Button
                variant="ghost"
                size="icon-sm"
                class="vkb-window-action"
                :aria-label="isAttached ? t('virtualKeyboard.detach') : t('virtualKeyboard.attach')"
                :title="isAttached ? t('virtualKeyboard.detach') : t('virtualKeyboard.attach')"
                @click="toggleAttached"
              >
                <PictureInPicture2 v-if="isAttached" :size="14" />
                <PanelBottomClose v-else :size="14" />
              </Button>
            </TooltipTrigger>
            <TooltipContent side="bottom">
              {{ isAttached ? t('virtualKeyboard.detach') : t('virtualKeyboard.attach') }}
            </TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger as-child>
              <Button
                variant="ghost"
                size="icon-sm"
                class="vkb-window-action"
                :aria-label="t('virtualKeyboard.hide')"
                :title="t('virtualKeyboard.hide')"
                @click="close"
              >
                <X :size="14" />
              </Button>
            </TooltipTrigger>
            <TooltipContent side="bottom">
              {{ t('virtualKeyboard.hide') }}
            </TooltipContent>
          </Tooltip>
        </TooltipProvider>
      </template>

      <!-- Keyboard body -->
      <div class="vkb-body">
        <!-- Media keys row -->
        <div v-if="props.consumerEnabled !== false" class="vkb-media-row">
          <Button
            v-for="key in mediaKeys"
            :key="key"
            variant="outline"
            size="icon-sm"
            class="h-8 min-w-10 text-base"
            @click="onMediaKeyPress(key)"
          >
            {{ mediaKeyLabels[key] || key }}
          </Button>
        </div>
        <div class="vkb-keyboards">
          <div :id="`${keyboardId}-main`" class="kb-main-container"></div>
        </div>
      </div>
    </DraggableWindowFrame>
  </Transition>
</template>

<style>
/* Simple-keyboard global overrides */
.vkb .simple-keyboard.hg-theme-default {
  font-family: inherit;
  background: transparent;
  padding: 0;
}

.vkb .simple-keyboard .hg-button {
  height: 36px;
  border-radius: 6px;
  background: var(--keyboard-button-bg, white);
  color: var(--keyboard-button-color, #1f2937);
  border: 1px solid var(--keyboard-button-border, #e5e7eb);
  border-bottom-width: 2px;
  box-shadow: 0 1px 2px 0 rgb(0 0 0 / 0.05);
  font-size: 12px;
  font-weight: 500;
  padding: 0 6px;
  margin: 0 2px 4px 0;
  /* Key sizing for alignment */
  flex-grow: 1;
  flex-shrink: 1;
  flex-basis: 0;
  min-width: 40px;
}

.vkb .simple-keyboard .hg-button:hover {
  background: var(--keyboard-button-hover-bg, #f3f4f6);
}

.vkb .simple-keyboard .hg-button:active {
  background: #3b82f6;
  color: white;
  border-bottom-width: 1px;
  margin-top: 1px;
}

.vkb .simple-keyboard .hg-button span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Combination/macro keys */
.vkb .simple-keyboard .hg-button.combination-key {
  font-size: 10px;
  height: 28px;
  min-width: auto !important;
  max-width: fit-content !important;
  flex-grow: 0 !important;
  padding: 0 8px;
}

/* Pressed keys */
.vkb .simple-keyboard .hg-button.down-key {
  background: #3b82f6;
  color: white;
  font-weight: 600;
  border-color: #2563eb;
}

/* Space bar */
.vkb .simple-keyboard .hg-button[data-skbtn="Space"] {
  min-width: 200px;
  flex-grow: 6;
}

/* Row spacing */
.vkb .simple-keyboard .hg-row {
  margin-bottom: 0;
}

.vkb .simple-keyboard .hg-row:last-child {
  margin-bottom: 0;
}

/* First row (macros) - left aligned */
.kb-main-container .hg-row:first-child {
  justify-content: flex-start !important;
  margin-bottom: 8px;
  gap: 4px;
}

/* Second row (function keys) spacing */
.kb-main-container .hg-row:nth-child(2) {
  margin-bottom: 8px;
}

/* Backspace - wider */
.vkb .simple-keyboard .hg-button[data-skbtn="Backspace"] {
  flex-grow: 2;
  min-width: 80px;
}

/* Tab key */
.vkb .simple-keyboard .hg-button[data-skbtn="Tab"] {
  flex-grow: 1.5;
  min-width: 60px;
}

/* Backslash - adjust to match row width */
.vkb .simple-keyboard .hg-button[data-skbtn="Backslash"],
.vkb .simple-keyboard .hg-button[data-skbtn="(Backslash)"] {
  flex-grow: 1.5;
  min-width: 60px;
}

/* Caps Lock */
.vkb .simple-keyboard .hg-button[data-skbtn="CapsLock"] {
  flex-grow: 1.75;
  min-width: 70px;
}

/* Enter key */
.vkb .simple-keyboard .hg-button[data-skbtn="Enter"] {
  flex-grow: 2.25;
  min-width: 90px;
}

/* Left Shift - wider */
.vkb .simple-keyboard .hg-button[data-skbtn="ShiftLeft"] {
  flex-grow: 2.25;
  min-width: 90px;
}

.vkb .simple-keyboard .hg-button[data-skbtn="ShiftRight"] {
  flex-grow: 1.75;
  min-width: 70px;
}

/* Bottom row modifiers */
.vkb .simple-keyboard .hg-button[data-skbtn="ControlLeft"],
.vkb .simple-keyboard .hg-button[data-skbtn="ControlRight"] {
  flex-grow: 1.25;
  min-width: 55px;
}

.vkb .simple-keyboard .hg-button[data-skbtn="MetaLeft"],
.vkb .simple-keyboard .hg-button[data-skbtn="MetaRight"] {
  flex-grow: 1.25;
  min-width: 55px;
}

.vkb .simple-keyboard .hg-button[data-skbtn="AltLeft"] {
  flex-grow: 1.25;
  min-width: 55px;
}

.vkb .simple-keyboard .hg-button[data-skbtn="AltRight"] {
  flex-grow: 1.25;
  min-width: 55px;
}

.vkb .simple-keyboard .hg-button[data-skbtn="ContextMenu"] {
  flex-grow: 1.25;
  min-width: 55px;
}

.vkb .simple-keyboard .hg-button[data-skbtn="PrintScreen"],
.vkb .simple-keyboard .hg-button[data-skbtn="ScrollLock"],
.vkb .simple-keyboard .hg-button[data-skbtn="Pause"],
.vkb .simple-keyboard .hg-button[data-skbtn="Insert"],
.vkb .simple-keyboard .hg-button[data-skbtn="Delete"],
.vkb .simple-keyboard .hg-button[data-skbtn="Home"],
.vkb .simple-keyboard .hg-button[data-skbtn="End"],
.vkb .simple-keyboard .hg-button[data-skbtn="PageUp"],
.vkb .simple-keyboard .hg-button[data-skbtn="PageDown"] {
  font-size: 11px;
  flex-grow: 0.95;
}

.vkb .simple-keyboard .hg-button[data-skbtn="PrintScreen"],
.vkb .simple-keyboard .hg-button[data-skbtn="Insert"],
.vkb .simple-keyboard .hg-button[data-skbtn="Delete"],
.vkb .simple-keyboard .hg-button[data-skbtn="ArrowUp"],
.vkb .simple-keyboard .hg-button[data-skbtn="ArrowLeft"] {
  margin-left: 6px;
}

.vkb .simple-keyboard .hg-button[data-skbtn="ArrowUp"],
.vkb .simple-keyboard .hg-button[data-skbtn="ArrowDown"],
.vkb .simple-keyboard .hg-button[data-skbtn="ArrowLeft"],
.vkb .simple-keyboard .hg-button[data-skbtn="ArrowRight"] {
  font-size: 14px;
  flex-grow: 1;
}

/* Dark mode - must be after simple-keyboard CSS import */
/* Use multiple selectors to ensure matching */
:root.dark .hg-theme-default .hg-button,
html.dark .hg-theme-default .hg-button,
.dark .hg-theme-default .hg-button {
  background: #374151 !important;
  color: #f9fafb !important;
  border-color: #4b5563 !important;
  border-bottom-color: #4b5563 !important;
  box-shadow: none !important;
}

:root.dark .hg-theme-default .hg-button:hover,
html.dark .hg-theme-default .hg-button:hover,
.dark .hg-theme-default .hg-button:hover {
  background: #4b5563 !important;
}

:root.dark .hg-theme-default .hg-button:active,
html.dark .hg-theme-default .hg-button:active,
.dark .hg-theme-default .hg-button:active,
:root.dark .hg-theme-default .hg-button.hg-activeButton,
html.dark .hg-theme-default .hg-button.hg-activeButton,
.dark .hg-theme-default .hg-button.hg-activeButton {
  background: #3b82f6 !important;
  color: white !important;
}

:root.dark .hg-theme-default .hg-button.down-key,
html.dark .hg-theme-default .hg-button.down-key,
.dark .hg-theme-default .hg-button.down-key {
  background: #3b82f6 !important;
  color: white !important;
  border-color: #2563eb !important;
  border-bottom-color: #2563eb !important;
}
</style>

<style scoped>
.vkb {
  z-index: 100;
  user-select: none;
}

.vkb--attached {
  width: 100%;
  border-radius: 0;
  box-shadow: inset 0 1px 0 var(--border);
}

.vkb--floating {
  min-width: 1200px;
  max-width: 1600px;
  width: auto;
}

.vkb-header-separator {
  width: 1px;
  height: 16px;
  margin: 0 2px;
  background: var(--border);
}

.vkb-window-action {
  width: 28px;
  height: 28px;
  padding: 0;
  color: var(--muted-foreground);
}

/* Keyboard body */
.vkb-body {
  display: flex;
  flex-direction: column;
  padding: 8px;
  gap: 8px;
  background: #f3f4f6;
}

:global(.dark .vkb-body) {
  background: #111827;
}

.vkb--floating .vkb-body {
  border-radius: 0 0 8px 8px;
}

/* Media keys row */
.vkb-media-row {
  display: flex;
  gap: 4px;
  justify-content: center;
  padding-bottom: 4px;
  border-bottom: 1px solid #e5e7eb;
}

:global(.dark .vkb-media-row) {
  border-color: #374151;
}

/* Keyboards container */
.vkb-keyboards {
  display: flex;
  flex-direction: row;
  gap: 8px;
}

.kb-main-container {
  flex: 1;
}

/* Responsive */
@media (max-width: 640px) {
  .vkb-body {
    padding: 4px;
    gap: 4px;
  }

  .vkb .simple-keyboard .hg-button {
    height: 30px;
    font-size: 11px;
    padding: 0 2px;
    margin: 0 1px 2px 0;
    min-width: 0;
  }

  .vkb .simple-keyboard .hg-button.combination-key {
    font-size: 9px;
    height: 24px;
    padding: 0 6px;
  }

  .vkb .simple-keyboard .hg-button[data-skbtn="Backspace"],
  .vkb .simple-keyboard .hg-button[data-skbtn="Tab"],
  .vkb .simple-keyboard .hg-button[data-skbtn="Backslash"],
  .vkb .simple-keyboard .hg-button[data-skbtn="(Backslash)"],
  .vkb .simple-keyboard .hg-button[data-skbtn="CapsLock"],
  .vkb .simple-keyboard .hg-button[data-skbtn="Enter"],
  .vkb .simple-keyboard .hg-button[data-skbtn="ShiftLeft"],
  .vkb .simple-keyboard .hg-button[data-skbtn="ShiftRight"],
  .vkb .simple-keyboard .hg-button[data-skbtn="Space"],
  .vkb .simple-keyboard .hg-button[data-skbtn="ControlLeft"],
  .vkb .simple-keyboard .hg-button[data-skbtn="ControlRight"],
  .vkb .simple-keyboard .hg-button[data-skbtn="MetaLeft"],
  .vkb .simple-keyboard .hg-button[data-skbtn="MetaRight"],
  .vkb .simple-keyboard .hg-button[data-skbtn="AltLeft"],
  .vkb .simple-keyboard .hg-button[data-skbtn="AltRight"],
  .vkb .simple-keyboard .hg-button[data-skbtn="ContextMenu"] {
    min-width: 0;
  }

  .vkb .simple-keyboard .hg-button[data-skbtn="Insert"],
  .vkb .simple-keyboard .hg-button[data-skbtn="Delete"],
  .vkb .simple-keyboard .hg-button[data-skbtn="Home"],
  .vkb .simple-keyboard .hg-button[data-skbtn="End"],
  .vkb .simple-keyboard .hg-button[data-skbtn="PageUp"],
  .vkb .simple-keyboard .hg-button[data-skbtn="PageDown"],
  .vkb .simple-keyboard .hg-button[data-skbtn="PrintScreen"] {
    font-size: 11px;
    flex-grow: 1;
  }

  .vkb .simple-keyboard .hg-button[data-skbtn="PrintScreen"],
  .vkb .simple-keyboard .hg-button[data-skbtn="Insert"],
  .vkb .simple-keyboard .hg-button[data-skbtn="Delete"],
  .vkb .simple-keyboard .hg-button[data-skbtn="ArrowLeft"] {
    margin-left: 0;
  }

  .vkb .simple-keyboard .hg-button[data-skbtn="ArrowUp"] {
    margin-left: 4px;
  }

}

@media (max-width: 400px) {
  .vkb .simple-keyboard .hg-button {
    height: 28px;
    font-size: 10px;
    padding: 0 1px;
    margin: 0 1px 2px 0;
    border-radius: 4px;
  }

  .vkb .simple-keyboard .hg-button.combination-key {
    font-size: 8px;
    height: 22px;
    padding: 0 4px;
  }

  .vkb .simple-keyboard .hg-button[data-skbtn="Insert"],
  .vkb .simple-keyboard .hg-button[data-skbtn="Delete"],
  .vkb .simple-keyboard .hg-button[data-skbtn="Home"],
  .vkb .simple-keyboard .hg-button[data-skbtn="End"],
  .vkb .simple-keyboard .hg-button[data-skbtn="PageUp"],
  .vkb .simple-keyboard .hg-button[data-skbtn="PageDown"],
  .vkb .simple-keyboard .hg-button[data-skbtn="PrintScreen"] {
    font-size: 10px;
  }

}

/* Floating mode - slightly smaller keys but still readable */
.vkb--floating .vkb-body {
  padding: 8px;
}

.vkb--floating :deep(.simple-keyboard .hg-button) {
  height: 34px;
  font-size: 12px;
}

.vkb--floating :deep(.simple-keyboard .hg-button.combination-key) {
  height: 26px;
  font-size: 10px;
}

/* Animation */
.keyboard-fade-enter-active,
.keyboard-fade-leave-active {
  transition: opacity 0.2s ease, transform 0.2s ease;
}

.keyboard-fade-enter-from,
.keyboard-fade-leave-to {
  opacity: 0;
}

.vkb--attached.keyboard-fade-enter-from,
.vkb--attached.keyboard-fade-leave-to {
  transform: translateY(20px);
}

</style>
