import { DeviceScope } from '@/api/deviceScope'
import { currentBmcIdFromLocation } from '@/api/request'
import { ref, computed, watch, type Ref } from 'vue'
import { storeToRefs } from 'pinia'
import { useSystemStore } from '@/stores/system'
import { useI18n } from 'vue-i18n'
import { toast } from 'vue-sonner'
import { CanonicalKey } from '@/types/generated'
import type { HidKeyboardEvent, HidMouseEvent } from '@/types/hid'
import { keyboardEventToCanonicalKey, updateModifierMaskForKey } from '@/lib/keyboardMappings'
import { useHidStore } from './store'
import { useHidEventBus } from './eventBus'

export function useHidInput(opts: {
  videoContainerRef: Ref<HTMLDivElement | null>
  isHttpMode: Ref<boolean>
  isFullscreen: Ref<boolean>
  webrtc: ReturnType<typeof import('@/composables/useWebRTC').useWebRTC>
  videoRef: Ref<HTMLImageElement | null>
  webrtcVideoRef: Ref<HTMLVideoElement | null>
  webTransportCanvasRef: Ref<HTMLCanvasElement | null>
}) {
  const { t } = useI18n()
  const systemStore = useSystemStore()
  const hidStore = useHidStore()
  const { mouseMode, cursorVisible } = storeToRefs(hidStore)
  const { syncMouseModeFromConfig } = hidStore

  watch(mouseMode, () => {
    pendingMouseMove = null
    accumulatedDelta = { x: 0, y: 0 }
  })

  const pressedKeys = ref<CanonicalKey[]>([])
  const keyboardLed = computed(() => ({
    capsLock: systemStore.hid?.ledState.capsLock ?? false,
    numLock: systemStore.hid?.ledState.numLock ?? false,
    scrollLock: systemStore.hid?.ledState.scrollLock ?? false,
  }))
  const keyboardLedEnabled = computed(() => systemStore.hid?.keyboardLedsEnabled ?? false)
  const activeModifierMask = ref(0)
  const mousePosition = ref({ x: 0, y: 0 })
  const isPointerLocked = ref(false)
  const localCrosshairPos = ref<{ x: number; y: number } | null>(null)
  const virtualKeyboardVisible = ref(false)
  const virtualKeyboardAttached = ref(true)
  const virtualKeyboardConsumerEnabled = computed(() => true)
  type MouseButton = 'left' | 'right' | 'middle'
  const pressedMouseButtons = ref<Set<MouseButton>>(new Set())

  const hidBus = useHidEventBus()

  const DEFAULT_MOUSE_MOVE_SEND_INTERVAL_MS = 16
  let mouseMoveSendIntervalMs = DEFAULT_MOUSE_MOVE_SEND_INTERVAL_MS
  let mouseFlushTimer: ReturnType<typeof setTimeout> | null = null
  let lastMouseMoveSendTime = 0
  let pendingMouseMove: { type: 'move' | 'move_abs'; x: number; y: number } | null = null
  let accumulatedDelta = { x: 0, y: 0 }
  let interactionListenersBound = false
  let discreteInputChain: Promise<void> = Promise.resolve()
  const inputScope = new DeviceScope(currentBmcIdFromLocation)
  let acceptingInput = false

  function sendKeyboardEvent(type: 'down' | 'up', key: CanonicalKey, immediate = false) {
    const event: HidKeyboardEvent = {
      type: type === 'down' ? 'keydown' : 'keyup',
      key,
    }
    if (immediate) {
      return hidBus.sendKeyboard(event).then(() => true, () => false)
    }
    // Keep releases ahead of later presses when queue admission must wait.
    return enqueueReliableInput(() => type === 'down' && !pressedKeys.value.includes(key)
      ? Promise.resolve()
      : hidBus.sendKeyboard(event))
  }

  function sendMouseEvent(data: { type: 'move' | 'move_abs' | 'down' | 'up' | 'scroll'; x?: number; y?: number; button?: 'left' | 'right' | 'middle'; scroll?: number }) {
    const event: HidMouseEvent = {
      type: data.type === 'move_abs' ? 'moveabs' : data.type,
      x: data.x,
      y: data.y,
      button: data.button === 'left' ? 0 : data.button === 'middle' ? 1 : data.button === 'right' ? 2 : undefined,
      scroll: data.scroll,
    }
    hidBus.sendMouse(event).catch(() => {})
  }

  function enqueueReliableMouseEvent(data: { type: 'down' | 'up' | 'scroll'; x?: number; y?: number; button?: 'left' | 'right' | 'middle'; scroll?: number }) {
    const event: HidMouseEvent = {
      type: data.type,
      x: data.x,
      y: data.y,
      button: data.button === 'left' ? 0 : data.button === 'middle' ? 1 : data.button === 'right' ? 2 : undefined,
      scroll: data.scroll,
    }
    enqueueReliableInput(() => hidBus.sendMouse(event))
  }

  function enqueueReliableConsumerEvent(usage: number) {
    enqueueReliableInput(() => hidBus.sendConsumer({ usage }))
  }

  function enqueueReliableInput(send: () => Promise<void>) {
    const owner = inputScope.capture()
    const current = () => acceptingInput && owner.current()
    const operation = discreteInputChain.then(async () => {
      for (let attempt = 0; attempt < 4; attempt++) {
        if (!current()) return false
        try {
          await send()
          return true
        } catch {
          if (!current()) return false
          if (!hidBus.connected.value) await hidBus.connect().catch(() => false)
          if (!current()) return false
          await new Promise(resolve => window.setTimeout(resolve, 16 * (attempt + 1)))
        }
      }
      if (current()) toast.error(t('console.hidInputFailed'), {
        description: hidBus.lastSendError.value ?? undefined,
      })
      return false
    })
    discreteInputChain = operation.then(() => undefined)
    return operation
  }

  function shouldBlockKey(e: KeyboardEvent): boolean {
    if (opts.isFullscreen.value) return true
    const key = e.key.toUpperCase()
    if (e.ctrlKey && ['W', 'T', 'N'].includes(key)) return false
    if (key === 'F11') return false
    if (e.altKey && key === 'TAB') return false
    return true
  }

  function handleKeyDown(e: KeyboardEvent) {
    const container = opts.videoContainerRef.value
    if (!container) return
    if (!opts.isFullscreen.value && !container.contains(document.activeElement)) return
    if (shouldBlockKey(e)) {
      e.preventDefault()
      e.stopPropagation()
    }
    const canonicalKey = keyboardEventToCanonicalKey(e.code, e.key)
    if (canonicalKey === undefined) {
      console.warn(`[HID] Unmapped key down: code=${e.code}, key=${e.key}`)
      return
    }
    const alreadyPressed = pressedKeys.value.includes(canonicalKey)
    if (alreadyPressed) return
    pressedKeys.value = [...pressedKeys.value, canonicalKey]
    const modifierMask = updateModifierMaskForKey(activeModifierMask.value, canonicalKey, true)
    activeModifierMask.value = modifierMask
    sendKeyboardEvent('down', canonicalKey)
  }

  function handleKeyUp(e: KeyboardEvent) {
    const container = opts.videoContainerRef.value
    if (!container) return
    if (!opts.isFullscreen.value && !container.contains(document.activeElement)) return
    if (shouldBlockKey(e)) {
      e.preventDefault()
      e.stopPropagation()
    }
    const canonicalKey = keyboardEventToCanonicalKey(e.code, e.key)
    if (canonicalKey === undefined) {
      console.warn(`[HID] Unmapped key up: code=${e.code}, key=${e.key}`)
      return
    }
    const wasPressed = pressedKeys.value.includes(canonicalKey)
    if (!wasPressed) return
    pressedKeys.value = pressedKeys.value.filter(k => k !== canonicalKey)
    const modifierMask = updateModifierMaskForKey(activeModifierMask.value, canonicalKey, false)
    activeModifierMask.value = modifierMask
    sendKeyboardEvent('up', canonicalKey)
  }

  function getActiveVideoElement(): HTMLImageElement | HTMLVideoElement | HTMLCanvasElement | null {
    return opts.videoRef.value ?? opts.webrtcVideoRef.value ?? opts.webTransportCanvasRef.value
  }

  function getActiveVideoDimensions(): { width: number; height: number } | null {
    const element = getActiveVideoElement()
    if (element instanceof HTMLImageElement && element.naturalWidth && element.naturalHeight) {
      return { width: element.naturalWidth, height: element.naturalHeight }
    }
    if (element instanceof HTMLVideoElement && element.videoWidth && element.videoHeight) {
      return { width: element.videoWidth, height: element.videoHeight }
    }
    if (element instanceof HTMLCanvasElement && element.width && element.height) {
      return { width: element.width, height: element.height }
    }
    return null
  }

  function getRenderedVideoRect() {
    const videoElement = getActiveVideoElement()
    if (!videoElement) return null
    const rect = videoElement.getBoundingClientRect()
    if (rect.width <= 0 || rect.height <= 0) return null
    const dimensions = getActiveVideoDimensions()
    if (!dimensions) return rect
    const contentAspectRatio = dimensions.width / dimensions.height
    const boxAspectRatio = rect.width / rect.height
    if (!Number.isFinite(boxAspectRatio) || boxAspectRatio <= 0) return rect
    if (boxAspectRatio > contentAspectRatio) {
      const width = rect.height * contentAspectRatio
      return {
        left: rect.left + (rect.width - width) / 2,
        top: rect.top,
        width,
        height: rect.height,
      }
    }
    const height = rect.width / contentAspectRatio
    return {
      left: rect.left,
      top: rect.top + (rect.height - height) / 2,
      width: rect.width,
      height,
    }
  }

  function getAbsoluteMousePosition(e: MouseEvent) {
    const rect = getRenderedVideoRect()
    if (!rect) return null
    const normalizedX = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width))
    const normalizedY = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height))
    const dimensions = getActiveVideoDimensions()
    if (!dimensions) return null
    return {
      x: Math.round(normalizedX * Math.max(0, dimensions.width - 1)),
      y: Math.round(normalizedY * Math.max(0, dimensions.height - 1)),
    }
  }

  function updateLocalCrosshairFromEvent(e: MouseEvent) {
    if (!cursorVisible.value) {
      localCrosshairPos.value = null
      return
    }
    const container = opts.videoContainerRef.value
    if (!container) return
    const rect = container.getBoundingClientRect()
    if (rect.width <= 0 || rect.height <= 0) return
    if (mouseMode.value === 'relative' && isPointerLocked.value) {
      const cur = localCrosshairPos.value
      const nx = cur ? cur.x + e.movementX : rect.width / 2
      const ny = cur ? cur.y + e.movementY : rect.height / 2
      localCrosshairPos.value = {
        x: Math.max(0, Math.min(rect.width, nx)),
        y: Math.max(0, Math.min(rect.height, ny)),
      }
      return
    }
    localCrosshairPos.value = {
      x: Math.max(0, Math.min(rect.width, e.clientX - rect.left)),
      y: Math.max(0, Math.min(rect.height, e.clientY - rect.top)),
    }
  }

  function handleMouseLeaveVideo() {
    if (!isPointerLocked.value) {
      localCrosshairPos.value = null
    }
  }

  function handleMouseMove(e: MouseEvent) {
    updateLocalCrosshairFromEvent(e)
    const videoElement = getActiveVideoElement()
    if (!videoElement) return
    if (mouseMode.value === 'absolute') {
      const absolutePosition = getAbsoluteMousePosition(e)
      if (!absolutePosition) return
      const { x, y } = absolutePosition
      mousePosition.value = { x, y }
      pendingMouseMove = { type: 'move_abs', x, y }
      requestMouseMoveFlush()
    } else {
      if (isPointerLocked.value) {
        const dx = e.movementX
        const dy = e.movementY
        if (dx !== 0 || dy !== 0) {
          accumulatedDelta.x += dx
          accumulatedDelta.y += dy
          requestMouseMoveFlush()
        }
        mousePosition.value = {
          x: mousePosition.value.x + dx,
          y: mousePosition.value.y + dy,
        }
      }
    }
  }

  function hasPendingMouseMove(): boolean {
    if (mouseMode.value === 'absolute') return pendingMouseMove !== null
    return accumulatedDelta.x !== 0 || accumulatedDelta.y !== 0
  }

  function flushMouseMoveOnce(): boolean {
    if (mouseMode.value === 'absolute') {
      if (!pendingMouseMove) return false
      sendMouseEvent(pendingMouseMove)
      pendingMouseMove = null
      return true
    }
    if (accumulatedDelta.x === 0 && accumulatedDelta.y === 0) return false
    const clampedDx = Math.max(-127, Math.min(127, accumulatedDelta.x))
    const clampedDy = Math.max(-127, Math.min(127, accumulatedDelta.y))
    sendMouseEvent({ type: 'move', x: clampedDx, y: clampedDy })
    accumulatedDelta.x -= clampedDx
    accumulatedDelta.y -= clampedDy
    return true
  }

  function scheduleMouseMoveFlush() {
    if (mouseFlushTimer !== null) return
    const interval = mouseMoveSendIntervalMs
    const now = Date.now()
    const elapsed = now - lastMouseMoveSendTime
    const delay = interval <= 0 ? 0 : Math.max(0, interval - elapsed)
    mouseFlushTimer = setTimeout(() => {
      mouseFlushTimer = null
      const burstLimit = mouseMoveSendIntervalMs <= 0 ? 8 : 1
      let sent = false
      for (let i = 0; i < burstLimit; i++) {
        if (!flushMouseMoveOnce()) break
        sent = true
        if (!hasPendingMouseMove()) break
      }
      if (sent) lastMouseMoveSendTime = Date.now()
      if (hasPendingMouseMove()) {
        scheduleMouseMoveFlush()
      }
    }, delay)
  }

  function requestMouseMoveFlush() {
    const interval = mouseMoveSendIntervalMs
    const now = Date.now()
    if (interval <= 0 || now - lastMouseMoveSendTime >= interval) {
      const burstLimit = interval <= 0 ? 8 : 1
      let sent = false
      for (let i = 0; i < burstLimit; i++) {
        if (!flushMouseMoveOnce()) break
        sent = true
        if (!hasPendingMouseMove()) break
      }
      if (sent) lastMouseMoveSendTime = Date.now()
      if (hasPendingMouseMove()) {
        scheduleMouseMoveFlush()
      }
      return
    }
    scheduleMouseMoveFlush()
  }

  function handleMouseDown(e: MouseEvent) {
    e.preventDefault()
    const container = opts.videoContainerRef.value
    if (container && document.activeElement !== container) {
      if (typeof container.focus === 'function') {
        container.focus()
      }
    }
    if (mouseMode.value === 'relative' && !isPointerLocked.value) {
      requestPointerLock()
      return
    }
    if (e.button === 3 || e.button === 4) {
      const usage = e.button === 3 ? 0x0224 : 0x0225
      enqueueReliableConsumerEvent(usage)
      return
    }
    if (mouseMode.value === 'absolute') {
      const absolutePosition = getAbsoluteMousePosition(e)
      if (absolutePosition) {
        mousePosition.value = absolutePosition
        sendMouseEvent({ type: 'move_abs', ...absolutePosition })
        pendingMouseMove = null
      }
    }
    const button: MouseButton = e.button === 0 ? 'left' : e.button === 2 ? 'right' : 'middle'
    if (pressedMouseButtons.value.has(button)) return
    pressedMouseButtons.value = new Set(pressedMouseButtons.value).add(button)
    enqueueReliableMouseEvent({
      type: 'down',
      x: mouseMode.value === 'relative' ? 0 : mousePosition.value.x,
      y: mouseMode.value === 'relative' ? 0 : mousePosition.value.y,
      button,
    })
  }

  function handleMouseUp(e: MouseEvent) {
    e.preventDefault()
    handleMouseUpInternal(e.button)
  }

  function handleWindowMouseUp(e: MouseEvent) {
    if (pressedMouseButtons.value.size > 0) {
      handleMouseUpInternal(e.button)
    }
  }

  function handleMouseUpInternal(rawButton: number) {
    if (rawButton === 3 || rawButton === 4) return
    const button: MouseButton = rawButton === 0 ? 'left' : rawButton === 2 ? 'right' : 'middle'
    if (mouseMode.value === 'relative' && !isPointerLocked.value) {
      const next = new Set(pressedMouseButtons.value)
      next.delete(button)
      pressedMouseButtons.value = next
      return
    }
    if (!pressedMouseButtons.value.has(button)) return
    const next = new Set(pressedMouseButtons.value)
    next.delete(button)
    pressedMouseButtons.value = next
    enqueueReliableMouseEvent({
      type: 'up',
      x: mouseMode.value === 'relative' ? 0 : mousePosition.value.x,
      y: mouseMode.value === 'relative' ? 0 : mousePosition.value.y,
      button,
    })
  }

  function handleWheel(e: WheelEvent) {
    e.preventDefault()
    const scroll = e.deltaY > 0 ? -1 : 1
    enqueueReliableMouseEvent({
      type: 'scroll',
      x: mouseMode.value === 'relative' ? 0 : mousePosition.value.x,
      y: mouseMode.value === 'relative' ? 0 : mousePosition.value.y,
      scroll,
    })
  }

  function handleContextMenu(e: MouseEvent) {
    e.preventDefault()
  }

  function requestPointerLock() {
    const container = opts.videoContainerRef.value
    if (!container) return
    Promise.resolve(container.requestPointerLock()).catch((err: Error) => {
      toast.error(t('console.pointerLockFailed'), {
        description: err.message,
      })
    })
  }

  function exitPointerLock() {
    if (document.pointerLockElement) {
      document.exitPointerLock()
    }
  }

  function handlePointerLockChange() {
    const container = opts.videoContainerRef.value
    isPointerLocked.value = document.pointerLockElement === container
    if (isPointerLocked.value) {
      mousePosition.value = { x: 0, y: 0 }
      if (cursorVisible.value && container) {
        const r = container.getBoundingClientRect()
        if (r.width > 0 && r.height > 0) {
          localCrosshairPos.value = { x: r.width / 2, y: r.height / 2 }
        }
      }
    }
  }

  function handlePointerLockError() {
    isPointerLocked.value = false
  }

  function releasePressedInputs(immediate = false) {
    const keysToRelease = [...pressedKeys.value]
    pressedKeys.value = []
    activeModifierMask.value = 0
    for (const key of keysToRelease) {
      sendKeyboardEvent('up', key, immediate)
    }

    const buttonsToRelease = [...pressedMouseButtons.value]
    pressedMouseButtons.value = new Set()
    for (const button of buttonsToRelease) {
      const release = immediate ? sendMouseEvent : enqueueReliableMouseEvent
      release({
        type: 'up',
        x: mouseMode.value === 'relative' ? 0 : mousePosition.value.x,
        y: mouseMode.value === 'relative' ? 0 : mousePosition.value.y,
        button,
      })
    }
  }

  function handleBlur() {
    releasePressedInputs()
  }

  function handleCursorVisibilityChange(e: Event) {
    const customEvent = e as CustomEvent<{ visible: boolean }>
    cursorVisible.value = customEvent.detail.visible
    if (!cursorVisible.value) {
      localCrosshairPos.value = null
    }
  }

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

  function setMouseMoveSendInterval(ms: number) {
    mouseMoveSendIntervalMs = clampMouseMoveSendIntervalMs(ms)
    if (mouseFlushTimer !== null) {
      clearTimeout(mouseFlushTimer)
      mouseFlushTimer = null
    }
    if (hasPendingMouseMove()) {
      requestMouseMoveFlush()
    }
  }

  function handleMouseSendIntervalChange(e: Event) {
    const customEvent = e as CustomEvent<{ intervalMs: number }>
    setMouseMoveSendInterval(customEvent.detail?.intervalMs)
  }

  function handleMouseSendIntervalStorage(e: StorageEvent) {
    if (e.key !== 'hidMouseThrottle') return
    setMouseMoveSendInterval(loadMouseMoveSendIntervalFromStorage())
  }

  function registerInteractionListeners() {
    if (interactionListenersBound) return
    window.addEventListener('keydown', handleKeyDown)
    window.addEventListener('keyup', handleKeyUp)
    window.addEventListener('blur', handleBlur)
    window.addEventListener('mouseup', handleWindowMouseUp)
    window.addEventListener('beforeunload', handleBlur)
    window.addEventListener('hidCursorVisibilityChanged', handleCursorVisibilityChange as EventListener)
    window.addEventListener('hidMouseSendIntervalChanged', handleMouseSendIntervalChange as EventListener)
    window.addEventListener('storage', handleMouseSendIntervalStorage)
    document.addEventListener('pointerlockchange', handlePointerLockChange)
    document.addEventListener('pointerlockerror', handlePointerLockError)
    interactionListenersBound = true
  }

  function unregisterInteractionListeners() {
    if (!interactionListenersBound) return
    window.removeEventListener('keydown', handleKeyDown)
    window.removeEventListener('keyup', handleKeyUp)
    window.removeEventListener('blur', handleBlur)
    window.removeEventListener('mouseup', handleWindowMouseUp)
    window.removeEventListener('beforeunload', handleBlur)
    window.removeEventListener('hidCursorVisibilityChanged', handleCursorVisibilityChange as EventListener)
    window.removeEventListener('hidMouseSendIntervalChanged', handleMouseSendIntervalChange as EventListener)
    window.removeEventListener('storage', handleMouseSendIntervalStorage)
    document.removeEventListener('pointerlockchange', handlePointerLockChange)
    document.removeEventListener('pointerlockerror', handlePointerLockError)
    interactionListenersBound = false
  }

  function init() {
    inputScope.reset()
    acceptingInput = true
    setMouseMoveSendInterval(loadMouseMoveSendIntervalFromStorage())
    hidBus.connect()
  }

  function dispose() {
    inputScope.reset()
    acceptingInput = false
    discreteInputChain = Promise.resolve()
    releasePressedInputs(true)
    pendingMouseMove = null
    accumulatedDelta = { x: 0, y: 0 }
    if (mouseFlushTimer !== null) {
      clearTimeout(mouseFlushTimer)
      mouseFlushTimer = null
    }
    exitPointerLock()
  }

  function handleToggleMouseMode() {
    releasePressedInputs()
    if (mouseMode.value === 'relative' && isPointerLocked.value) {
      exitPointerLock()
    }
    mouseMode.value = mouseMode.value === 'absolute' ? 'relative' : 'absolute'
    pendingMouseMove = null
    accumulatedDelta = { x: 0, y: 0 }
    mousePosition.value = { x: 0, y: 0 }
  }

  function handleToggleVirtualKeyboard() {
    virtualKeyboardVisible.value = !virtualKeyboardVisible.value
  }

  function captureKeyboard() {
    const owner = inputScope.capture()
    const current = () => acceptingInput && owner.current()
    return {
      current,
      async send(key: CanonicalKey, press: boolean) {
        if (!current()) return false
        if (press) {
          if (!pressedKeys.value.includes(key)) pressedKeys.value = [...pressedKeys.value, key]
        } else {
          pressedKeys.value = pressedKeys.value.filter(candidate => candidate !== key)
        }
        return sendKeyboardEvent(press ? 'down' : 'up', key)
      },
    }
  }

  return {
    mouseMode,
    pressedKeys,
    keyboardLed,
    keyboardLedEnabled,
    mousePosition,
    isPointerLocked,
    localCrosshairPos,
    cursorVisible,
    virtualKeyboardVisible,
    virtualKeyboardAttached,
    virtualKeyboardConsumerEnabled,
    syncMouseModeFromConfig,
    handleKeyDown,
    handleKeyUp,
    handleMouseMove,
    handleMouseDown,
    handleMouseUp,
    handleMouseLeaveVideo,
    handleWheel,
    handleContextMenu,
    handleBlur,
    handleToggleMouseMode,
    handleToggleVirtualKeyboard,
    captureKeyboard,
    registerInteractionListeners,
    unregisterInteractionListeners,
    init,
    dispose,
  }
}
