<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Focus, Maximize2, Minimize2, Minus, Monitor, MoveDiagonal2, Plus, Scan } from 'lucide-vue-next'

import { Button } from '@/components/ui/button'
import DraggableWindowFrame from '@/components/DraggableWindowFrame.vue'
import type { WindowPosition } from '@/lib/floatingWindowGeometry'

import {
  fitVideoWindow,
  layoutForSourceChange,
  layoutForWorkspaceChange,
  maximizeVideoWindow,
  resizeVideoWindow,
  type VideoDimensions,
  type VideoWindowLayout,
  type VideoWindowLayoutMode,
} from './viewportGeometry'

const props = defineProps<{
  sourceWidth: number | null
  sourceHeight: number | null
}>()

const { t } = useI18n()
const workspaceRef = ref<HTMLDivElement | null>(null)
const layout = ref<VideoWindowLayout | null>(null)
const layoutMode = ref<VideoWindowLayoutMode>('fit')
const customScale = ref(1)
const workspaceSize = ref<VideoDimensions>({ width: 1, height: 1 })
const isResizing = ref(false)
const isMaximized = computed(() => layoutMode.value === 'maximized')

type WindowedLayoutMode = Exclude<VideoWindowLayoutMode, 'maximized'>

interface RestoreState {
  layout: VideoWindowLayout
  mode: WindowedLayoutMode
  customScale: number
}

const sourceSize = computed<VideoDimensions>(() => ({
  width: props.sourceWidth && props.sourceWidth > 0 ? props.sourceWidth : 1280,
  height: props.sourceHeight && props.sourceHeight > 0 ? props.sourceHeight : 720,
}))

const hasSourceSize = computed(() =>
  props.sourceWidth !== null
  && props.sourceHeight !== null
  && props.sourceWidth > 0
  && props.sourceHeight > 0,
)

const windowStyle = computed(() => {
  const current = layout.value
  if (!current) return { visibility: 'hidden' as const }
  return {
    width: `${current.videoWidth}px`,
    height: `${current.videoHeight + current.chromeHeight}px`,
  }
})

const windowPosition = computed<WindowPosition>({
  get: () => ({ x: layout.value?.x ?? 0, y: layout.value?.y ?? 0 }),
  set: (position) => {
    if (layout.value) layout.value = { ...layout.value, ...position }
  },
})

const windowSize = computed<VideoDimensions>(() => ({
  width: layout.value?.videoWidth ?? 0,
  height: layout.value ? layout.value.videoHeight + layout.value.chromeHeight : 0,
}))

const surfaceStyle = computed(() => {
  const current = layout.value
  if (!current) return {}
  return {
    width: `${current.videoWidth}px`,
    height: `${current.videoHeight}px`,
  }
})

const scalePercent = computed(() => Math.round((layout.value?.scale ?? 1) * 100))
const sourceResolution = computed(() => hasSourceSize.value
  ? t('console.sourceResolution', { width: props.sourceWidth, height: props.sourceHeight })
  : t('console.sourceResolutionPending'))
const scaleLabel = computed(() => t('console.effectiveScale', { scale: scalePercent.value }))

interface PointerInteraction {
  pointerId: number
  startX: number
  startY: number
  initialLayout: VideoWindowLayout
}

let resizeInteraction: PointerInteraction | null = null
let resizeObserver: ResizeObserver | null = null
let restoreState: RestoreState | null = null

function measureWorkspace(): VideoDimensions {
  const element = workspaceRef.value
  if (!element) return workspaceSize.value
  return {
    width: Math.max(1, element.clientWidth),
    height: Math.max(1, element.clientHeight),
  }
}

function applyWorkspaceLayout(nextWorkspace: VideoDimensions) {
  workspaceSize.value = nextWorkspace
  if (!layout.value) {
    layout.value = fitVideoWindow(sourceSize.value, nextWorkspace)
    customScale.value = layout.value.scale
    return
  }
  layout.value = layoutForWorkspaceChange(
    sourceSize.value,
    nextWorkspace,
    layoutMode.value,
    customScale.value,
    layout.value,
  )
}

function applyMode(mode: WindowedLayoutMode) {
  restoreState = null
  layoutMode.value = mode
  const requestedScale = mode === 'actual' ? 1 : customScale.value
  layout.value = layoutForSourceChange(
    sourceSize.value,
    workspaceSize.value,
    mode,
    requestedScale,
    mode === 'fit' ? undefined : layout.value ?? undefined,
  )
  customScale.value = layout.value.scale
}

