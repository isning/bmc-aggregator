<script setup lang="ts">
import { defineAsyncComponent, ref, computed, onMounted, onUnmounted, watch, nextTick } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { useSystemStore } from '@/stores/system'
import { Button } from '@/components/ui/button'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import {
  ClipboardPaste,
  HardDrive,
  Keyboard,
  Settings,
  Maximize,
  Power,
  BarChart3,
  MoreHorizontal,
  ListRestart,
  LocateFixed,
  ShieldCheck,
} from 'lucide-vue-next'
import type { VideoMode } from '@/features/video/VideoConfigPopover.vue'
import { systemApi } from '@/api/config'
import { useFeatureDataSubscription } from '@/feature-data/resource'
import { identifyStatusResource } from '@/feature-data/resources'

const PasteModal = defineAsyncComponent(() => import('@/components/PasteModal.vue'))
const PowerPopover = defineAsyncComponent(() => import('@/components/PowerPopover.vue'))
const BootPopover = defineAsyncComponent(() => import('@/components/BootPopover.vue'))
const IdentifyPopover = defineAsyncComponent(() => import('@/components/IdentifyPopover.vue'))
const VideoConfigPopover = defineAsyncComponent(() => import('@/features/video/VideoConfigPopover.vue'))
const HidConfigPopover = defineAsyncComponent(() => import('@/features/hid/HidConfigPopover.vue'))
const AudioConfigPopover = defineAsyncComponent(() => import('@/features/audio/AudioConfigPopover.vue'))
const MsdDialog = defineAsyncComponent(() => import('@/features/msd/MsdDialog.vue'))
const RequirementsSheet = defineAsyncComponent(() => import('@/components/RequirementsSheet.vue'))

const { t, locale } = useI18n()
const router = useRouter()
const route = useRoute()
const systemStore = useSystemStore()

const overflowMenuOpen = ref(false)
const showMsd = computed(() => systemStore.isFeatureAvailable('msd'))
const showBoot = computed(() => systemStore.isFeatureAvailable('boot'))
const showIdentify = computed(() => systemStore.isFeatureAvailable('identify'))
useFeatureDataSubscription(identifyStatusResource, showIdentify)
const identifyState = computed(() => identifyStatusResource.data.value?.identify.mode ?? 'off')
const identifyStateColor = computed(() => {
  if (!showIdentify.value) return 'bg-slate-400'
  if (identifyState.value === 'indefinite') return 'bg-blue-500'
  if (identifyState.value === 'temporary') return 'bg-blue-500'
  return 'bg-slate-400'
})

const props = defineProps<{
  mouseMode?: 'absolute' | 'relative'
  videoMode?: VideoMode
}>()

const emit = defineEmits<{
  (e: 'toggleFullscreen'): void
  (e: 'toggleStats'): void
  (e: 'toggleVirtualKeyboard'): void
  (e: 'toggleMouseMode'): void
  (e: 'update:videoMode', mode: VideoMode): void
  (e: 'updateVideoTransport', transport: 'webrtc' | 'webtransport'): void
  (e: 'powerShort'): void
  (e: 'powerLong'): void
  (e: 'reset'): void
  (e: 'wol', macAddress: string): void
}>()

const pasteOpen = ref(false)
const powerOpen = ref(false)
const bootOpen = ref(false)
const identifyOpen = ref(false)
const videoPopoverOpen = ref(false)
const hidPopoverOpen = ref(false)
const audioPopoverOpen = ref(false)
const msdDialogOpen = ref(false)
const requirementsOpen = ref(false)
const requirementsActionCount = ref(0)

const mobilePowerOpen = ref(false)
const mobileBootOpen = ref(false)
const mobileIdentifyOpen = ref(false)
const mobilePasteOpen = ref(false)
const mobilePowerOpenTime = ref(0)
const mobileBootOpenTime = ref(0)
const mobileIdentifyOpenTime = ref(0)
const mobilePasteOpenTime = ref(0)

const OPEN_GUARD_MS = 350

async function refreshRequirementSummary() {
  try {
    const result = await systemApi.requirements()
    requirementsActionCount.value = result.summary.action_required + result.summary.pending
  } catch {
    requirementsActionCount.value = 0
  }
}

const guardOutside = (openTime: number, e: Event) => {
  if (Date.now() - openTime < OPEN_GUARD_MS) {
    e.preventDefault()
  }
}

const openFromOverflow = (setter: () => void) => {
  overflowMenuOpen.value = false
  setTimeout(setter, 50)
}

