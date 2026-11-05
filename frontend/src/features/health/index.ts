import { registerFeature } from '../registry'
import { defineAsyncComponent } from 'vue'
const HealthStatusCard = defineAsyncComponent(() => import('./HealthStatusCard.vue'))

registerFeature({
  id: 'health',
  label: 'Health',
  components: { StatusCard: HealthStatusCard },
})