function toggleMaximize() {
  const current = layout.value
  if (!current) return
  if (layoutMode.value === 'maximized') {
    const restore = restoreState
    restoreState = null
    if (!restore) {
      applyMode('fit')
      return
    }
    layoutMode.value = restore.mode
    customScale.value = restore.customScale
    layout.value = layoutForSourceChange(
      sourceSize.value,
      workspaceSize.value,
      restore.mode,
      restore.customScale,
      restore.layout,
    )
    return
  }

  restoreState = {
    layout: { ...current },
    mode: layoutMode.value,
    customScale: customScale.value,
  }
  layoutMode.value = 'maximized'
  layout.value = maximizeVideoWindow(sourceSize.value, workspaceSize.value)
  workspaceRef.value?.scrollTo({ left: 0, top: 0 })
}

function zoom(factor: number) {
  const current = layout.value
  if (!current) return
  restoreState = null
  layoutMode.value = 'custom'
  customScale.value = current.scale * factor
  layout.value = layoutForSourceChange(
    sourceSize.value,
    workspaceSize.value,
    'custom',
    customScale.value,
    current,
  )
  customScale.value = layout.value.scale
}

function beginResize(event: PointerEvent) {
  if (event.button !== 0 || !layout.value || isMaximized.value) return
  event.preventDefault()
  const handle = event.currentTarget as HTMLElement
  handle.setPointerCapture(event.pointerId)
  resizeInteraction = {
    pointerId: event.pointerId,
    startX: event.clientX,
    startY: event.clientY,
    initialLayout: { ...layout.value },
  }
  isResizing.value = true
}

function updateResize(event: PointerEvent) {
  if (!resizeInteraction || event.pointerId !== resizeInteraction.pointerId) return
  layoutMode.value = 'custom'
  layout.value = resizeVideoWindow(
    sourceSize.value,
    workspaceSize.value,
    resizeInteraction.initialLayout,
    event.clientX - resizeInteraction.startX,
    event.clientY - resizeInteraction.startY,
  )
  customScale.value = layout.value.scale
}

function endResize(event: PointerEvent) {
  if (!resizeInteraction || event.pointerId !== resizeInteraction.pointerId) return
  const handle = event.currentTarget as HTMLElement
  if (handle.hasPointerCapture(event.pointerId)) handle.releasePointerCapture(event.pointerId)
  resizeInteraction = null
  isResizing.value = false
}

function resizeWithKeyboard(event: KeyboardEvent) {
  const direction = event.key === 'ArrowLeft' || event.key === 'ArrowDown'
    ? -1
    : event.key === 'ArrowRight' || event.key === 'ArrowUp'
      ? 1
      : 0
  if (!direction || !layout.value || isMaximized.value) return
  event.preventDefault()
  const delta = direction * (event.shiftKey ? 48 : 12)
  layoutMode.value = 'custom'
  layout.value = resizeVideoWindow(
    sourceSize.value,
    workspaceSize.value,
    layout.value,
    delta,
    delta * sourceSize.value.height / sourceSize.value.width,
  )
  customScale.value = layout.value.scale
}

watch(
  () => [props.sourceWidth, props.sourceHeight] as const,
  ([width, height], [previousWidth, previousHeight]) => {
    if (!width || !height || width <= 0 || height <= 0) return
    if (width === previousWidth && height === previousHeight) return
    layout.value = layoutForSourceChange(
      sourceSize.value,
      workspaceSize.value,
      layoutMode.value,
      customScale.value,
      layout.value ?? undefined,
    )
    customScale.value = layout.value.scale
  },
)

onMounted(async () => {
  await nextTick()
  applyWorkspaceLayout(measureWorkspace())
  resizeObserver = new ResizeObserver(() => applyWorkspaceLayout(measureWorkspace()))
  if (workspaceRef.value) resizeObserver.observe(workspaceRef.value)
})

onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  resizeObserver = null
})
</script>

