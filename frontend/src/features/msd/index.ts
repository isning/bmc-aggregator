import { registerFeature } from '../registry'
import { defineAsyncComponent } from 'vue'
import { useMsdStore } from './store'
const MsdStatusCard = defineAsyncComponent(() => import('./MsdStatusCard.vue'))
const MsdSheet = defineAsyncComponent(() => import('./MsdSheet.vue'))


registerFeature({
  id: 'msd',
  label: 'MSD',
  store: () => useMsdStore(),
  components: {
    StatusCard: MsdStatusCard,
    Overlay: [MsdSheet],
  },
  onDeactivate: () => {
    const store = useMsdStore()
    store.reset()
  },
})
