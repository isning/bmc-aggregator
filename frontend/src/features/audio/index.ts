import { registerFeature } from '../registry'
import { defineAsyncComponent } from 'vue'
import { useAudioStore } from './store'
const AudioStatusCard = defineAsyncComponent(() => import('./AudioStatusCard.vue'))
const AudioConfigPopover = defineAsyncComponent(() => import('./AudioConfigPopover.vue'))


registerFeature({
  id: 'audio',
  label: 'Audio',
  store: () => useAudioStore(),
  components: {
    StatusCard: AudioStatusCard,
    ActionBar: [AudioConfigPopover],
  },
  onDeactivate: () => {
    const store = useAudioStore()
    store.reset()
  },
})