const openMobilePower = () => openFromOverflow(() => {
  mobilePowerOpen.value = true
  mobilePowerOpenTime.value = Date.now()
})

const openMobileBoot = () => openFromOverflow(() => {
  mobileBootOpen.value = true
  mobileBootOpenTime.value = Date.now()
})

const openMobileIdentify = () => openFromOverflow(() => {
  mobileIdentifyOpen.value = true
  mobileIdentifyOpenTime.value = Date.now()
})

const openMobilePaste = () => openFromOverflow(() => {
  mobilePasteOpen.value = true
  mobilePasteOpenTime.value = Date.now()
})

function openSettings() {
  router.push({ name: 'DeviceSettings', params: { bmcId: route.params.bmcId } })
}


const barRef = ref<HTMLElement | null>(null)
const measureRef = ref<HTMLElement | null>(null)
const barWidth = ref(0)
let resizeObserver: ResizeObserver | null = null

type CollapsibleItem =
  | 'video' | 'audio' | 'hid'
  | 'msd' | 'power' | 'boot' | 'identify' | 'paste'
  | 'stats' | 'settings'

interface ItemSpec {
  id: CollapsibleItem
  side: 'left' | 'right'
}

const ITEM_SPECS: ItemSpec[] = [
  { id: 'video',     side: 'left' },
  { id: 'audio',     side: 'left' },
  { id: 'hid',       side: 'left' },
  { id: 'msd',       side: 'left' },
  { id: 'power',       side: 'left' },
  { id: 'boot',        side: 'left' },
  { id: 'identify',    side: 'left' },
  { id: 'paste',     side: 'left' },
  { id: 'stats',     side: 'right' },
  { id: 'settings',  side: 'right' },
]

const measuredWidths = ref<Map<CollapsibleItem, { icon: number; label: number }>>(new Map())
const measurementReady = ref(false)

const measureButtonWidths = async () => {
  await nextTick()
  if (!measureRef.value) return

  const newWidths = new Map<CollapsibleItem, { icon: number; label: number }>()

  for (const spec of ITEM_SPECS) {
    const iconEl = measureRef.value.querySelector(`[data-measure="${spec.id}-icon"]`) as HTMLElement
    const labelEl = measureRef.value.querySelector(`[data-measure="${spec.id}-label"]`) as HTMLElement

    if (iconEl && labelEl) {
      newWidths.set(spec.id, {
        icon: Math.ceil(iconEl.offsetWidth) + 8,
        label: Math.ceil(labelEl.offsetWidth) + 8,
      })
    }
  }

  measuredWidths.value = newWidths
  measurementReady.value = true
}

onMounted(() => {
  if (barRef.value) {
    resizeObserver = new ResizeObserver((entries) => {
      const entry = entries[0]
      if (entry) barWidth.value = entry.contentRect.width
    })
    resizeObserver.observe(barRef.value)
    barWidth.value = barRef.value.clientWidth
  }

  measureButtonWidths()
  refreshRequirementSummary().catch(() => {})
})

onUnmounted(() => {
  resizeObserver?.disconnect()
})

watch(locale, () => {
  measurementReady.value = false
  measureButtonWidths()
})

const RIGHT_FIXED_PX = 120

const collapsibleItems = computed(() => {
  const items = ITEM_SPECS.slice(3).filter(item => {
    if (item.id === 'msd' && !showMsd.value) return false
    if (item.id === 'boot' && !showBoot.value) return false
    if (item.id === 'identify' && !showIdentify.value) return false
    return true
  })
  return items
})

const visibleSet = computed(() => {
  if (!measurementReady.value) {
    return new Map<CollapsibleItem, 'icon' | 'label'>()
  }

  const available = barWidth.value - RIGHT_FIXED_PX

  let used = 0
  if (barRef.value) {
    const leftContainer = barRef.value.querySelector('.left-buttons') as HTMLElement
    if (leftContainer) {
      const children = Array.from(leftContainer.children).slice(0, 3) as HTMLElement[]
      used = children.reduce((sum, el) => sum + el.offsetWidth, 0)
    }
  }

  if (used === 0) used = 330

  const result = new Map<CollapsibleItem, 'icon' | 'label'>()

  for (const item of collapsibleItems.value) {
    const widths = measuredWidths.value.get(item.id)
    if (!widths) continue

    if (used + widths.icon <= available) {
      if (used + widths.label <= available) {
        result.set(item.id, 'label')
        used += widths.label
      } else {
        result.set(item.id, 'icon')
        used += widths.icon
      }
    }
  }

  return result
})

