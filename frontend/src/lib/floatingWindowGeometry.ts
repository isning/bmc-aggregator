export interface WindowPosition {
  x: number
  y: number
}

export interface WindowDimensions {
  width: number
  height: number
}

function finiteNonNegative(value: number): number {
  return Number.isFinite(value) ? Math.max(0, value) : 0
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, value))
}

export function moveBoundedWindow(
  position: WindowPosition,
  windowSize: WindowDimensions,
  workspaceSize: WindowDimensions,
  deltaX: number,
  deltaY: number,
): WindowPosition {
  const workspaceWidth = finiteNonNegative(workspaceSize.width)
  const workspaceHeight = finiteNonNegative(workspaceSize.height)
  const width = finiteNonNegative(windowSize.width)
  const height = finiteNonNegative(windowSize.height)

  return {
    x: clamp(position.x + deltaX, 0, Math.max(0, workspaceWidth - width)),
    y: clamp(position.y + deltaY, 0, Math.max(0, workspaceHeight - height)),
  }
}
