<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { Check, ChevronRight, Plus } from 'lucide-vue-next'
import AppLayout from '@/components/AppLayout.vue'
import SettingsLayout from '@/components/settings/SettingsLayout.vue'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { useDeviceConfigurationStore } from '@/stores/deviceConfiguration'
import { configurationApi, type DeviceConfigurationMutation } from '@/configuration/catalog'

const router = useRouter()
const store = useDeviceConfigurationStore()
const profileId = ref('')
const id = ref('')
const name = ref('')
const host = ref('')
const port = ref(443)
const username = ref('root')
const password = ref('')
const draft = ref<DeviceConfigurationMutation | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)
const sections = [{ id: 'identity', label: 'New device' }, { id: 'review', label: 'Review draft' }]
const active = ref('identity')
let disposed = false
let draftRequest = 0
watch([profileId, id, name, host, port, username], () => {
  ++draftRequest
  draft.value = null
  active.value = 'identity'
}, { flush: 'sync' })
onBeforeUnmount(() => {
  disposed = true
  ++draftRequest
})
const profile = computed(() => store.catalog?.creation_profiles.find(candidate => candidate.profile_id === profileId.value))

async function preview() {
  if (disposed || loading.value || !profileId.value || !id.value.trim() || !name.value.trim() || !host.value.trim()) return
  const request = ++draftRequest
  loading.value = true
  error.value = null
  try {
    const response = await configurationApi.creationDraft(profileId.value, { id: id.value.trim(), name: name.value.trim(), host: host.value.trim(), port: port.value, username: username.value.trim() })
    if (disposed || request !== draftRequest) return
    draft.value = response.draft
    active.value = 'review'
  } catch (reason) {
    if (!disposed && request === draftRequest) error.value = reason instanceof Error ? reason.message : String(reason)
  } finally {
    loading.value = false
  }
}

async function create() {
  if (disposed || loading.value || !draft.value) return
  loading.value = true
  error.value = null
  try {
    const mutation = JSON.parse(JSON.stringify(draft.value)) as DeviceConfigurationMutation
    mutation.credentials = {
      ...(mutation.credentials ?? {}),
      password: password.value ? { operation: 'replace', value: password.value } : { operation: 'keep' },
    }
    const device = await store.create(mutation)
    if (!disposed) await router.push({ name: 'DeviceSettings', params: { bmcId: device.id } })
  } catch (reason) {
    if (!disposed) error.value = reason instanceof Error ? reason.message : String(reason)
  } finally {
    loading.value = false
  }
}

onMounted(async () => {
  if (!store.catalog) {
    try { await store.loadCatalog() } catch (reason) { error.value = reason instanceof Error ? reason.message : String(reason) }
  }
})
</script>

<template>
  <AppLayout>
    <SettingsLayout title="Add device" description="Create an explicit configuration from a server-described profile." :sections="sections" :active="active" @select="active = $event">
      <div v-if="error" class="mb-5 rounded-md border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive" role="alert">{{ error }}</div>
      <section v-if="active === 'identity'" class="space-y-6">
        <header><h2 class="text-xl font-semibold tracking-tight">Device identity</h2><p class="mt-1 text-sm text-muted-foreground">Choose a profile only to generate an explicit draft. The saved device never depends on the profile.</p></header>
        <div class="grid gap-5 border-y py-5 sm:grid-cols-2">
          <div class="space-y-2 sm:col-span-2"><Label>Creation profile</Label><Select v-model="profileId"><SelectTrigger><SelectValue placeholder="Select a profile" /></SelectTrigger><SelectContent><SelectItem v-for="candidate in store.catalog?.creation_profiles ?? []" :key="candidate.profile_id" :value="candidate.profile_id">{{ candidate.profile_id }}</SelectItem></SelectContent></Select><p v-if="profile" class="text-xs text-muted-foreground">{{ profile.description_key }}</p></div>
          <div class="space-y-2"><Label for="new-id">Device ID</Label><Input id="new-id" v-model="id" placeholder="idrac-lab" /></div>
          <div class="space-y-2"><Label for="new-name">Display name</Label><Input id="new-name" v-model="name" placeholder="Rack server" /></div>
          <div class="space-y-2"><Label for="new-host">Host</Label><Input id="new-host" v-model="host" placeholder="192.0.2.10" /></div>
          <div class="space-y-2"><Label for="new-port">Port</Label><Input id="new-port" v-model.number="port" type="number" min="1" max="65535" /></div>
          <div class="space-y-2"><Label for="new-user">Username</Label><Input id="new-user" v-model="username" autocomplete="username" /></div>
          <div class="space-y-2"><Label for="new-password">Password</Label><Input id="new-password" v-model="password" type="password" autocomplete="new-password" /></div>
        </div>
        <Button :disabled="loading || !profileId || !id || !name || !host" type="button" @click="preview"><Plus class="mr-2 h-4 w-4" /> Generate draft</Button>
      </section>
      <section v-else class="space-y-6">
        <header><h2 class="text-xl font-semibold tracking-tight">Review draft</h2><p class="mt-1 text-sm text-muted-foreground">{{ profile?.profile_id }} produces the following explicit feature assignments.</p></header>
        <div class="divide-y rounded-lg border bg-background">
          <div v-for="(assignment, featureId) in draft?.assignments ?? {}" :key="featureId" class="flex items-center justify-between gap-3 px-4 py-3 text-sm"><span class="font-medium">{{ featureId }}</span><Badge variant="outline">{{ assignment.kind === 'provider_set' ? `${assignment.providers.length} providers` : assignment.kind }}</Badge><ChevronRight class="h-4 w-4 text-muted-foreground" /></div>
        </div>
        <div class="flex flex-wrap items-center gap-3"><Button :disabled="loading" type="button" @click="create"><Check class="mr-2 h-4 w-4" /> Create device</Button><Button variant="outline" type="button" @click="active = 'identity'">Back</Button></div>
      </section>
    </SettingsLayout>
  </AppLayout>
</template>
