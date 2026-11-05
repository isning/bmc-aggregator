<script setup lang="ts">
import { onMounted } from 'vue'
import { RouterLink } from 'vue-router'
import { ArrowRight, Plus, Settings2 } from 'lucide-vue-next'
import AppLayout from '@/components/AppLayout.vue'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useDeviceConfigurationStore } from '@/stores/deviceConfiguration'
import { humanize } from '@/configuration/catalog'
import { useConfigurationEvents } from '@/configuration/ws'

const store = useDeviceConfigurationStore()
let initialLoadCompleted = false
onMounted(async () => {
  try {
    if (!store.catalog) await store.loadCatalog()
    await store.loadDevices()
    initialLoadCompleted = true
  } catch {
  }
})

useConfigurationEvents(event => {
  const inventoryChanged = event.scope?.kind === 'global'
    && event.scope.section === 'devices'
  const needsAuthoritativeReload = event.type === 'configuration.resync_required'
    || inventoryChanged
    || (event.type === 'configuration.ready' && initialLoadCompleted)
  if (needsAuthoritativeReload) {
    void store.loadDevices().catch(() => undefined)
  }
})
</script>

<template>
  <AppLayout>
    <div class="h-full overflow-auto bg-muted/20">
      <div class="mx-auto max-w-5xl space-y-8 p-4 sm:p-6 lg:p-10">
        <header class="flex flex-wrap items-end justify-between gap-4">
          <div><p class="text-[11px] font-semibold uppercase tracking-[0.18em] text-muted-foreground">Inventory</p><h1 class="mt-2 text-2xl font-semibold tracking-tight">Managed devices</h1><p class="mt-2 text-sm text-muted-foreground">Choose a device to open its console or configuration.</p></div>
          <Button as-child><RouterLink :to="{ name: 'DeviceCreate' }"><Plus class="mr-2 h-4 w-4" /> Add device</RouterLink></Button>
        </header>
        <div v-if="store.error" class="rounded-md border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive">{{ store.error }}</div>
        <div v-if="store.loading && !store.devices.length" class="py-16 text-center text-sm text-muted-foreground">Loading devices...</div>
        <div v-else-if="!store.devices.length" class="border-y py-16 text-center"><p class="font-medium">No devices configured</p><p class="mt-2 text-sm text-muted-foreground">Create an explicit device configuration to begin.</p><Button class="mt-5" as-child><RouterLink :to="{ name: 'DeviceCreate' }">Create first device</RouterLink></Button></div>
        <div v-else class="divide-y rounded-lg border bg-background">
          <div v-for="device in store.devices" :key="device.id" class="flex flex-wrap items-center gap-4 px-4 py-4 sm:px-5">
            <div class="min-w-0 flex-1"><div class="flex items-center gap-2"><h2 class="truncate font-medium">{{ device.name || device.id }}</h2><Badge variant="outline" class="font-mono text-[10px]">{{ device.id }}</Badge></div><p class="mt-1 truncate text-sm text-muted-foreground">{{ device.endpoint.host }}:{{ device.endpoint.port }}</p><div class="mt-2 flex flex-wrap gap-1"><Badge v-for="(assignment, featureId) in device.assignments" :key="featureId" variant="secondary" class="text-[10px]">{{ humanize(featureId) }} · {{ assignment.kind }}</Badge></div></div>
            <div class="flex items-center gap-1"><Button variant="ghost" size="icon" as-child :aria-label="`Open ${device.name || device.id} console`"><RouterLink :to="{ name: 'Console', params: { bmcId: device.id } }"><ArrowRight class="h-4 w-4" /></RouterLink></Button><Button variant="ghost" size="icon" as-child aria-label="Open device settings"><RouterLink :to="{ name: 'DeviceSettings', params: { bmcId: device.id } }"><Settings2 class="h-4 w-4" /></RouterLink></Button></div>
          </div>
        </div>
      </div>
    </div>
  </AppLayout>
</template>
