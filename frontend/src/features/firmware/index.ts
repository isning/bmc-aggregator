import { registerFeature } from '../registry'
import { defineAsyncComponent } from 'vue'
const FirmwareStatusCard = defineAsyncComponent(() => import('./FirmwareStatusCard.vue'))

registerFeature({
  id: 'firmware',
  label: 'Firmware',
  components: { StatusCard: FirmwareStatusCard },
})
