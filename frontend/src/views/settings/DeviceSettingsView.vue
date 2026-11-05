<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { onBeforeRouteLeave, RouterLink, useRoute, useRouter } from 'vue-router'
import { AlertTriangle, ChevronRight, Save, Trash2 } from 'lucide-vue-next'
import AppLayout from '@/components/AppLayout.vue'
import SettingsLayout from '@/components/settings/SettingsLayout.vue'
import FeatureAssignmentEditor from '@/components/settings/FeatureAssignmentEditor.vue'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useDeviceConfigurationStore } from '@/stores/deviceConfiguration'
import { useConfigurationEvents, type ConfigurationEvent } from '@/configuration/ws'
import { ApiError } from '@/api/request'
import { featureTitle, type FeatureAssignmentMutation } from '@/configuration/catalog'
import { featureAssignmentToMutation } from '@/configuration/assignment'

type Section = 'overview' | 'features' | 'requirements' | 'advanced'
const route = useRoute()
const router = useRouter()
const store = useDeviceConfigurationStore()
const active = ref<Section>((typeof route.query.section === 'string' && ['overview', 'features', 'requirements', 'advanced'].includes(route.query.section) ? route.query.section : 'overview') as Section)
const activeFeatureId = ref<string | null>(null)
const name = ref('')
const host = ref('')
const port = ref(443)
const username = ref('')
const password = ref('')
const assignments = ref<Record<string, FeatureAssignmentMutation>>({})
const dirty = ref(false)
const saving = ref(false)
const deleting = ref(false)
const error = ref<string | null>(null)
let initialLoadCompleted = false
let draftRevision = 0

const sections = [
  { id: 'overview', label: 'Overview' },
  { id: 'features', label: 'Features' },
  { id: 'requirements', label: 'Requirements' },
  { id: 'advanced', label: 'Advanced' },
]

const deviceId = computed(() => String(route.params.bmcId))
const catalogFeatures = computed(() => store.catalog?.features ?? [])
const activeFeature = computed(() => catalogFeatures.value.find(feature => feature.feature_id === activeFeatureId.value) ?? catalogFeatures.value[0])

function copy<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

function loadForm() {
  const device = store.selected
  if (!device || device.id !== deviceId.value) return
  name.value = device.name
  host.value = device.endpoint.host
  port.value = device.endpoint.port
  username.value = device.credentials.username
  password.value = ''
  assignments.value = Object.fromEntries(
    Object.entries(device.assignments).map(([id, assignment]) => [id, featureAssignmentToMutation(assignment)]),
  )
  dirty.value = false
}

function markDirty() {
  draftRevision++
  dirty.value = true
}

function setAssignment(featureId: string, assignment: FeatureAssignmentMutation) {
  assignments.value[featureId] = assignment
  markDirty()
}

function selectFeature(id: string) {
  activeFeatureId.value = id
  active.value = 'features'
}

async function refreshFromEvent(event: ConfigurationEvent) {
  if (saving.value) return
  if (event.type === 'configuration.ready' && !initialLoadCompleted) return
  if (event.type === 'configuration.resync_required' || event.type === 'configuration.ready') {
    if (dirty.value) {
      await store.loadDevice(deviceId.value).catch(() => undefined)
      error.value = 'Configuration changed elsewhere. Your local edits are preserved; review them before saving.'
    } else {
      await load()
    }
    return
  }
  if (event.scope?.kind === 'options' && event.scope.resource_id) {
    await store.loadOptions(event.scope.resource_id).catch(() => undefined)
    return
  }
  const isDeviceChange = event.scope?.kind === 'device' && event.scope.id === deviceId.value
  const isFeatureChange = event.scope?.kind === 'feature' && event.scope.device_id === deviceId.value
  if (isDeviceChange || isFeatureChange) {
    await refreshFromEvent({ ...event, type: 'configuration.resync_required' })
  }
}

