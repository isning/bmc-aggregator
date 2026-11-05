<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, useAttrs, watch } from 'vue'

import { usePointerWindowMove } from '@/composables/usePointerWindowMove'
import {
  moveBoundedWindow,
  type WindowDimensions,
  type WindowPosition,
} from '@/lib/floatingWindowGeometry'

defineOptions({ inheritAttrs: false })

const props = withDefaults(defineProps<{
  modelValue: WindowPosition
  workspaceSize: WindowDimensions
  windowTitle: string
  windowSize?: WindowDimensions
  positioning?: 'absolute' | 'fixed' | 'relative'
  disabled?: boolean
  maximized?: boolean
  headerHeight?: number
}>(), {
  positioning: 'absolute',
  disabled: false,
  maximized: false,
  headerHeight: 38,
})

const emit = defineEmits<{
  (event: 'update:modelValue', value: WindowPosition): void
  (event: 'handleDblclick', value: MouseEvent): void
}>()

const attrs = useAttrs()
const frameRef = ref<HTMLElement | null>(null)
const measuredSize = ref<WindowDimensions>({ width: 0, height: 0 })
let resizeObserver: ResizeObserver | null = null

const effectiveWindowSize = computed(() => props.windowSize ?? measuredSize.value)
const effectiveHeaderHeight = computed(() => {
  const requestedHeight = Number.isFinite(props.headerHeight) ? props.headerHeight : 38
  return Math.min(64, Math.max(32, requestedHeight))
})
const canMove = computed(() => props.positioning !== 'relative' && !props.disabled)
const frameStyle = computed(() => {
  const positioningStyle = props.positioning === 'relative'
    ? { position: 'relative' as const }
    : {
        position: props.positioning,
        left: '0',
        top: '0',
        transform: `translate3d(${props.modelValue.x}px, ${props.modelValue.y}px, 0)`,
      }
  return [
    attrs.style,
    positioningStyle,
    { '--window-frame-header-height': `${effectiveHeaderHeight.value}px` },
  ]
})

function updateMeasuredSize() {
  const rect = frameRef.value?.getBoundingClientRect()
  if (!rect) return
  measuredSize.value = { width: rect.width, height: rect.height }
}

function constrainPosition() {
  if (props.positioning === 'relative') return
  const next = moveBoundedWindow(
    props.modelValue,
    effectiveWindowSize.value,
    props.workspaceSize,
    0,
    0,
  )
  if (next.x !== props.modelValue.x || next.y !== props.modelValue.y) {
    emit('update:modelValue', next)
  }
}

const { isMoving, beginMove, updateMove, endMove } = usePointerWindowMove<WindowPosition>({
  disabled: () => !canMove.value,
  snapshot: () => ({ ...props.modelValue }),
  move: (initialPosition, deltaX, deltaY) => {
    emit('update:modelValue', moveBoundedWindow(
      initialPosition,
      effectiveWindowSize.value,
      props.workspaceSize,
      deltaX,
      deltaY,
    ))
  },
})

watch(
  () => [
    props.workspaceSize.width,
    props.workspaceSize.height,
    effectiveWindowSize.value.width,
    effectiveWindowSize.value.height,
    props.positioning,
  ],
  () => void nextTick(constrainPosition),
)

onMounted(() => {
  updateMeasuredSize()
  resizeObserver = new ResizeObserver(() => {
    updateMeasuredSize()
    constrainPosition()
  })
  if (frameRef.value) resizeObserver.observe(frameRef.value)
  constrainPosition()
})

onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  resizeObserver = null
})
</script>

<template>
  <section
    v-bind="attrs"
    ref="frameRef"
    :class="[
      'draggable-window-frame',
      attrs.class,
      isMoving && 'is-moving',
      maximized && 'is-maximized',
      !canMove && 'is-move-disabled',
    ]"
    :style="frameStyle"
  >
    <header
      class="draggable-window-frame__bar"
      data-window-frame-handle
      @pointerdown="beginMove"
      @pointermove="updateMove"
      @pointerup="endMove"
      @pointercancel="endMove"
      @dblclick="emit('handleDblclick', $event)"
    >
      <div class="draggable-window-frame__identity">
        <span class="draggable-window-frame__icon" aria-hidden="true">
          <slot name="icon" />
        </span>
        <span class="draggable-window-frame__title">{{ windowTitle }}</span>
        <span v-if="$slots.meta" class="draggable-window-frame__meta">
          <slot name="meta" />
        </span>
      </div>
      <div
        class="draggable-window-frame__actions"
        @pointerdown.stop
        @dblclick.stop
      >
        <slot name="actions" />
      </div>
    </header>
    <slot />
    <slot name="overlay" />
  </section>
</template>

<style scoped>
.draggable-window-frame {
  --window-frame-header-height: 38px;

  z-index: 10;
  display: grid;
  grid-template-rows: var(--window-frame-header-height) minmax(0, 1fr);
  overflow: hidden;
  border: 0;
  border-radius: var(--radius);
  color: var(--card-foreground);
  background: var(--card);
  box-shadow:
    inset 0 0 0 1px var(--border),
    0 8px 24px rgb(15 23 42 / 0.14),
    0 1px 3px rgb(15 23 42 / 0.12);
  contain: layout paint;
  container-type: inline-size;
  will-change: transform, width, height;
}

.draggable-window-frame.is-maximized {
  border-radius: 0;
  box-shadow: inset 0 0 0 1px var(--border);
}

.draggable-window-frame.is-moving {
  user-select: none;
  transition: none;
}

.draggable-window-frame__bar {
  position: relative;
  z-index: 20;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  min-width: 0;
  height: var(--window-frame-header-height);
  padding: 0 5px 0 10px;
  color: var(--card-foreground);
  background: color-mix(in oklch, var(--card) 82%, var(--muted));
  border-bottom: 1px solid var(--border);
  cursor: grab;
  touch-action: none;
}

.draggable-window-frame__identity,
.draggable-window-frame__actions {
  display: flex;
  align-items: center;
}

.draggable-window-frame__identity {
  min-width: 0;
  gap: 7px;
}

.draggable-window-frame__icon {
  display: inline-flex;
  flex: 0 0 auto;
  color: var(--muted-foreground);
}

.draggable-window-frame__title {
  flex: 0 0 auto;
  color: var(--foreground);
  font-size: 12px;
  font-weight: 600;
  line-height: 1;
}

.draggable-window-frame__meta {
  display: flex;
  align-items: center;
  gap: 5px;
  min-width: 0;
  overflow: hidden;
  color: var(--muted-foreground);
  font-size: 11px;
  font-variant-numeric: tabular-nums;
  line-height: 1;
  white-space: nowrap;
}

.draggable-window-frame__actions {
  flex: 0 0 auto;
  gap: 2px;
}

.is-moving > .draggable-window-frame__bar {
  cursor: grabbing;
}

.is-move-disabled > .draggable-window-frame__bar {
  cursor: default;
}

@container (max-width: 500px) {
  .draggable-window-frame__meta {
    display: none;
  }
}

@container (max-width: 340px) {
  .draggable-window-frame__title {
    display: none;
  }

  .draggable-window-frame__identity {
    gap: 0;
  }
}

@media (prefers-reduced-motion: reduce) {
  .draggable-window-frame {
    transition: none;
  }
}
</style>
