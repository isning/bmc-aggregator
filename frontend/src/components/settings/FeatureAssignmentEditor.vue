<script setup lang="ts">
import { computed } from 'vue'
import { ArrowDown, ArrowUp, Plus, Trash2 } from 'lucide-vue-next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import BackendFieldRenderer from './BackendFieldRenderer.vue'
import {
  backendFor,
  featureTitle,
  type BackendAssignmentMutation,
  type ConfigurationCatalog,
  type FeatureAssignment,
  type FeatureAssignmentMutation,
  type FeatureDescriptor,
  type DiscoveredOption,
} from '@/configuration/catalog'
import { configuredSecrets, moveProvider, nextProviderId, patchBackendAssignment } from '@/configuration/assignment'

const props = defineProps<{
  catalog: ConfigurationCatalog
  feature: FeatureDescriptor
  assignment: FeatureAssignmentMutation
  configuredAssignment?: FeatureAssignment
  optionSnapshots: Record<string, { options: DiscoveredOption[] }>
}>()

const emit = defineEmits<{ update: [assignment: FeatureAssignmentMutation] }>()

const backends = computed(() => props.catalog.backends.filter(backend => backend.feature_id === props.feature.feature_id))
const availableBackends = computed(() => backends.value.filter(backend => backend.availability.state === 'available'))
const isProviderSet = computed(() => props.feature.binding.kind === 'provider_set')
const providerLimitReached = computed(() => props.feature.binding.kind === 'provider_set'
  && props.feature.binding.max != null
  && props.assignment.kind === 'provider_set'
  && props.assignment.providers.length >= props.feature.binding.max)
const providerRemovalAllowed = computed(() => {
  if (props.feature.binding.kind !== 'provider_set' || props.assignment.kind !== 'provider_set') return false
  const minimum = props.feature.binding.min
  return props.assignment.providers.length > minimum
    || (props.assignment.providers.length === 1 && props.feature.allow_disabled)
})

function backendName(backendId: string) {
  return backendFor(props.catalog, props.feature.feature_id, backendId)?.display_name ?? backendId
}

function backendMutation(backendId: string): BackendAssignmentMutation {
  const descriptor = backendFor(props.catalog, props.feature.feature_id, backendId)
  return {
    backend_id: backendId,
    params: { ...(descriptor?.defaults ?? {}) },
    secrets: Object.fromEntries((descriptor?.fields ?? []).filter(field => field.kind === 'secret').map(field => [field.id, { operation: 'keep' as const }])),
  }
}

function existingBackend(assignment: FeatureAssignmentMutation): BackendAssignmentMutation | null {
  if (assignment.kind === 'single') return assignment.backend
  if (assignment.kind === 'provider_set') return assignment.providers[0] ?? null
  return null
}

function selectSingle(value: unknown) {
  if (typeof value !== 'string' || !value) return
  const backendId = value
  if (props.assignment.kind === 'single' && props.assignment.backend.backend_id !== backendId
    && !window.confirm('Changing backend clears fields that belong to the current backend. Continue?')) return
  emit('update', { kind: 'single', backend: backendMutation(backendId) })
}

function selectDisabled() {
  if (props.feature.allow_disabled) emit('update', { kind: 'disabled' })
}

function addProvider() {
  if (!isProviderSet.value) return
  const backendId = availableBackends.value[0]?.backend_id
  if (!backendId) return
  const providers = props.assignment.kind === 'provider_set'
    ? props.assignment.providers.map(provider => ({ id: provider.id, ...patchBackendAssignment(provider, {}) }))
    : []
  providers.push({
    id: nextProviderId(backendId, providers.map(provider => provider.id)),
    ...backendMutation(backendId),
  })
  emit('update', { kind: 'provider_set', providers })
}

function reorderProvider(index: number, offset: -1 | 1) {
  if (props.assignment.kind !== 'provider_set') return
  emit('update', {
    kind: 'provider_set',
    providers: moveProvider(props.assignment.providers, index, offset),
  })
}

function selectProviderBackend(index: number, value: unknown) {
  if (props.assignment.kind !== 'provider_set' || typeof value !== 'string' || !value) return
  const current = props.assignment.providers[index]
  if (!current || current.backend_id === value) return
  if (!window.confirm('Changing backend clears fields that belong to the current backend. Continue?')) return
  const providers = props.assignment.providers.map((provider, candidate) => candidate === index
    ? { id: provider.id, ...backendMutation(value) }
    : provider)
  emit('update', { kind: 'provider_set', providers })
}

function removeProvider(index: number) {
  if (props.assignment.kind !== 'provider_set') return
  const providers = props.assignment.providers
    .filter((_provider, candidate) => candidate !== index)
    .map(provider => ({ id: provider.id, ...patchBackendAssignment(provider, {}) }))
  if (providers.length === 0 && props.feature.allow_disabled) emit('update', { kind: 'disabled' })
  else emit('update', { kind: 'provider_set', providers })
}

function updateProvider(index: number, patch: Partial<BackendAssignmentMutation>) {
  if (props.assignment.kind !== 'provider_set') return
  const providers = props.assignment.providers.map((provider, candidate) => {
    const current = { id: provider.id, ...patchBackendAssignment(provider, {}) }
    return candidate === index
      ? { id: current.id, ...patchBackendAssignment(current, patch) }
      : current
  })
  emit('update', { kind: 'provider_set', providers })
}

