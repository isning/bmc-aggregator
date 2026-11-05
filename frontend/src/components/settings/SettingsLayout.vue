<script setup lang="ts">
import { Menu } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle, SheetTrigger } from '@/components/ui/sheet'

defineProps<{
  title: string
  description?: string
  sections: Array<{ id: string; label: string; detail?: string }>
  active: string
}>()

const emit = defineEmits<{ select: [id: string] }>()
</script>

<template>
  <div class="h-full overflow-auto bg-muted/20">
    <div class="mx-auto grid min-h-full max-w-7xl gap-6 p-4 sm:p-6 lg:grid-cols-[220px_minmax(0,1fr)] lg:gap-10 lg:p-8">
      <aside class="hidden lg:block">
        <div class="sticky top-8 space-y-6">
          <div>
            <p class="text-[11px] font-semibold uppercase tracking-[0.18em] text-muted-foreground">Configuration</p>
            <h1 class="mt-2 text-xl font-semibold tracking-tight">{{ title }}</h1>
            <p v-if="description" class="mt-2 text-sm leading-5 text-muted-foreground">{{ description }}</p>
          </div>
          <nav class="space-y-1" aria-label="Settings sections">
            <button
              v-for="section in sections"
              :key="section.id"
              type="button"
              class="flex w-full items-center justify-between rounded-md px-3 py-2 text-left text-sm transition-colors"
              :class="active === section.id ? 'bg-primary text-primary-foreground shadow-sm' : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'"
              @click="emit('select', section.id)"
            >
              <span>{{ section.label }}</span>
              <span v-if="section.detail" class="text-xs opacity-70">{{ section.detail }}</span>
            </button>
          </nav>
        </div>
      </aside>

      <div class="min-w-0">
        <div class="mb-5 flex items-start gap-3 lg:hidden">
          <Sheet>
            <SheetTrigger as-child><Button variant="outline" size="icon" aria-label="Open settings sections"><Menu class="h-4 w-4" /></Button></SheetTrigger>
            <SheetContent side="left" class="w-[280px]">
              <SheetHeader><SheetTitle>{{ title }}</SheetTitle><SheetDescription>{{ description }}</SheetDescription></SheetHeader>
              <nav class="mt-6 space-y-1">
                <button v-for="section in sections" :key="section.id" type="button" class="flex w-full rounded-md px-3 py-2 text-left text-sm" :class="active === section.id ? 'bg-primary text-primary-foreground' : 'hover:bg-accent'" @click="emit('select', section.id)">{{ section.label }}</button>
              </nav>
            </SheetContent>
          </Sheet>
          <div><h1 class="text-lg font-semibold">{{ title }}</h1><p class="text-sm text-muted-foreground">{{ sections.find(section => section.id === active)?.label }}</p></div>
        </div>
        <slot />
      </div>
    </div>
  </div>
</template>
