<script setup lang="ts">
import type { MsdMediaKind } from '@/api'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

const props = defineProps<{
  modelValue?: MsdMediaKind
  types: MsdMediaKind[]
  disabled?: boolean
}>()

const emit = defineEmits<{
  (event: 'update:modelValue', value: MsdMediaKind): void
}>()

const labels: Record<MsdMediaKind, string> = {
  optical: 'Optical drive',
  floppy: 'Floppy drive',
  disk: 'Disk drive',
  flash: 'Flash drive',
}
</script>

<template>
  <div v-if="types.length > 1" class="flex items-center gap-3 rounded-md border px-3 py-2">
    <Label class="shrink-0 text-xs text-muted-foreground">Media type</Label>
    <Select
      :model-value="props.modelValue"
      :disabled="disabled"
      @update:model-value="emit('update:modelValue', $event as MsdMediaKind)"
    >
      <SelectTrigger class="ml-auto h-8 w-[170px] text-xs">
        <SelectValue placeholder="Select media type" />
      </SelectTrigger>
      <SelectContent>
        <SelectItem v-for="type in types" :key="type" :value="type">
          {{ labels[type] }}
        </SelectItem>
      </SelectContent>
    </Select>
  </div>
</template>