function updateSingle(patch: Partial<BackendAssignmentMutation>) {
  const current = existingBackend(props.assignment)
  if (!current) return
  emit('update', {
    kind: 'single',
    backend: patchBackendAssignment(current, patch),
  })
}

function fieldsFor(backendId: string) {
  return backendFor(props.catalog, props.feature.feature_id, backendId)?.fields ?? []
}

function optionsFor(fieldId: string, backendId: string) {
  const field = fieldsFor(backendId).find(field => field.id === fieldId)
  if (!field || field.kind !== 'selection' || field.options.source !== 'registered_resource') return []
  return props.optionSnapshots[field.options.resource_id]?.options ?? []
}

const currentBackend = computed(() => existingBackend(props.assignment))

function updateSingleSecret(fieldId: string, mutation: BackendAssignmentMutation['secrets'][string]) {
  updateSingle({ secrets: { [fieldId]: mutation } })
}

function updateProviderSecret(index: number, fieldId: string, mutation: BackendAssignmentMutation['secrets'][string]) {
  updateProvider(index, { secrets: { [fieldId]: mutation } })
}
</script>

<template>
  <div class="space-y-4">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div>
        <h3 class="font-semibold tracking-tight">{{ featureTitle(feature) }}</h3>
        <p v-if="feature.description_key" class="mt-1 text-sm text-muted-foreground">{{ feature.description_key }}</p>
      </div>
      <Badge v-if="isProviderSet" variant="outline">Provider set</Badge>
    </div>

    <div v-if="!isProviderSet" class="space-y-4">
      <div class="flex items-center gap-2">
        <Select :model-value="assignment.kind === 'single' ? assignment.backend.backend_id : ''" @update:model-value="selectSingle">
          <SelectTrigger class="max-w-sm"><SelectValue placeholder="Select backend" /></SelectTrigger>
          <SelectContent>
            <SelectItem v-for="backend in backends" :key="backend.backend_id" :value="backend.backend_id" :disabled="backend.availability.state !== 'available'">
              {{ backend.display_name }}
            </SelectItem>
          </SelectContent>
        </Select>
        <Button v-if="feature.allow_disabled" variant="outline" size="sm" type="button" @click="selectDisabled">Disable</Button>
      </div>
      <div v-if="currentBackend" class="grid gap-4 rounded-lg border bg-muted/20 p-4 sm:grid-cols-2">
        <BackendFieldRenderer
          v-for="field in fieldsFor(currentBackend.backend_id)"
          :key="field.id"
          :field="field"
          :values="currentBackend.params"
          :secrets="configuredSecrets(configuredAssignment, currentBackend.backend_id)"
          :options="optionsFor(field.id, currentBackend.backend_id)"
          @update:values="updateSingle({ params: $event })"
          @update:secret="updateSingleSecret"
        />
      </div>
    </div>

    <div v-else class="space-y-3">
      <div v-if="assignment.kind !== 'provider_set' || assignment.providers.length === 0" class="rounded-lg border border-dashed p-5 text-sm text-muted-foreground">
        No providers configured yet.
      </div>
      <Card v-for="(provider, index) in assignment.kind === 'provider_set' ? assignment.providers : []" :key="provider.id" class="rounded-lg shadow-none">
        <CardHeader class="flex-row items-center justify-between gap-3 space-y-0 border-b py-3">
          <div>
            <CardTitle class="text-sm">{{ provider.id }}</CardTitle>
            <CardDescription>{{ backendName(provider.backend_id) }}</CardDescription>
          </div>
          <div class="flex items-center gap-1">
            <Button variant="ghost" size="icon" type="button" :disabled="index === 0" :aria-label="`Move ${provider.id} up`" @click="reorderProvider(index, -1)"><ArrowUp class="h-4 w-4" /></Button>
            <Button variant="ghost" size="icon" type="button" :disabled="assignment.kind !== 'provider_set' || index === assignment.providers.length - 1" :aria-label="`Move ${provider.id} down`" @click="reorderProvider(index, 1)"><ArrowDown class="h-4 w-4" /></Button>
            <Button variant="ghost" size="icon" type="button" :disabled="!providerRemovalAllowed" :aria-label="`Remove ${provider.id}`" @click="removeProvider(index)"><Trash2 class="h-4 w-4" /></Button>
          </div>
        </CardHeader>
        <CardContent class="grid gap-4 p-4 sm:grid-cols-2">
          <Select :model-value="provider.backend_id" @update:model-value="value => selectProviderBackend(index, value)">
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent><SelectItem v-for="backend in backends" :key="backend.backend_id" :value="backend.backend_id" :disabled="backend.availability.state !== 'available'">{{ backend.display_name }}</SelectItem></SelectContent>
          </Select>
          <BackendFieldRenderer
            v-for="field in fieldsFor(provider.backend_id)"
            :key="field.id"
            :field="field"
            :values="provider.params"
            :secrets="configuredSecrets(configuredAssignment, provider.backend_id, provider.id)"
            :options="optionsFor(field.id, provider.backend_id)"
            @update:values="updateProvider(index, { params: $event })"
            @update:secret="(fieldId, mutation) => updateProviderSecret(index, fieldId, mutation)"
          />
        </CardContent>
      </Card>
      <Button variant="outline" type="button" :disabled="providerLimitReached || availableBackends.length === 0" @click="addProvider"><Plus class="mr-2 h-4 w-4" /> Add provider</Button>
    </div>
  </div>
</template>
