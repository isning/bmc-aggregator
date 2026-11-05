<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { Check, Save, Server, Activity } from 'lucide-vue-next'
import AppLayout from '@/components/AppLayout.vue'
import SettingsLayout from '@/components/settings/SettingsLayout.vue'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { ApiError, rootRequestMeta, type ApiResourceResponse } from '@/api/request'
import { systemApi } from '@/api'
import { useConfigurationEvents } from '@/configuration/ws'
import { RequestOrder } from '@/configuration/requestOrder'

interface WebConfig {
  http_port: number
  https_port: number
  bind_addresses: string[]
  bind_address: string
  https_enabled: boolean
  has_custom_cert: boolean
  restart_required: boolean
  frontend_dir?: string
}

type Section = 'appearance' | 'aggregator' | 'diagnostics' | 'about'
const active = ref<Section>('aggregator')
const web = ref<WebConfig | null>(null)
const version = ref<{ version: string; mode: string; arch: string; os: string } | null>(null)
const etag = ref<string | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)
const dirty = ref(false)
const theme = ref(localStorage.getItem('theme') ?? 'system')
let webRequestSerial = 0
const webOrder = new RequestOrder()
let initialLoadCompleted = false

const sections = [
  { id: 'appearance', label: 'Appearance', detail: 'Browser' },
  { id: 'aggregator', label: 'Aggregator', detail: 'Service' },
  { id: 'diagnostics', label: 'Diagnostics' },
  { id: 'about', label: 'About' },
]

function setTheme(value: string) {
  theme.value = value
  localStorage.setItem('theme', value)
  const dark = value === 'dark' || (value === 'system' && matchMedia('(prefers-color-scheme: dark)').matches)
  document.documentElement.classList.toggle('dark', dark)
}

async function refreshWeb(preserveDraft = false, duringWrite = false) {
  const requestSerial = ++webRequestSerial
  const ticket = webOrder.read()
  const response: ApiResourceResponse<WebConfig> = await rootRequestMeta('/server/web')
  if (requestSerial !== webRequestSerial || !webOrder.accepts(ticket) || (loading.value && !duringWrite)) return
  // Edits may have started while the HTTP request was in flight.
  preserveDraft ||= dirty.value
  if (!preserveDraft) web.value = response.data
  etag.value = response.etag
  if (!preserveDraft) dirty.value = false
}

async function loadWeb() {
  try {
    await refreshWeb()
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : String(reason)
  }
}

async function saveWeb() {
  if (!web.value || !etag.value || loading.value) return
  const ticket = webOrder.write()
  const submittedDraft = JSON.stringify(web.value)
  loading.value = true
  error.value = null
  try {
    const response = await rootRequestMeta<WebConfig>('/server/web', {
      method: 'PATCH',
      headers: { 'If-Match': etag.value, 'X-Mutation-Id': crypto.randomUUID() },
      body: JSON.stringify({ bind_address: web.value.bind_address, http_port: Number(web.value.http_port), frontend_dir: web.value.frontend_dir }),
    })
    if (JSON.stringify(web.value) === submittedDraft) {
      web.value = response.data
      dirty.value = false
    }
    etag.value = response.etag
  } catch (reason) {
    if (reason instanceof ApiError && reason.status === 409) {
      webOrder.finish(ticket)
      await refreshWeb(true, true).catch(() => undefined)
      dirty.value = true
      error.value = 'Service settings changed elsewhere. Your local edits are preserved; review them and save again.'
    } else {
      error.value = reason instanceof Error ? reason.message : String(reason)
    }
  } finally {
    webOrder.finish(ticket)
    loading.value = false
  }
}

async function loadAbout() {
  const info = await systemApi.info()
  version.value = { version: info.version, mode: info.mode, arch: info.arch, os: info.os }
}

onMounted(async () => {
  await Promise.all([
    loadWeb(),
    loadAbout().catch(reason => {
      error.value = reason instanceof Error ? reason.message : String(reason)
    }),
  ])
  initialLoadCompleted = true
})

useConfigurationEvents(event => {
  const serviceChanged = event.scope?.kind === 'global'
    && event.scope.section === 'server.web'
  if (event.type === 'configuration.resync_required'
    || serviceChanged
    || (event.type === 'configuration.ready' && initialLoadCompleted)) {
    if (dirty.value) {
      void refreshWeb(true).then(() => {
        error.value = 'Service settings changed elsewhere. Your local edits are preserved; review them before saving.'
      }).catch(() => undefined)
    } else {
      void loadWeb()
    }
  }
})
</script>

