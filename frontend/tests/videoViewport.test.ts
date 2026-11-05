import assert from 'node:assert/strict'
import test from 'node:test'

import {
  fitVideoWindow,
  maximizeVideoWindow,
  layoutForSourceChange,
  layoutForWorkspaceChange,
  resizeVideoWindow,
  moveVideoWindow,
  type VideoWindowLayout,
} from '../src/features/video/viewportGeometry.ts'
import { moveBoundedWindow } from '../src/lib/floatingWindowGeometry.ts'

const workspace = { width: 1200, height: 700 }

test('fit preserves the source aspect ratio inside the workspace', () => {
  const layout = fitVideoWindow({ width: 1024, height: 768 }, workspace)
  assert.ok(layout.videoWidth <= workspace.width)
  assert.ok(layout.videoHeight + layout.chromeHeight <= workspace.height)
  assert.ok(Math.abs(layout.videoWidth / layout.videoHeight - 4 / 3) < 0.0001)
})

test('a source resolution change recomputes both window dimensions', () => {
  const initial = fitVideoWindow({ width: 800, height: 600 }, workspace)
  const changed = layoutForSourceChange(
    { width: 1920, height: 1080 },
    workspace,
    'fit',
    initial.scale,
    initial,
  )
  assert.notEqual(changed.videoWidth, initial.videoWidth)
  assert.notEqual(changed.videoHeight, initial.videoHeight)
  assert.ok(Math.abs(changed.videoWidth / changed.videoHeight - 16 / 9) < 0.0001)
})

test('manual resize scales without distorting the source', () => {
  const initial = fitVideoWindow({ width: 1280, height: 720 }, workspace)
  const smaller = resizeVideoWindow(
    { width: 1280, height: 720 },
    workspace,
    initial,
    -180,
    -100,
  )
  assert.ok(smaller.scale < initial.scale)
  assert.ok(Math.abs(smaller.videoWidth / smaller.videoHeight - 16 / 9) < 0.0001)
})

test('dragging keeps the complete floating window reachable', () => {
  const initial: VideoWindowLayout = {
    x: 100,
    y: 100,
    videoWidth: 640,
    videoHeight: 480,
    chromeHeight: 38,
    scale: 1,
  }
  const moved = moveVideoWindow(initial, workspace, 5000, -5000)
  assert.equal(moved.y, 0)
  assert.equal(moved.x, workspace.width - moved.videoWidth)
})

test('generic floating windows use the same workspace boundary rules as video', () => {
  const moved = moveBoundedWindow(
    { x: 100, y: 100 },
    { width: 640, height: 480 },
    workspace,
    5000,
    -5000,
  )
  assert.deepEqual(moved, { x: 560, y: 0 })
})

test('fit mode re-centers when the workspace size changes', () => {
  const initial = fitVideoWindow({ width: 1024, height: 768 }, workspace)
  const narrowerWorkspace = { width: 900, height: 500 }
  const changed = layoutForWorkspaceChange(
    { width: 1024, height: 768 },
    narrowerWorkspace,
    'fit',
    initial.scale,
    initial,
  )
  assert.ok(Math.abs(changed.x - (narrowerWorkspace.width - changed.videoWidth) / 2) < 0.0001)
  assert.ok(
    Math.abs(
      changed.y
      - (narrowerWorkspace.height - changed.videoHeight - changed.chromeHeight) / 2,
    ) < 0.0001,
  )
})

test('actual size and custom zoom may exceed the workspace without distortion', () => {
  const initial = fitVideoWindow({ width: 1024, height: 768 }, workspace)
  const actual = layoutForSourceChange(
    { width: 1024, height: 768 },
    workspace,
    'actual',
    initial.scale,
    initial,
  )
  assert.equal(actual.scale, 1)
  assert.equal(actual.videoWidth, 1024)
  assert.equal(actual.videoHeight, 768)

  const zoomed = layoutForSourceChange(
    { width: 1024, height: 768 },
    workspace,
    'custom',
    initial.scale * 1.1,
    initial,
  )
  assert.ok(zoomed.scale > initial.scale)
  assert.ok(Math.abs(zoomed.videoWidth / zoomed.videoHeight - 4 / 3) < 0.0001)
})

test('maximize occupies the complete workspace while preserving the rendered content scale', () => {
  const maximized = maximizeVideoWindow({ width: 1024, height: 768 }, workspace)
  assert.equal(maximized.x, 0)
  assert.equal(maximized.y, 0)
  assert.equal(maximized.videoWidth, workspace.width)
  assert.equal(maximized.videoHeight + maximized.chromeHeight, workspace.height)
  assert.equal(maximized.scale, Math.min(1200 / 1024, (700 - 38) / 768))
})