<template>
  <div
    ref="workspaceRef"
    class="video-window-workspace"
    data-video-workspace
  >
    <DraggableWindowFrame
      v-model="windowPosition"
      :workspace-size="workspaceSize"
      :window-title="t('console.videoWindowTitle')"
      :window-size="windowSize"
      :disabled="isMaximized"
      :maximized="isMaximized"
      positioning="absolute"
      class="video-window"
      :class="{
        'is-resizing': isResizing,
      }"
      :style="windowStyle"
      data-video-window
      :data-layout-mode="layoutMode"
      :aria-label="t('console.videoWindowTitle')"
      @handle-dblclick="toggleMaximize"
    >
      <template #icon>
        <Monitor :size="14" />
      </template>
      <template #meta>
        <span>{{ sourceResolution }}</span>
        <span aria-hidden="true">·</span>
        <span>{{ scaleLabel }}</span>
      </template>
      <template #actions>
        <Button
          variant="ghost"
          size="icon-sm"
          class="video-window__control"
          :aria-label="t('console.zoomOut')"
          :title="t('console.zoomOut')"
          data-video-window-action="zoom-out"
          @click="zoom(0.9)"
        ><Minus :size="14" /></Button>
        <Button
          variant="ghost"
          size="icon-sm"
          class="video-window__control"
          :aria-label="t('console.zoomIn')"
          :title="t('console.zoomIn')"
          data-video-window-action="zoom-in"
          @click="zoom(1.1)"
        ><Plus :size="14" /></Button>
        <span class="video-window__separator" aria-hidden="true" />
        <Button
          variant="ghost"
          size="icon-sm"
          class="video-window__control"
          :class="{ 'is-active': layoutMode === 'fit' }"
          :aria-label="t('console.fitVideo')"
          :title="t('console.fitVideo')"
          data-video-window-action="fit"
          @click="applyMode('fit')"
        ><Scan :size="14" /></Button>
        <Button
          variant="ghost"
          size="sm"
          class="video-window__control video-window__control--actual"
          :class="{ 'is-active': layoutMode === 'actual' }"
          :aria-label="t('console.actualSize')"
          :title="t('console.actualSize')"
          data-video-window-action="actual"
          @click="applyMode('actual')"
        ><Focus :size="13" /><span>1:1</span></Button>
        <Button
          variant="ghost"
          size="icon-sm"
          class="video-window__control"
          :class="{ 'is-active': isMaximized }"
          :aria-label="isMaximized ? t('console.restoreVideoWindow') : t('console.maximizeVideoWindow')"
          :title="isMaximized ? t('console.restoreVideoWindow') : t('console.maximizeVideoWindow')"
          data-video-window-action="maximize"
          @click="toggleMaximize"
        >
          <Minimize2 v-if="isMaximized" :size="14" />
          <Maximize2 v-else :size="14" />
        </Button>
      </template>

      <div class="video-window__surface" :style="surfaceStyle" data-video-surface-frame>
        <slot />
      </div>

      <template #overlay>
        <button
          v-if="!isMaximized"
          type="button"
          class="video-window__resize"
          :aria-label="t('console.resizeVideoWindow')"
          :title="t('console.resizeVideoWindow')"
          data-video-window-resize-handle
          @pointerdown.stop="beginResize"
          @pointermove="updateResize"
          @pointerup="endResize"
          @pointercancel="endResize"
          @keydown="resizeWithKeyboard"
        ><MoveDiagonal2 :size="13" /></button>
      </template>
    </DraggableWindowFrame>
  </div>
</template>

<style scoped>
.video-window-workspace {
  position: relative;
  width: 100%;
  height: 100%;
  min-width: 0;
  min-height: 0;
  overflow: auto;
  scrollbar-color: color-mix(in oklch, var(--muted-foreground) 55%, transparent) transparent;
  scrollbar-width: thin;
}

.video-window.is-resizing {
  user-select: none;
  transition: none;
}

.video-window__separator {
  width: 1px;
  height: 16px;
  margin: 0 2px;
  background: var(--border);
}

.video-window__control {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  padding: 0;
  color: var(--muted-foreground);
}

.video-window__control.is-active {
  color: var(--accent-foreground);
  background: var(--accent);
}

.video-window__control--actual {
  width: 42px;
  padding: 0 6px;
  gap: 2px;
  font-size: 10px;
  font-variant-numeric: tabular-nums;
}

.video-window__surface {
  position: relative;
  min-width: 0;
  min-height: 0;
  overflow: hidden;
  background: #000;
}

.video-window__resize {
  position: absolute;
  z-index: 30;
  right: 0;
  bottom: 0;
  display: flex;
  align-items: flex-end;
  justify-content: flex-end;
  width: 24px;
  height: 24px;
  padding: 7px 3px 3px 7px;
  border: 0;
  color: color-mix(in oklch, var(--muted-foreground) 75%, transparent);
  background: linear-gradient(
    135deg,
    transparent 48%,
    color-mix(in oklch, var(--card) 88%, transparent) 49%
  );
  cursor: nwse-resize;
  touch-action: none;
}

.video-window__resize:hover,
.video-window__resize:focus-visible {
  color: var(--foreground);
  outline: 1px solid var(--ring);
  outline-offset: -1px;
}

@media (prefers-reduced-motion: reduce) {
  .video-window__control {
    transition: none;
  }
}
</style>
