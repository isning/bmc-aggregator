import { ref } from 'vue'

interface PointerMoveInteraction<T> {
  pointerId: number
  startX: number
  startY: number
  initial: T
}

interface PointerWindowMoveOptions<T> {
  disabled?: () => boolean
  snapshot: () => T | null
  move: (initial: T, deltaX: number, deltaY: number) => void
}

export function usePointerWindowMove<T>(options: PointerWindowMoveOptions<T>) {
  const isMoving = ref(false)
  let interaction: PointerMoveInteraction<T> | null = null

  function beginMove(event: PointerEvent) {
    if (event.button !== 0 || options.disabled?.()) return
    const initial = options.snapshot()
    if (initial === null) return

    event.preventDefault()
    const handle = event.currentTarget as HTMLElement
    handle.setPointerCapture(event.pointerId)
    interaction = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      initial,
    }
    isMoving.value = true
  }

  function updateMove(event: PointerEvent) {
    if (!interaction || event.pointerId !== interaction.pointerId) return
    options.move(
      interaction.initial,
      event.clientX - interaction.startX,
      event.clientY - interaction.startY,
    )
  }

  function endMove(event: PointerEvent) {
    if (!interaction || event.pointerId !== interaction.pointerId) return
    const handle = event.currentTarget as HTMLElement
    if (handle.hasPointerCapture(event.pointerId)) {
      handle.releasePointerCapture(event.pointerId)
    }
    interaction = null
    isMoving.value = false
  }

  return { isMoving, beginMove, updateMove, endMove }
}
