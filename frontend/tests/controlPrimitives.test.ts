import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const bootPopover = readFileSync(
  new URL('../src/components/BootPopover.vue', import.meta.url),
  'utf8',
)
const virtualKeyboard = readFileSync(
  new URL('../src/components/VirtualKeyboard.vue', import.meta.url),
  'utf8',
)
const videoWindow = readFileSync(
  new URL('../src/features/video/VideoConsoleWindow.vue', import.meta.url),
  'utf8',
)
const draggableWindowFrame = readFileSync(
  new URL('../src/components/DraggableWindowFrame.vue', import.meta.url),
  'utf8',
)
const hidConfigPopover = readFileSync(
  new URL('../src/features/hid/HidConfigPopover.vue', import.meta.url),
  'utf8',
)
const videoConfigPopover = readFileSync(
  new URL('../src/features/video/VideoConfigPopover.vue', import.meta.url),
  'utf8',
)

test('boot mode selection uses the shared controlled tabs primitive', () => {
  assert.match(bootPopover, /import \{ Tabs, TabsList, TabsTrigger \} from '@\/components\/ui\/tabs'/)
  assert.match(bootPopover, /<Tabs[\s\S]*?:model-value="mode"/)
  assert.match(bootPopover, /<TabsTrigger[\s\S]*?:value="item"/)
  assert.doesNotMatch(bootPopover, /:variant="mode === item \? 'secondary' : 'ghost'"/)
})

test('boot uses one provider-driven selection label', () => {
  const english = readFileSync(new URL('../src/i18n/en-US.ts', import.meta.url), 'utf8')
  const chinese = readFileSync(new URL('../src/i18n/zh-CN.ts', import.meta.url), 'utf8')
  assert.match(english, /bootSelection: 'Boot selection'/)
  assert.match(chinese, /bootSelection: '启动选择'/)
  assert.doesNotMatch(english, /bootDeviceTypes|Boot device types/)
  assert.doesNotMatch(chinese, /bootDeviceTypes|启动设备类型|设备类别/)
})

test('boot duration choices come directly from the selected entry', () => {
  assert.match(bootPopover, /<Label class="text-xs">\{\{ t\('atx\.bootDuration'\) \}\}<\/Label>/)
  assert.match(bootPopover, /v-for="item in selectedModes"/)
  assert.doesNotMatch(bootPopover, /usingEntry|bootEntryOnceOnly/)
})

test('boot entries use one selection control without entry-family translation', () => {
  assert.match(bootPopover, /atx\.bootSelection/)
  assert.match(bootPopover, /:value="entry\.id"/)
  assert.match(bootPopover, /SelectGroup/)
  assert.match(bootPopover, /SelectLabel/)
  assert.match(bootPopover, /entry\.badges/)
  assert.match(bootPopover, /selectedEntry\?\.badges/)
  assert.match(bootPopover, /<Badge/)
  assert.doesNotMatch(bootPopover, /class="ml-auto flex shrink-0 gap-1"/)
  assert.doesNotMatch(bootPopover, /category|firmware|targetChoice|\befi\b/i)
  assert.doesNotMatch(bootPopover, /id="boot-target"/)
})

test('remote video performance uses the shared controlled tabs primitive', () => {
  assert.match(videoConfigPopover, /import \{ Tabs, TabsList, TabsTrigger \} from '@\/components\/ui\/tabs'/)
  assert.match(videoConfigPopover, /<Tabs\s+:model-value="colorPerformancePreference"/)
  assert.match(videoConfigPopover, /aria-label="Remote video performance"/)
  assert.match(videoConfigPopover, /<TabsTrigger\s+value="performance"/)
  assert.match(videoConfigPopover, /<TabsTrigger\s+value="video"/)
  assert.match(videoConfigPopover, /data-\[state=active\]:border-border/)
})

test('virtual keyboard chrome uses shared controls and the video pointer-drag lifecycle', () => {
  assert.match(virtualKeyboard, /<DraggableWindowFrame/)
  assert.match(videoWindow, /<DraggableWindowFrame/)
  assert.match(draggableWindowFrame, /usePointerWindowMove/)
  assert.match(draggableWindowFrame, /moveBoundedWindow/)
  assert.match(virtualKeyboard, /<ToggleGroup/)
  assert.match(virtualKeyboard, /<Button/)
  assert.doesNotMatch(virtualKeyboard, /useDraggable/)
  assert.doesNotMatch(virtualKeyboard, /@pointer(move|down|up|cancel)="(begin|update|end)Move"/)
  assert.doesNotMatch(videoWindow, /@pointer(move|down|up|cancel)="(begin|update|end)Move"/)
  assert.doesNotMatch(virtualKeyboard, /document\.addEventListener\('(mouse|touch)/)
  assert.doesNotMatch(virtualKeyboard, /class="vkb-(btn|os-btn|media-btn)/)
})

test('floating windows share one title-bar layout and height contract', () => {
  assert.match(draggableWindowFrame, /windowTitle: string/)
  assert.match(draggableWindowFrame, /\{\{ windowTitle \}\}/)
  assert.match(draggableWindowFrame, /<slot name="icon" \/>/)
  assert.match(draggableWindowFrame, /<slot name="actions" \/>/)
  assert.match(videoWindow, /:window-title=/)
  assert.match(videoWindow, /<template #icon>/)
  assert.match(videoWindow, /<template #actions>/)
  assert.match(virtualKeyboard, /:window-title=/)
  assert.match(virtualKeyboard, /<template #icon>/)
  assert.match(virtualKeyboard, /<template #actions>/)
  assert.doesNotMatch(videoWindow, /<template #header>/)
  assert.doesNotMatch(virtualKeyboard, /<template #header>/)
  assert.doesNotMatch(videoWindow, /video-window__(identity|title|telemetry|controls)/)
})

test('window frames allow bounded title-bar heights and keyboard chrome uses icon actions', () => {
  assert.match(draggableWindowFrame, /headerHeight\?: number/)
  assert.match(draggableWindowFrame, /Math\.min\(64, Math\.max\(32, requestedHeight\)\)/)
  assert.match(virtualKeyboard, /<PictureInPicture2 v-if="isAttached"/)
  assert.match(virtualKeyboard, /<PanelBottomClose v-else/)
  assert.match(virtualKeyboard, /<X :size="14"/)
  assert.match(virtualKeyboard, /size="icon-sm"/)
  assert.match(virtualKeyboard, /:aria-label="isAttached \? t\('virtualKeyboard\.detach'\) : t\('virtualKeyboard\.attach'\)"/)
})

test('console popovers do not allow selecting local devices or feature backends', () => {
  assert.match(hidConfigPopover, /actionbar\.mouseSettings/)
  assert.doesNotMatch(hidConfigPopover, /actionbar\.hidDeviceSettings/)
  assert.doesNotMatch(hidConfigPopover, /HidBackend|ch9329_port|devicePath/)

  assert.match(videoConfigPopover, /actionbar\.streamSettings/)
  assert.match(videoConfigPopover, /actionbar\.videoFormat/)
  assert.match(videoConfigPopover, /actionbar\.videoResolution/)
  assert.match(videoConfigPopover, /actionbar\.videoFps/)
  assert.doesNotMatch(videoConfigPopover, /actionbar\.deviceSettings/)
  assert.doesNotMatch(videoConfigPopover, /actionbar\.videoDevice|handleDeviceChange|selectedDevice/)
})
