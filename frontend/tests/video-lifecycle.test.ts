import assert from 'node:assert/strict'
import {
  initialVideoLifecycle,
  presentVideoLifecycle,
  transitionVideoLifecycle,
  type VideoLifecycleEvent,
  type VideoLifecycleState,
} from '../src/features/video/lifecycle.ts'

function apply(events: VideoLifecycleEvent[]): VideoLifecycleState {
  return events.reduce(transitionVideoLifecycle, initialVideoLifecycle)
}

const streaming = apply([
  { type: 'connect_requested' },
  { type: 'source_ready' },
  { type: 'frame_received' },
])
assert.equal(streaming.phase, 'streaming')
assert.equal(presentVideoLifecycle(streaming).showSurface, true)

const noSignal = transitionVideoLifecycle(streaming, {
  type: 'signal',
  state: 'no_signal',
  reason: 'bmc_video_stopped',
})
assert.equal(noSignal.phase, 'no_signal')
assert.equal(presentVideoLifecycle(noSignal).showSignalOverlay, true)
assert.equal(presentVideoLifecycle(noSignal).mountSurface, false)

// Cumulative WebRTC/WebTransport counters must not override an authoritative
// source signal. Recovery requires source streaming followed by a fresh frame.
assert.equal(transitionVideoLifecycle(noSignal, { type: 'frame_received' }).phase, 'no_signal')
const sourceRecovered = transitionVideoLifecycle(noSignal, {
  type: 'signal',
  state: 'streaming',
})
assert.equal(sourceRecovered.phase, 'waiting_first_frame')
assert.equal(presentVideoLifecycle(sourceRecovered).mountSurface, true)
assert.equal(transitionVideoLifecycle(sourceRecovered, { type: 'frame_received' }).phase, 'streaming')

const failed = transitionVideoLifecycle(streaming, { type: 'fault', message: 'failed' })
assert.equal(presentVideoLifecycle(failed).showError, true)