const isVisible = (id: CollapsibleItem) => visibleSet.value.has(id)
const hasOverflow = computed(() => {
  return collapsibleItems.value.some(i => !visibleSet.value.has(i.id))
})
</script>

<template>
  <div class="w-full border-b border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900">
    <div ref="barRef" class="flex items-center px-2 sm:px-4 py-1 sm:py-1.5">
      <!-- Left side buttons -->
      <div class="left-buttons flex items-center gap-0.5 sm:gap-1.5 flex-1 min-w-0 overflow-hidden">
        <!-- Video Config - Always visible -->
        <VideoConfigPopover
          v-model:open="videoPopoverOpen"
          :video-mode="props.videoMode || 'mjpeg'"
          @update:video-mode="emit('update:videoMode', $event)"
          @update:transport="emit('updateVideoTransport', $event)"
        />

        <!-- Audio Config - Always visible -->
        <AudioConfigPopover v-model:open="audioPopoverOpen" />

        <!-- HID Config - Always visible -->
        <HidConfigPopover
          v-model:open="hidPopoverOpen"
          :mouse-mode="mouseMode"
          @update:mouse-mode="emit('toggleMouseMode')"
        />

        <!-- Virtual Media (MSD) - Adaptive -->
        <div v-if="showMsd && isVisible('msd')">
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger as-child>
                <Button variant="ghost" size="sm" class="h-8 gap-1.5 text-xs" @click="msdDialogOpen = true">
                  <HardDrive class="h-4 w-4" />
                  <span v-if="visibleSet.get('msd') === 'label'">{{ t('actionbar.virtualMedia') }}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                <p>{{ t('actionbar.virtualMediaTip') }}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>

        <!-- ATX Power Control - Adaptive -->
        <div v-if="isVisible('power')">
          <Popover v-model:open="powerOpen">
            <PopoverTrigger as-child>
              <Button variant="ghost" size="sm" class="h-8 gap-1.5 text-xs" :aria-label="t('actionbar.power')">
                <Power class="h-4 w-4" />
                <span v-if="visibleSet.get('power') === 'label'">{{ t('actionbar.power') }}</span>
              </Button>
            </PopoverTrigger>
            <PopoverContent class="w-[min(280px,90vw)] p-0" align="start">
              <PowerPopover
                :active="powerOpen"
                @close="powerOpen = false"
                @power-short="emit('powerShort')"
                @power-long="emit('powerLong')"
                @reset="emit('reset')"
                @wol="(mac) => emit('wol', mac)"
              />
            </PopoverContent>
          </Popover>
        </div>

        <div v-if="showBoot && isVisible('boot')">
          <Popover v-model:open="bootOpen">
            <PopoverTrigger as-child>
              <Button variant="ghost" size="sm" class="h-8 gap-1.5 text-xs" :aria-label="t('actionbar.boot')" :title="t('actionbar.bootTip')">
                      <ListRestart class="h-4 w-4" />
                <span v-if="visibleSet.get('boot') === 'label'">{{ t('actionbar.boot') }}</span>
              </Button>
            </PopoverTrigger>
            <PopoverContent class="w-[min(280px,90vw)] p-0" align="start">
              <BootPopover :active="bootOpen" />
            </PopoverContent>
          </Popover>
        </div>

        <div v-if="showIdentify && isVisible('identify')">
          <Popover v-model:open="identifyOpen">
            <PopoverTrigger as-child>
              <Button variant="ghost" size="sm" class="h-8 gap-1.5 text-xs" :aria-label="t('actionbar.identify')" :title="t('actionbar.identifyTip')">
                <LocateFixed class="h-4 w-4" />
                <span v-if="visibleSet.get('identify') === 'label'">{{ t('actionbar.identify') }}</span>
                <span :class="['uid-status-dot h-2 w-2 rounded-full', identifyStateColor, identifyState !== 'off' && 'uid-status-dot--active']" aria-hidden="true" />
              </Button>
            </PopoverTrigger>
            <PopoverContent class="w-[min(280px,90vw)] p-0" align="start">
              <IdentifyPopover :active="identifyOpen" />
            </PopoverContent>
          </Popover>
        </div>

        <!-- Paste Text - Adaptive -->
        <div v-if="isVisible('paste')">
          <Popover v-model:open="pasteOpen">
            <PopoverTrigger as-child>
              <Button variant="ghost" size="sm" class="h-8 gap-1.5 text-xs">
                <ClipboardPaste class="h-4 w-4" />
                <span v-if="visibleSet.get('paste') === 'label'">{{ t('actionbar.paste') }}</span>
              </Button>
            </PopoverTrigger>
            <PopoverContent class="w-[min(400px,90vw)] p-0" align="start">
              <PasteModal @close="pasteOpen = false" />
            </PopoverContent>
          </Popover>
        </div>
      </div>

      <!-- Right side buttons -->
      <div class="flex items-center gap-0.5 sm:gap-1.5 shrink-0 ml-1 sm:ml-2">
        <TooltipProvider>
          <Tooltip>
            <TooltipTrigger as-child>
              <Button
                variant="ghost"
                size="sm"
                class="relative h-7 w-7 p-0 sm:h-8 sm:w-8"
                :aria-label="t('actionbar.requirements')"
                @click="requirementsOpen = true"
              >
                <ShieldCheck class="h-3.5 w-3.5 sm:h-4 sm:w-4" />
                <span
                  v-if="requirementsActionCount > 0"
                  class="absolute -right-0.5 -top-0.5 flex h-3.5 min-w-3.5 items-center justify-center bg-amber-500 px-1 text-[9px] font-semibold leading-none text-black"
                >{{ Math.min(requirementsActionCount, 9) }}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent><p>{{ t('actionbar.requirementsTip') }}</p></TooltipContent>
          </Tooltip>
        </TooltipProvider>

        <!-- Connection Stats - Adaptive -->
        <div v-if="isVisible('stats')">
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger as-child>
                <Button variant="ghost" size="sm" class="h-8 gap-1.5 text-xs" @click="emit('toggleStats')">
                  <BarChart3 class="h-4 w-4" />
                  <span v-if="visibleSet.get('stats') === 'label'">{{ t('actionbar.stats') }}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                <p>{{ t('actionbar.statsTip') }}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>

        <!-- Settings - Adaptive -->
        <div v-if="isVisible('settings')">
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger as-child>
                <Button variant="ghost" size="sm" class="h-8 gap-1.5 text-xs" @click="openSettings">
                  <Settings class="h-4 w-4" />
                  <span v-if="visibleSet.get('settings') === 'label'">{{ t('actionbar.settings') }}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                <p>{{ t('actionbar.settingsTip') }}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>

        <div v-if="isVisible('stats') || isVisible('settings')" class="h-5 w-px bg-slate-200 dark:bg-slate-700" />

        <!-- Virtual Keyboard - Always visible -->
        <TooltipProvider>
          <Tooltip>
            <TooltipTrigger as-child>
              <Button
                variant="ghost"
                size="sm"
                class="h-7 w-7 sm:h-8 sm:w-auto p-0 sm:px-2 sm:gap-1.5 text-xs"
                @click="emit('toggleVirtualKeyboard')"
              >
                <Keyboard class="h-3.5 w-3.5 sm:h-4 sm:w-4" />
                <span class="hidden xl:inline">{{ t('actionbar.keyboard') }}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>
              <p>{{ t('actionbar.keyboardTip') }}</p>
            </TooltipContent>
          </Tooltip>
        </TooltipProvider>

        <!-- Fullscreen - Always visible -->
        <TooltipProvider>
          <Tooltip>
            <TooltipTrigger as-child>
              <Button
                variant="ghost"
                size="sm"
                class="h-7 w-7 sm:h-8 sm:w-auto p-0 sm:px-2 sm:gap-1.5 text-xs"
                @click="emit('toggleFullscreen')"
              >
                <Maximize class="h-3.5 w-3.5 sm:h-4 sm:w-4" />
                <span class="hidden xl:inline">{{ t('actionbar.fullscreen') }}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>
              <p>{{ t('actionbar.fullscreenTip') }}</p>
            </TooltipContent>
          </Tooltip>
        </TooltipProvider>

        <!-- Overflow Menu - Only show if there are overflowed items -->
        <DropdownMenu v-if="hasOverflow" v-model:open="overflowMenuOpen">
          <DropdownMenuTrigger as-child>
            <Button variant="ghost" size="sm" class="h-7 w-7 sm:h-8 sm:w-8 p-0">
              <MoreHorizontal class="h-3.5 w-3.5 sm:h-4 sm:w-4" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" class="w-48">
            <!-- MSD -->
            <DropdownMenuItem v-if="showMsd && !isVisible('msd')" @click="openFromOverflow(() => msdDialogOpen = true)">
              <HardDrive class="h-4 w-4 mr-2" />
              {{ t('actionbar.virtualMedia') }}
            </DropdownMenuItem>

            <!-- ATX -->
            <DropdownMenuItem v-if="!isVisible('power')" @click="openMobilePower">
              <Power class="h-4 w-4 mr-2" />
              {{ t('actionbar.power') }}
            </DropdownMenuItem>

            <DropdownMenuItem v-if="showBoot && !isVisible('boot')" @click="openMobileBoot">
              <ListRestart class="h-4 w-4 mr-2" />
              {{ t('actionbar.boot') }}
            </DropdownMenuItem>

            <DropdownMenuItem v-if="showIdentify && !isVisible('identify')" @click="openMobileIdentify">
              <LocateFixed class="h-4 w-4 mr-2" />
              {{ t('actionbar.identify') }}
            </DropdownMenuItem>

            <!-- Paste -->
            <DropdownMenuItem v-if="!isVisible('paste')" @click="openMobilePaste">
              <ClipboardPaste class="h-4 w-4 mr-2" />
              {{ t('actionbar.paste') }}
            </DropdownMenuItem>

            <DropdownMenuSeparator v-if="(!isVisible('msd') || !isVisible('power') || (showBoot && !isVisible('boot')) || (showIdentify && !isVisible('identify')) || !isVisible('paste')) && (!isVisible('stats') || !isVisible('settings'))" />

            <!-- Stats -->
            <DropdownMenuItem v-if="!isVisible('stats')" @click="openFromOverflow(() => emit('toggleStats'))">
              <BarChart3 class="h-4 w-4 mr-2" />
              {{ t('actionbar.stats') }}
            </DropdownMenuItem>

            <!-- Settings -->
            <DropdownMenuItem v-if="!isVisible('settings')" @click="openFromOverflow(openSettings)">
              <Settings class="h-4 w-4 mr-2" />
              {{ t('actionbar.settings') }}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </div>
  </div>

  <!-- MSD Dialog -->
  <MsdDialog v-if="showMsd" v-model:open="msdDialogOpen" />

  <Sheet v-model:open="requirementsOpen" @update:open="open => { if (!open) refreshRequirementSummary() }">
    <SheetContent side="right" class="flex h-full w-[min(440px,94vw)] flex-col p-0 sm:max-w-[440px]">
      <SheetHeader class="sr-only">
        <SheetTitle>{{ t('requirements.title') }}</SheetTitle>
      </SheetHeader>
      <RequirementsSheet v-if="requirementsOpen" />
    </SheetContent>
  </Sheet>

  <!-- Mobile ATX Sheet — used when ATX is opened from the overflow menu.
       A Sheet avoids the Popover anchor-positioning issues on mobile. -->
  <Sheet v-model:open="mobilePowerOpen">
    <SheetContent
      side="bottom"
      class="max-h-[90dvh] overflow-y-auto"
      @pointer-down-outside="(e) => guardOutside(mobilePowerOpenTime, e)"
      @interact-outside="(e) => guardOutside(mobilePowerOpenTime, e)"
    >
      <SheetHeader class="mb-2">
        <SheetTitle>{{ t('actionbar.power') }}</SheetTitle>
      </SheetHeader>
      <PowerPopover
        :active="mobilePowerOpen"
        @close="mobilePowerOpen = false"
        @power-short="emit('powerShort')"
        @power-long="emit('powerLong')"
        @reset="emit('reset')"
        @wol="(mac) => emit('wol', mac)"
      />
    </SheetContent>
  </Sheet>

  <Sheet v-model:open="mobileBootOpen">
    <SheetContent
      side="bottom"
      class="max-h-[90dvh] overflow-y-auto"
      @pointer-down-outside="(e) => guardOutside(mobileBootOpenTime, e)"
      @interact-outside="(e) => guardOutside(mobileBootOpenTime, e)"
    >
      <SheetHeader class="mb-2"><SheetTitle>{{ t('actionbar.boot') }}</SheetTitle></SheetHeader>
      <BootPopover :active="mobileBootOpen" />
    </SheetContent>
  </Sheet>

  <Sheet v-model:open="mobileIdentifyOpen">
    <SheetContent
      side="bottom"
      class="max-h-[90dvh] overflow-y-auto"
      @pointer-down-outside="(e) => guardOutside(mobileIdentifyOpenTime, e)"
      @interact-outside="(e) => guardOutside(mobileIdentifyOpenTime, e)"
    >
      <SheetHeader class="mb-2"><SheetTitle>{{ t('actionbar.identify') }}</SheetTitle></SheetHeader>
      <IdentifyPopover :active="mobileIdentifyOpen" />
    </SheetContent>
  </Sheet>

  <!-- Mobile Paste Sheet — used when Paste is opened from the overflow menu. -->
  <Sheet v-model:open="mobilePasteOpen">
    <SheetContent
      side="bottom"
      class="max-h-[90dvh] overflow-y-auto"
      @pointer-down-outside="(e) => guardOutside(mobilePasteOpenTime, e)"
      @interact-outside="(e) => guardOutside(mobilePasteOpenTime, e)"
    >
      <SheetHeader class="mb-2">
        <SheetTitle>{{ t('actionbar.paste') }}</SheetTitle>
      </SheetHeader>
      <PasteModal @close="mobilePasteOpen = false" />
    </SheetContent>
  </Sheet>

  <!-- Hidden measurement container: renders each collapsible button in both
       icon-only and with-label forms so we can read their real offsetWidth. -->
  <div ref="measureRef" aria-hidden="true" class="fixed pointer-events-none" style="visibility: hidden; top: -9999px; left: -9999px; white-space: nowrap;">
    <div class="flex items-center gap-0.5 sm:gap-1.5 px-2 sm:px-4 py-1 sm:py-1.5">
      <!-- MSD -->
      <Button data-measure="msd-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><HardDrive class="h-4 w-4" /></Button>
      <Button data-measure="msd-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><HardDrive class="h-4 w-4" />{{ t('actionbar.virtualMedia') }}</Button>
      <!-- ATX -->
      <Button data-measure="power-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Power class="h-4 w-4" /></Button>
      <Button data-measure="power-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Power class="h-4 w-4" />{{ t('actionbar.power') }}</Button>
      <!-- Boot -->
      <Button data-measure="boot-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><ListRestart class="h-4 w-4" /></Button>
      <Button data-measure="boot-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><ListRestart class="h-4 w-4" />{{ t('actionbar.boot') }}</Button>
      <!-- Identify -->
      <Button data-measure="identify-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><LocateFixed class="h-4 w-4" /></Button>
      <Button data-measure="identify-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><LocateFixed class="h-4 w-4" />{{ t('actionbar.identify') }}</Button>
      <!-- Paste -->
      <Button data-measure="paste-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><ClipboardPaste class="h-4 w-4" /></Button>
      <Button data-measure="paste-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><ClipboardPaste class="h-4 w-4" />{{ t('actionbar.paste') }}</Button>
      <!-- Stats -->
      <Button data-measure="stats-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><BarChart3 class="h-4 w-4" /></Button>
      <Button data-measure="stats-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><BarChart3 class="h-4 w-4" />{{ t('actionbar.stats') }}</Button>
      <!-- Settings -->
      <Button data-measure="settings-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Settings class="h-4 w-4" /></Button>
      <Button data-measure="settings-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Settings class="h-4 w-4" />{{ t('actionbar.settings') }}</Button>
      <!-- Always-visible items (for measuring their actual width) -->
      <Button data-measure="video-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><HardDrive class="h-4 w-4" /></Button>
      <Button data-measure="video-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><HardDrive class="h-4 w-4" /></Button>
      <Button data-measure="audio-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><HardDrive class="h-4 w-4" /></Button>
      <Button data-measure="audio-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><HardDrive class="h-4 w-4" /></Button>
      <Button data-measure="hid-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><HardDrive class="h-4 w-4" /></Button>
      <Button data-measure="hid-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><HardDrive class="h-4 w-4" /></Button>
    </div>
  </div>
</template>

<style scoped>
.uid-status-dot--active {
  animation: uid-status-pulse 1.8s ease-in-out infinite;
}

@keyframes uid-status-pulse {
  0%, 8%, 100% {
    background-color: #94a3b8;
  }
  28%, 72% {
    background-color: #3b82f6;
  }
}

/* Use an actual PQ HDR color only when both the browser and output device
   advertise a high dynamic range. SDR displays keep the color above. */
@media (dynamic-range: high) {
  .uid-status-dot--active {
    dynamic-range-limit: no-limit;
  }

  @keyframes uid-status-pulse {
    0%, 8%, 100% {
      background-color: #94a3b8;
    }
    28%, 72% {
      background-color: color(rec2100-pq 0.02 0.18 0.8);
    }
  }
}
</style>
