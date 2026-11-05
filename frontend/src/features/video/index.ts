import { registerFeature } from '../registry'
import { defineAsyncComponent } from 'vue'
import { useVideoStore } from './store'
const VideoStatusCard = defineAsyncComponent(() => import('./VideoStatusCard.vue'))
const VideoConfigPopover = defineAsyncComponent(() => import('./VideoConfigPopover.vue'))
const StatsSheet = defineAsyncComponent(() => import('./StatsSheet.vue'))

export type { VideoMode } from './store'
export { useVideoStore } from './store'
export { useVideoStream } from './useVideoStream'
export { default as VideoConsoleWindow } from './VideoConsoleWindow.vue'

registerFeature({
  id: 'video',
  label: 'Video',
  store: () => useVideoStore(),
  components: {
    StatusCard: VideoStatusCard,
    ActionBar: [VideoConfigPopover],
    Overlay: [StatsSheet],
  },
  onActivate: async () => {
    const store = useVideoStore()
    await store.fetchConstraints()
  },
  onDeactivate: () => {
    const store = useVideoStore()
    store.reset()
  },
})
