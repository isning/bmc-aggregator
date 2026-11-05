<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Eye, EyeOff, KeyRound } from 'lucide-vue-next'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import type { ConfigField, ConfigOption, ConfigValue, DiscoveredOption } from '@/configuration/catalog'

const props = defineProps<{
  field: ConfigField
  values: Record<string, ConfigValue>
  secrets: Record<string, { configured: boolean }>
  options?: DiscoveredOption[]
}>()

const emit = defineEmits<{
  'update:values': [values: Record<string, ConfigValue>]
  'update:secret': [fieldId: string, mutation: { operation: 'keep' } | { operation: 'replace'; value: string } | { operation: 'clear' }]
}>()

const { t } = useI18n()
const secretDraft = defineModel<string>({ default: '' })
const showingSecret = defineModel<boolean>('showingSecret', { default: false })

const label = computed(() => {
  const key = props.field.title_key
  const translated = t(key)
  return translated === key ? props.field.id.replace(/[_-]+/g, ' ') : translated
})

function setValue(value: ConfigValue) {
  emit('update:values', { ...props.values, [props.field.id]: value })
}

function valueOrDefault(): ConfigValue | undefined {
  return props.values[props.field.id] ?? props.field.default
}

function optionLabel(option: ConfigOption) {
  const translated = t(option.label_key)
  return translated === option.label_key ? String(option.value) : translated
}

function selectionOptions() {
  if (props.field.kind !== 'selection') return []
  if (props.field.options.source === 'static') return props.field.options.options
  return (props.options ?? []).map(option => ({ value: option.value, label_key: option.label }))
}

function selectionValue() {
  const value = valueOrDefault()
  return value == null ? '' : String(value)
}

function updateSelection(value: unknown) {
  if (typeof value !== 'string') return
  const option = selectionOptions().find(candidate => String(candidate.value) === value)
  setValue(option?.value ?? value)
}

function updateNumber(raw: unknown) {
  const value = Number(raw == null ? '' : raw)
  if (Number.isFinite(value)) setValue(value)
}

function keepSecret() {
  emit('update:secret', props.field.id, { operation: 'keep' })
}

function replaceSecret() {
  if (secretDraft.value.trim()) emit('update:secret', props.field.id, { operation: 'replace', value: secretDraft.value })
}

function clearSecret() {
  secretDraft.value = ''
  emit('update:secret', props.field.id, { operation: 'clear' })
}
</script>

<template>
  <div class="space-y-2">
    <div class="flex items-center justify-between gap-3">
      <Label :for="`field-${field.id}`" class="text-sm font-medium">{{ label }}</Label>
      <Badge v-if="field.group === 'advanced'" variant="outline" class="text-[10px] uppercase tracking-[0.12em]">Advanced</Badge>
    </div>
    <p v-if="field.description_key" class="text-xs leading-5 text-muted-foreground">{{ t(field.description_key) }}</p>

    <Input
      v-if="field.kind === 'text'"
      :id="`field-${field.id}`"
      :model-value="String(valueOrDefault() ?? '')"
      :required="field.required"
      @update:model-value="setValue($event)"
    />
    <div v-else-if="field.kind === 'secret'" class="space-y-2">
      <div class="relative">
        <KeyRound class="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-muted-foreground" />
        <Input
          :id="`field-${field.id}`"
          v-model="secretDraft"
          :type="showingSecret ? 'text' : 'password'"
          autocomplete="new-password"
          class="pl-9 pr-10"
          :placeholder="secrets[field.id]?.configured ? 'Configured; leave blank to keep' : 'Not configured'"
          @change="replaceSecret"
        />
        <button type="button" class="absolute right-2 top-2 text-muted-foreground" :aria-label="showingSecret ? 'Hide secret' : 'Show secret'" @click="showingSecret = !showingSecret">
          <EyeOff v-if="showingSecret" class="h-4 w-4" />
          <Eye v-else class="h-4 w-4" />
        </button>
      </div>
      <div class="flex gap-2 text-xs">
        <button type="button" class="text-muted-foreground underline-offset-2 hover:underline" @click="keepSecret">Keep configured value</button>
        <button v-if="secrets[field.id]?.configured" type="button" class="text-destructive underline-offset-2 hover:underline" @click="clearSecret">Clear</button>
      </div>
    </div>
    <Input
      v-else-if="field.kind === 'integer' || field.kind === 'decimal'"
      :id="`field-${field.id}`"
      type="number"
      :min="field.min"
      :max="field.max"
      :step="field.step ?? (field.kind === 'integer' ? 1 : 'any')"
      :model-value="String(valueOrDefault() ?? '')"
      @update:model-value="updateNumber"
    />
    <div v-else-if="field.kind === 'boolean'" class="flex items-center gap-3 pt-1">
      <Switch :id="`field-${field.id}`" :model-value="Boolean(valueOrDefault())" @update:model-value="setValue($event)" />
      <Label :for="`field-${field.id}`" class="text-sm text-muted-foreground">Enabled</Label>
    </div>
    <Select v-else-if="field.kind === 'selection'" :model-value="selectionValue()" @update:model-value="updateSelection">
      <SelectTrigger :id="`field-${field.id}`"><SelectValue placeholder="Select an option" /></SelectTrigger>
      <SelectContent>
        <SelectItem v-for="option in selectionOptions()" :key="String(option.value)" :value="String(option.value)">{{ optionLabel(option) }}</SelectItem>
      </SelectContent>
    </Select>
  </div>
</template>