async function save() {
  if (saving.value || deleting.value || store.selected?.id !== deviceId.value) return
  const owner = deviceId.value
  const submittedRevision = draftRevision
  saving.value = true
  error.value = null
  try {
    const mutation = {
      name: name.value.trim(),
      endpoint: { host: host.value.trim(), port: Number(port.value) },
      credentials: {
        username: username.value.trim(),
        password: password.value ? { operation: 'replace' as const, value: password.value } : { operation: 'keep' as const },
      },
      assignments: copy(assignments.value),
    }
    await store.update(owner, mutation)
    if (deviceId.value === owner && draftRevision === submittedRevision) loadForm()
  } catch (reason) {
    if (deviceId.value !== owner) return
    if (reason instanceof ApiError && reason.status === 409) {
      await store.loadDevice(owner).catch(() => undefined)
      if (deviceId.value !== owner) return
      dirty.value = true
      error.value = 'Configuration changed elsewhere. Your local edits are preserved; review them and save again.'
    } else {
      error.value = reason instanceof Error ? reason.message : String(reason)
    }
  } finally {
    saving.value = false
  }
}

async function remove() {
  if (saving.value || deleting.value) return
  const owner = deviceId.value
  if (!window.confirm(`Delete ${deviceId.value}?`)) return
  deleting.value = true
  error.value = null
  try {
    await store.remove(owner)
    if (deviceId.value === owner) await router.push({ name: 'Devices' })
  } catch (reason) {
    if (deviceId.value === owner) error.value = reason instanceof Error ? reason.message : String(reason)
  } finally {
    deleting.value = false
  }
}

let loadSerial = 0
async function load() {
  const serial = ++loadSerial
  const owner = deviceId.value
  error.value = null
  try {
    if (!store.catalog) await store.loadCatalog()
    if (serial !== loadSerial || owner !== deviceId.value) return
    await store.loadDevice(owner)
    await Promise.all((store.catalog?.option_resources ?? []).map(resource => store.loadOptions(resource.resource_id)))
    if (serial !== loadSerial || owner !== deviceId.value || dirty.value) return
    activeFeatureId.value ??= catalogFeatures.value[0]?.feature_id ?? null
    loadForm()
  } catch (reason) {
    if (serial === loadSerial && owner === deviceId.value) error.value = reason instanceof Error ? reason.message : String(reason)
  }
}

onMounted(async () => {
  await load()
  initialLoadCompleted = true
})
watch(deviceId, () => {
  dirty.value = false
  void load()
})
useConfigurationEvents(refreshFromEvent)
onBeforeRouteLeave(() => {
  if (dirty.value && !window.confirm('Discard unsaved configuration changes?')) return false
})
</script>

