/**
 * Browser-facing video lifecycle. Transport implementations keep their own
 * connection state; this state machine describes only what the console can
 * safely present to the operator.
 */
export type VideoLifecyclePhase =
  | 'connecting'
  | 'waiting_first_frame'
  | 'streaming'
  | 'no_signal'
  | 'device_lost'
  | 'device_busy'
  | 'error'
  | 'idle'

export type VideoLifecycleEvent =
  | { type: 'connect_requested' }
  | { type: 'source_ready' }
  | { type: 'frame_received' }
  | { type: 'signal'; state: 'streaming' | 'no_signal' | 'device_lost' | 'device_busy'; reason?: string | null }
  | { type: 'switch_requested' }
  | { type: 'fault'; message: string }
  | { type: 'stopped' }

export interface VideoLifecycleState {
  phase: VideoLifecyclePhase
  reason: string | null
  error: string | null
}

export interface VideoLifecyclePresentation {
  mountSurface: boolean
  showSurface: boolean
  showLoading: boolean
  showSignalOverlay: boolean
  showError: boolean
}

export const initialVideoLifecycle: VideoLifecycleState = {
  phase: 'connecting',
  reason: null,
  error: null,
}

export function transitionVideoLifecycle(
  current: VideoLifecycleState,
  event: VideoLifecycleEvent,
): VideoLifecycleState {
  switch (event.type) {
    case 'connect_requested':
      return { phase: 'connecting', reason: null, error: null }
    case 'source_ready':
      return current.phase === 'connecting' || current.phase === 'waiting_first_frame'
        ? { phase: 'waiting_first_frame', reason: null, error: null }
        : current
    case 'frame_received':
      if (current.phase === 'no_signal' || current.phase === 'device_lost' || current.phase === 'device_busy') {
        return current
      }
      return { phase: 'streaming', reason: null, error: null }
    case 'signal':
      if (event.state === 'streaming') {
        return current.phase === 'streaming'
          ? current
          : { phase: 'waiting_first_frame', reason: null, error: null }
      }
      return { phase: event.state, reason: event.reason ?? null, error: null }
    case 'switch_requested':
      return { phase: 'connecting', reason: 'mode_switching', error: null }
    case 'fault':
      return { phase: 'error', reason: null, error: event.message }
    case 'stopped':
      return { phase: 'idle', reason: null, error: null }
  }
}

export function presentVideoLifecycle(state: VideoLifecycleState): VideoLifecyclePresentation {
  const mountSurface = state.phase === 'connecting'
    || state.phase === 'waiting_first_frame'
    || state.phase === 'streaming'
  return {
    mountSurface,
    showSurface: state.phase === 'streaming',
    showLoading: state.phase === 'connecting' || state.phase === 'waiting_first_frame',
    showSignalOverlay: state.phase === 'no_signal' || state.phase === 'device_lost' || state.phase === 'device_busy',
    showError: state.phase === 'error',
  }
}