<template>
  <AppLayout>
    <SettingsLayout title="Settings" description="Aggregator-wide service and browser preferences." :sections="sections" :active="active" @select="active = $event as Section">
      <div v-if="error" class="mb-5 rounded-md border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive" role="alert">{{ error }}</div>

      <section v-if="active === 'appearance'" class="space-y-6">
        <header><h2 class="text-xl font-semibold tracking-tight">Appearance</h2><p class="mt-1 text-sm text-muted-foreground">Preferences stay in this browser and never change BMC configuration.</p></header>
        <div class="border-y py-5">
          <Label class="text-sm font-medium">Theme</Label>
          <div class="mt-3 flex flex-wrap gap-2">
            <Button v-for="value in ['system', 'light', 'dark']" :key="value" :variant="theme === value ? 'default' : 'outline'" size="sm" type="button" @click="setTheme(value)">{{ value.charAt(0).toUpperCase() + value.slice(1) }}</Button>
          </div>
        </div>
      </section>

      <section v-else-if="active === 'aggregator'" class="space-y-6">
        <header><h2 class="text-xl font-semibold tracking-tight">Aggregator Service</h2><p class="mt-1 text-sm text-muted-foreground">Changes are persisted before they are published to other clients.</p></header>
        <div v-if="web" class="space-y-5 border-y py-5">
          <div class="grid gap-5 sm:grid-cols-2">
            <div class="space-y-2"><Label for="bind-address">Bind address</Label><Input id="bind-address" v-model="web.bind_address" @update:model-value="dirty = true" /></div>
            <div class="space-y-2"><Label for="http-port">HTTP port</Label><Input id="http-port" v-model.number="web.http_port" type="number" min="1" max="65535" @update:model-value="dirty = true" /></div>
          </div>
          <div class="space-y-2"><Label for="frontend-dir">Frontend directory</Label><Input id="frontend-dir" v-model="web.frontend_dir" @update:model-value="dirty = true" /></div>
          <div class="flex flex-wrap items-center gap-3"><Button :disabled="!dirty || loading" type="button" @click="saveWeb"><Save class="mr-2 h-4 w-4" /> Save service settings</Button><Badge v-if="!dirty" variant="outline"><Check class="mr-1 h-3 w-3" /> Applied</Badge></div>
        </div>
      </section>

      <section v-else-if="active === 'diagnostics'" class="space-y-6">
        <header><h2 class="text-xl font-semibold tracking-tight">Diagnostics</h2><p class="mt-1 text-sm text-muted-foreground">Read-only health of the aggregator configuration and runtime inventory.</p></header>
        <div class="grid gap-3 sm:grid-cols-2">
          <div class="border-y py-4"><Activity class="h-4 w-4 text-muted-foreground" /><p class="mt-3 text-sm font-medium">Configuration coordinator</p><p class="mt-1 text-xs text-muted-foreground">Optimistic versions and revisioned events are active.</p></div>
          <div class="border-y py-4"><Server class="h-4 w-4 text-muted-foreground" /><p class="mt-3 text-sm font-medium">Backend catalog</p><p class="mt-1 text-xs text-muted-foreground">Backend fields are described by the server catalog.</p></div>
        </div>
      </section>

      <section v-else class="space-y-6">
        <header><h2 class="text-xl font-semibold tracking-tight">About</h2><p class="mt-1 text-sm text-muted-foreground">Build and platform information.</p></header>
        <div v-if="version" class="grid gap-4 border-y py-5 sm:grid-cols-2">
          <div><p class="text-xs uppercase tracking-[0.12em] text-muted-foreground">Version</p><p class="mt-1 font-medium">{{ version.version }}</p></div>
          <div><p class="text-xs uppercase tracking-[0.12em] text-muted-foreground">Mode</p><p class="mt-1 font-medium">{{ version.mode }}</p></div>
          <div><p class="text-xs uppercase tracking-[0.12em] text-muted-foreground">Platform</p><p class="mt-1 font-medium">{{ version.os }} / {{ version.arch }}</p></div>
        </div>
      </section>
    </SettingsLayout>
  </AppLayout>
</template>
