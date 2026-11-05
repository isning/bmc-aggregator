import { registerFeature } from '../registry'
import { defineAsyncComponent } from 'vue'
import { useHidStore } from './store'
const HidStatusCard = defineAsyncComponent(() => import('./HidStatusCard.vue'))
const HidConfigPopover = defineAsyncComponent(() => import('./HidConfigPopover.vue'))

export { useHidInput } from './useHidInput'
export { useHidEventBus } from './eventBus'
export { useHidStore } from './store'

registerFeature({
  id: 'hid',
  label: 'HID',
  store: () => useHidStore(),
  components: {
    StatusCard: HidStatusCard,
    ActionBar: [HidConfigPopover],
  },
  onActivate: () => {
    const store = useHidStore()
    store.syncMouseModeFromConfig()
  },
  onDeactivate: () => {
    const store = useHidStore()
    store.reset()
  },
})