<template>
  <AppLayout>
    <SettingsLayout :title="store.selected?.name || deviceId" description="Explicit feature assignments and connection details." :sections="sections" :active="active" @select="active = $event as Section">
      <div v-if="error" class="mb-5 rounded-md border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive" role="alert">{{ error }}</div>
      <div v-if="store.loading && store.selected?.id !== deviceId" class="py-16 text-center text-sm text-muted-foreground">Loading configuration...</div>
      <div v-else-if="store.selected?.id !== deviceId" class="border-y py-16 text-center">
        <p class="font-medium">Device configuration is unavailable</p>
        <p class="mt-2 text-sm text-muted-foreground">This device may have been removed or renamed in another session.</p>
        <Button class="mt-5" as-child><RouterLink :to="{ name: 'Devices' }">Back to devices</RouterLink></Button>
      </div>

      <section v-else-if="active === 'overview'" class="space-y-6">
        <header><h2 class="text-xl font-semibold tracking-tight">Overview</h2><p class="mt-1 text-sm text-muted-foreground">Identity and primary management endpoint.</p></header>
        <div class="grid gap-5 border-y py-5 sm:grid-cols-2">
          <div class="space-y-2"><Label for="device-id">Device ID</Label><Input id="device-id" :model-value="deviceId" disabled /></div>
          <div class="space-y-2"><Label for="device-name">Display name</Label><Input id="device-name" v-model="name" @update:model-value="markDirty" /></div>
          <div class="space-y-2"><Label for="device-host">Host</Label><Input id="device-host" v-model="host" @update:model-value="markDirty" /></div>
          <div class="space-y-2"><Label for="device-port">Port</Label><Input id="device-port" v-model.number="port" type="number" min="1" max="65535" @update:model-value="markDirty" /></div>
          <div class="space-y-2"><Label for="device-username">Username</Label><Input id="device-username" v-model="username" autocomplete="username" @update:model-value="markDirty" /></div>
          <div class="space-y-2"><Label for="device-password">Password</Label><Input id="device-password" v-model="password" type="password" autocomplete="new-password" placeholder="Leave blank to keep configured password" @update:model-value="markDirty" /></div>
        </div>
        <div class="flex flex-wrap items-center gap-3"><Button :disabled="!dirty || saving" type="button" @click="save"><Save class="mr-2 h-4 w-4" /> Save overview</Button><Badge v-if="!dirty" variant="outline">Applied</Badge><span class="text-xs text-muted-foreground">ETag protects this device from lost updates.</span></div>
        <div class="border-t pt-6"><Button variant="destructive" :disabled="deleting" type="button" @click="remove"><Trash2 class="mr-2 h-4 w-4" /> Delete device</Button></div>
      </section>

      <section v-else-if="active === 'features'" class="space-y-5">
        <header><h2 class="text-xl font-semibold tracking-tight">Features</h2><p class="mt-1 text-sm text-muted-foreground">Assignments are described by the backend catalog. Select a row to edit its providers and primitive connection fields.</p></header>
        <div class="grid gap-2 md:grid-cols-[minmax(210px,0.34fr)_minmax(0,1fr)]">
          <div class="divide-y rounded-lg border bg-background">
            <button v-for="feature in catalogFeatures" :key="feature.feature_id" type="button" class="flex w-full items-center justify-between px-3 py-3 text-left text-sm" :class="activeFeature?.feature_id === feature.feature_id ? 'bg-accent text-accent-foreground' : 'hover:bg-muted/50'" @click="selectFeature(feature.feature_id)">
              <span>{{ featureTitle(feature) }}</span><ChevronRight class="h-4 w-4 text-muted-foreground" />
            </button>
          </div>
          <div class="min-w-0 rounded-lg border bg-background p-4 sm:p-6">
            <FeatureAssignmentEditor v-if="activeFeature" :catalog="store.catalog!" :feature="activeFeature" :assignment="assignments[activeFeature.feature_id] ?? { kind: 'disabled' }" :configured-assignment="store.selected?.assignments[activeFeature.feature_id]" :option-snapshots="store.options" @update="setAssignment(activeFeature.feature_id, $event)" />
            <div v-else class="text-sm text-muted-foreground">No catalog features are registered.</div>
            <div class="mt-6 flex items-center gap-3 border-t pt-4"><Button :disabled="!dirty || saving" type="button" @click="save"><Save class="mr-2 h-4 w-4" /> Save feature assignments</Button><Badge v-if="!dirty" variant="outline">Applied</Badge></div>
          </div>
        </div>
      </section>

      <section v-else-if="active === 'requirements'" class="space-y-6">
        <header><h2 class="text-xl font-semibold tracking-tight">Requirements</h2><p class="mt-1 text-sm text-muted-foreground">Backend prerequisites are evaluated by the selected feature adapters.</p></header>
        <div class="flex items-start gap-3 border-y py-5"><AlertTriangle class="mt-0.5 h-5 w-5 text-muted-foreground" /><div><p class="font-medium">Open the device console to inspect live requirements.</p><p class="mt-1 text-sm text-muted-foreground">The requirements endpoint remains authoritative and is intentionally separate from saved configuration.</p></div></div>
      </section>

      <section v-else class="space-y-6">
        <header><h2 class="text-xl font-semibold tracking-tight">Advanced</h2><p class="mt-1 text-sm text-muted-foreground">Low-frequency reconnect policy fields are exposed by backend descriptors when available.</p></header>
        <div class="border-y py-5 text-sm text-muted-foreground">Select a feature to edit its advanced backend fields. Runtime operations do not belong in this page.</div>
      </section>
    </SettingsLayout>
  </AppLayout>
</template>
