import {
  type HidKeyboardEvent,
  type HidMouseEvent,
  type HidConsumerEvent,
  encodeKeyboardEvent,
  encodeMouseEvent,
  encodeConsumerEvent,
  RESP_OK,
  RESP_ERR_HID_UNAVAILABLE,
  RESP_ERR_INVALID_MESSAGE,
} from '@/types/hid'
import { useFeatureEventBus, type FeatureEventCodec } from '@/composables/useFeatureEventBus'

export type { HidKeyboardEvent, HidMouseEvent, HidConsumerEvent }

type HidActions =
  | { action: 'keyboard'; payload: HidKeyboardEvent }
  | { action: 'mouse'; payload: HidMouseEvent }
  | { action: 'consumer'; payload: HidConsumerEvent }

const hidCodec: FeatureEventCodec<HidActions> = {
  responseOk: RESP_OK,
  responseUnavailable: RESP_ERR_HID_UNAVAILABLE,
  responseInvalidMessage: RESP_ERR_INVALID_MESSAGE,
  encode(event) {
    switch (event.action) {
      case 'keyboard':
        return encodeKeyboardEvent(event.payload)
      case 'mouse':
        return encodeMouseEvent(event.payload)
      case 'consumer':
        return encodeConsumerEvent(event.payload)
    }
  },
}

export function useHidEventBus() {
  const bus = useFeatureEventBus<HidActions>('hid', '/api/feature/hid/ws', hidCodec)

  return {
    connected: bus.connected,
    reconnectAttempts: bus.reconnectAttempts,
    networkError: bus.networkError,
    networkErrorMessage: bus.networkErrorMessage,
    lastSendError: bus.lastSendError,
    droppedEvents: bus.droppedEvents,
    hidUnavailable: bus.unavailable,
    connect: bus.connect,
    disconnect: bus.disconnect,
    sendKeyboard: (payload: HidKeyboardEvent) => bus.send({ action: 'keyboard', payload }),
    sendMouse: (payload: HidMouseEvent) => bus.send({ action: 'mouse', payload }),
    sendConsumer: (payload: HidConsumerEvent) => bus.send({ action: 'consumer', payload }),
  }
}
