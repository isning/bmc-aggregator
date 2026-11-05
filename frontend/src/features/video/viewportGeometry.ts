import { moveBoundedWindow } from '../../lib/floatingWindowGeometry.ts'

export interface VideoDimensions {
  width: number
  height: number
}

export interface VideoWindowLayout {
  x: number
  y: number
  videoWidth: number
  videoHeight: number
  chromeHeight: number
  scale: number
}

export type VideoWindowLayoutMode = 'fit' | 'actual' | 'custom' | 'maximized'

export const VIDEO_WINDOW_CHROME_HEIGHT = 38
export const VIDEO_WINDOW_MIN_WIDTH = 240
export const VIDEO_WINDOW_WORKSPACE_INSET = 16
export const VIDEO_WINDOW_MAX_SCALE = 4

function finitePositive(value: number, fallback: number): number {
  return Number.isFinite(value) && value > 0 ? value : fallback
}

function normalizeDimensions(dimensions: VideoDimensions): VideoDimensions {
  return {
    width: finitePositive(dimensions.width, 1),
    height: finitePositive(dimensions.height, 1),
  }
}

function maximumScale(source: VideoDimensions, workspace: VideoDimensions, inset: number): number {
  const availableWidth = Math.max(1, workspace.width - inset * 2)
  const availableHeight = Math.max(1, workspace.height - VIDEO_WINDOW_CHROME_HEIGHT - inset * 2)
  return Math.min(availableWidth / source.width, availableHeight / source.height)
}

function minimumScale(source: VideoDimensions, workspace: VideoDimensions): number {
  const availableWidth = Math.max(1, workspace.width)
  return Math.min(VIDEO_WINDOW_MIN_WIDTH, availableWidth) / source.width
}

function clamp(value: number, minimum: number, maximum: number): number {
  if (maximum < minimum) return maximum
  return Math.min(maximum, Math.max(minimum, value))
}

function layoutAtScale(
  sourceInput: VideoDimensions,
  workspaceInput: VideoDimensions,
  requestedScale: number,
  center?: { x: number; y: number },
  allowedMaximumScale = VIDEO_WINDOW_MAX_SCALE,
): VideoWindowLayout {
  const source = normalizeDimensions(sourceInput)
  const workspace = normalizeDimensions(workspaceInput)
  const maxScale = finitePositive(allowedMaximumScale, VIDEO_WINDOW_MAX_SCALE)
  const minScale = Math.min(minimumScale(source, workspace), maxScale)
  const scale = clamp(finitePositive(requestedScale, maxScale), minScale, maxScale)
  const videoWidth = source.width * scale
  const videoHeight = source.height * scale
  const windowCenter = center ?? { x: workspace.width / 2, y: workspace.height / 2 }
  const layout: VideoWindowLayout = {
    x: windowCenter.x - videoWidth / 2,
    y: windowCenter.y - (videoHeight + VIDEO_WINDOW_CHROME_HEIGHT) / 2,
    videoWidth,
    videoHeight,
    chromeHeight: VIDEO_WINDOW_CHROME_HEIGHT,
    scale,
  }
  return moveVideoWindow(layout, workspace, 0, 0)
}

export function fitVideoWindow(
  source: VideoDimensions,
  workspace: VideoDimensions,
): VideoWindowLayout {
  const normalizedSource = normalizeDimensions(source)
  const normalizedWorkspace = normalizeDimensions(workspace)
  return layoutAtScale(
    normalizedSource,
    normalizedWorkspace,
    maximumScale(normalizedSource, normalizedWorkspace, VIDEO_WINDOW_WORKSPACE_INSET),
    undefined,
    maximumScale(normalizedSource, normalizedWorkspace, VIDEO_WINDOW_WORKSPACE_INSET),
  )
}

export function maximizeVideoWindow(
  sourceInput: VideoDimensions,
  workspaceInput: VideoDimensions,
): VideoWindowLayout {
  const source = normalizeDimensions(sourceInput)
  const workspace = normalizeDimensions(workspaceInput)
  const videoHeight = Math.max(1, workspace.height - VIDEO_WINDOW_CHROME_HEIGHT)
  return {
    x: 0,
    y: 0,
    videoWidth: workspace.width,
    videoHeight,
    chromeHeight: VIDEO_WINDOW_CHROME_HEIGHT,
    scale: Math.min(workspace.width / source.width, videoHeight / source.height),
  }
}

export function layoutForSourceChange(
  source: VideoDimensions,
  workspace: VideoDimensions,
  mode: VideoWindowLayoutMode,
  customScale: number,
  previous?: VideoWindowLayout,
): VideoWindowLayout {
  if (mode === 'maximized') return maximizeVideoWindow(source, workspace)
  const center = previous
    ? {
        x: previous.x + previous.videoWidth / 2,
        y: previous.y + (previous.videoHeight + previous.chromeHeight) / 2,
      }
    : undefined
  if (mode === 'fit') {
    if (!previous) return fitVideoWindow(source, workspace)
    const normalizedSource = normalizeDimensions(source)
    const previousVideoArea = previous.videoWidth * previous.videoHeight
    const areaPreservingScale = Math.sqrt(
      previousVideoArea / (normalizedSource.width * normalizedSource.height),
    )
    return layoutAtScale(
      normalizedSource,
      workspace,
      areaPreservingScale,
      center,
      maximumScale(normalizedSource, normalizeDimensions(workspace), VIDEO_WINDOW_WORKSPACE_INSET),
    )
  }

  return layoutAtScale(source, workspace, mode === 'actual' ? 1 : customScale, center)
}

export function layoutForWorkspaceChange(
  source: VideoDimensions,
  workspace: VideoDimensions,
  mode: VideoWindowLayoutMode,
  customScale: number,
  previous?: VideoWindowLayout,
): VideoWindowLayout {
  if (mode === 'maximized') return maximizeVideoWindow(source, workspace)
  if (mode === 'fit') return fitVideoWindow(source, workspace)
  return layoutForSourceChange(source, workspace, mode, customScale, previous)
}

export function resizeVideoWindow(
  sourceInput: VideoDimensions,
  workspaceInput: VideoDimensions,
  layout: VideoWindowLayout,
  deltaX: number,
  deltaY: number,
): VideoWindowLayout {
  const source = normalizeDimensions(sourceInput)
  const workspace = normalizeDimensions(workspaceInput)
  const projectedScaleDelta =
    (deltaX * source.width + deltaY * source.height)
    / (source.width * source.width + source.height * source.height)
  const maxScale = VIDEO_WINDOW_MAX_SCALE
  const minScale = Math.min(minimumScale(source, workspace), maxScale)
  const scale = clamp(layout.scale + projectedScaleDelta, minScale, maxScale)
  const resized: VideoWindowLayout = {
    ...layout,
    videoWidth: source.width * scale,
    videoHeight: source.height * scale,
    scale,
  }
  return moveVideoWindow(resized, workspace, 0, 0)
}

export function moveVideoWindow(
  layout: VideoWindowLayout,
  workspaceInput: VideoDimensions,
  deltaX: number,
  deltaY: number,
): VideoWindowLayout {
  const workspace = normalizeDimensions(workspaceInput)
  const position = moveBoundedWindow(
    layout,
    {
      width: layout.videoWidth,
      height: layout.videoHeight + layout.chromeHeight,
    },
    workspace,
    deltaX,
    deltaY,
  )
  return {
    ...layout,
    ...position,
  }
}
