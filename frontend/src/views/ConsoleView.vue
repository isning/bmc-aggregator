<script setup lang="ts">
import { defineAsyncComponent, ref, onMounted, onUnmounted, onActivated, onDeactivated, watch, nextTick, type WatchStopHandle } from 'vue'
import { useI18n } from 'vue-i18n'
import { DeviceScope } from '@/api/deviceScope'
import { currentBmcIdFromLocation } from '@/api/request'
import { useRoute, useRouter } from 'vue-router'
import { useSystemStore, type DeviceInfoEvent } from '@/stores/system'
import { useHidConfigStore } from '@/stores/hidConfig'
import { useAuthStore } from '@/stores/auth'
import { useWebSocket } from '@/composables/useWebSocket'
import { useConsoleEvents } from '@/composables/useConsoleEvents'
import { useHidEventBus } from '@/features/hid'
import { useWebRTC } from '@/composables/useWebRTC'
import { useWebTransport } from '@/composables/useWebTransport'
import { useVideoSession } from '@/composables/useVideoSession'
import { getUnifiedAudio } from '@/composables/useUnifiedAudio'
import { useHidInput } from '@/features/hid'
import { useVideoStream, VideoConsoleWindow } from '@/features/video'
import { streamApi, powerApi, powerConfigApi, authApi } from '@/api'
import { toast } from 'vue-sonner'
import { cn } from '@/lib/utils'

import ActionBar from '@/components/ActionBar.vue'
import FeatureStatusStrip from '@/components/FeatureStatusStrip.vue'
import InfoBar from '@/components/InfoBar.vue'
import LanguageToggleButton from '@/components/LanguageToggleButton.vue'
import BrandMark from '@/components/BrandMark.vue'
import { Button } from '@/components/ui/button'
import { Spinner } from '@/components/ui/spinner'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'

const VirtualKeyboard = defineAsyncComponent(() => import('@/components/VirtualKeyboard.vue'))
const StatsSheet = defineAsyncComponent(() => import('@/features/video/StatsSheet.vue'))
import {
  MonitorOff,
  RefreshCw,
  LogOut,
  Sun,
  Moon,
  ChevronDown,
  KeyRound,
  Loader2,
} from 'lucide-vue-next'

const { t } = useI18n()
const router = useRouter()
const route = useRoute()
const systemStore = useSystemStore()
const hidConfigStore = useHidConfigStore()
const authStore = useAuthStore()
const { connected: wsConnected, networkError: wsNetworkError } = useWebSocket()
const hidBus = useHidEventBus()
const webrtc = useWebRTC()
const webtransport = useWebTransport()
const unifiedAudio = getUnifiedAudio()
const videoSession = useVideoSession()

let consoleEvents: ReturnType<typeof useConsoleEvents>

const videoRef = ref<HTMLImageElement | null>(null)
const webrtcVideoRef = ref<HTMLVideoElement | null>(null)
const webTransportCanvasRef = ref<HTMLCanvasElement | null>(null)
const videoContainerRef = ref<HTMLDivElement | null>(null)
const isFullscreen = ref(false)

const isConsoleActive = ref(false)
const consoleDeviceId = currentBmcIdFromLocation()
const activationScope = new DeviceScope(currentBmcIdFromLocation)
let consoleDisposed = false

function captureConsoleWork() {
  const owner = activationScope.capture()
  return () => !consoleDisposed && isConsoleActive.value && owner.current()
}

const statsSheetOpen = ref(false)

const changePasswordDialogOpen = ref(false)
const currentPassword = ref('')
const newPassword = ref('')
const confirmPassword = ref('')
const changingPassword = ref(false)

const isDark = ref(document.documentElement.classList.contains('dark'))
let consoleEventsSubscribed = false
const stopHandles: WatchStopHandle[] = []

const videoStream = useVideoStream({
  videoRef,
  webrtcVideoRef,
  webTransportCanvasRef,
  videoContainerRef,
  webrtc,
  webtransport,
  videoSession,
  unifiedAudio,
  isConsoleActive,
})
const {
  videoMode,
  httpModes,
  activeTransport,
  isHttpMode,
  isWebTransportMode,
  backendFps,
  mjpegFrameReceived,
  videoErrorMessage,
  backendPerf,
  frameOverlayUrl,
  mjpegFrameUrl,
  lifecyclePresentation,
  signalOverlayInfo,
  loadingMessage: webrtcLoadingMessage,
  connectProgress,
  videoSourceSize,
  refreshVideo,
  resetVideoStream,
  connectWebRTCOnly,
  connectWebTransportOnly,
  rebindWebRTCVideo,
  handleVideoModeChange,
  handleVideoLoad,
  syncVideoSourceSize,
  handleVideoError,
  handleVideoFeatureFault,
  handleStreamDeviceLost,
  handleStreamRecovered,
  handleStreamConfigChanging,
  handleStreamConfigApplied,
  handleWebRTCReady,
  handleStreamModeReady,
  handleStreamModeSwitching,
  handleStreamStateChanged,
  handleStreamStatsUpdate,
  handleAudioStateChanged,
  onDeviceInfo: handleVideoDeviceInfo,
  handleStreamModeChanged,
  normalizeServerMode,
  restoreInitialMode,
  setupWatchers: setupVideoWatchers,
  cleanup: cleanupVideoStream,
} = videoStream

const hidInput = useHidInput({
  videoContainerRef,
  isHttpMode,
  isFullscreen,
  webrtc,
  videoRef,
  webrtcVideoRef,
  webTransportCanvasRef,
})
const {
  mouseMode,
  pressedKeys,
  keyboardLed,
  keyboardLedEnabled,
  mousePosition,
  localCrosshairPos,
  cursorVisible,
  virtualKeyboardVisible,
  virtualKeyboardAttached,
  virtualKeyboardConsumerEnabled,
  handleMouseLeaveVideo,
  handleMouseMove,
  handleMouseDown,
  handleMouseUp,
  handleWheel,
  handleContextMenu,
  handleToggleMouseMode,
  handleToggleVirtualKeyboard,
  captureKeyboard,
  registerInteractionListeners,
  unregisterInteractionListeners,
  init: initHidInput,
  dispose: disposeHidInput,
  syncMouseModeFromConfig,
} = hidInput

function handleDeviceInfo(data: DeviceInfoEvent) {
  const result = systemStore.updateFromDeviceInfo(data)

  if (result.audioChanged) {
    void handleAudioStateChanged({
      streaming: result.audio.streaming,
      device: result.audio.device,
    })
  }

  handleVideoDeviceInfo(data)
}

function handleConnectionStateChanged(data: { feature: string; connection_id?: string; state: string }) {
  systemStore.updateFeatureState(data.feature, data.state, data.connection_id ?? 'default')
}

function handleConnectionHealth(data: {
  feature: string
  connection_id?: string
  online: boolean
  fps: number
  width?: number
  height?: number
  resolution?: [number, number]
}) {
  systemStore.updateFeatureHealth(
    data.feature,
    {
      online: data.online,
      fps: data.fps,
      width: data.width,
      height: data.height,
      resolution: data.resolution,
    },
    data.connection_id ?? 'default',
  )
}

function handleFeatureFaultChanged(data: {
  feature: string
  connection_id?: string
  fault: import('@/api/request').FeatureFault | null
}) {
  systemStore.updateFeatureFault(data.feature, data.fault, data.connection_id ?? 'default')
  if (data.feature === 'video') {
    handleVideoFeatureFault(data.fault)
  }
}

async function toggleFullscreen() {
  if (!videoContainerRef.value) return
  if (!document.fullscreenElement) {
    await videoContainerRef.value.requestFullscreen()
    isFullscreen.value = true
  } else {
    await document.exitFullscreen()
    isFullscreen.value = false
  }
}

function toggleTheme() {
  isDark.value = !isDark.value
  document.documentElement.classList.toggle('dark', isDark.value)
  localStorage.setItem('theme', isDark.value ? 'dark' : 'light')
}

async function logout() {
  await authStore.logout()
  router.push('/login')
}

async function handleChangePassword() {
  if (!newPassword.value || !confirmPassword.value) {
    toast.error(t('auth.passwordRequired'))
    return
  }

  if (newPassword.value !== confirmPassword.value) {
    toast.error(t('auth.passwordMismatch'))
    return
  }

  if (newPassword.value.length < 4) {
    toast.error(t('auth.passwordTooShort'))
    return
  }

  changingPassword.value = true
  try {
    await authApi.changePassword(currentPassword.value, newPassword.value)

    currentPassword.value = ''
    newPassword.value = ''
    confirmPassword.value = ''
    changePasswordDialogOpen.value = false
  } catch (e) {
    console.info('[ChangePassword] Failed:', e)
  } finally {
    changingPassword.value = false
  }
}

async function handlePowerShort() {
  try {
    await powerApi.power('short')
    await systemStore.fetchAtxState()
  } catch {
    toast.error(t('atx.powerActionFailed'))
  }
}

async function handlePowerLong() {
  try {
    await powerApi.power('long')
    await systemStore.fetchAtxState()
  } catch {
    toast.error(t('atx.powerActionFailed'))
  }
}

async function handleReset() {
  try {
    await powerApi.power('reset')
    await systemStore.fetchAtxState()
  } catch {
    toast.error(t('atx.powerActionFailed'))
  }
}

async function handleWol(mac: string) {
  try {
    await powerConfigApi.sendWol(mac)
  } catch (e) {
    toast.error(t('atx.wolFailed'))
  }
}

async function handleVideoTransportChange(transport: 'webrtc' | 'webtransport') {
  if (isHttpMode.value) return
  if (transport === 'webtransport') {
    await connectWebTransportOnly(videoMode.value)
    return
  }
  await connectWebRTCOnly(videoMode.value)
}

async function activateConsoleView() {
  if (consoleDisposed || currentBmcIdFromLocation() !== consoleDeviceId) return
  isConsoleActive.value = true
  const current = captureConsoleWork()
  initHidInput()
  setupVideoWatchers()
  if (!consoleEventsSubscribed) {
    consoleEvents.subscribe()
    consoleEventsSubscribed = true
  }
  registerInteractionListeners()

  void systemStore.fetchAllStates()
  void hidConfigStore.refreshHid().then(() => { if (current()) syncMouseModeFromConfig() }).catch(() => {})

  if (!hidBus.connected.value) {
    hidBus.connect().then(() => {
      if (current()) systemStore.fetchHidState().catch(() => {})
    }).catch(() => {})
  }

  if (!isHttpMode.value && webrtc.videoTrack.value) {
    await nextTick()
    if (!current()) return
    await rebindWebRTCVideo()
    return
  }

  if (
    !isHttpMode.value
    && !isWebTransportMode.value
    && !webrtc.isConnected.value
    && !webrtc.isConnecting.value
    && !videoSession.localSwitching.value
    && !videoSession.backendSwitching.value
  ) {
    await connectWebRTCOnly(videoMode.value)
  }
}

function deactivateConsoleView() {
  activationScope.reset()
  isConsoleActive.value = false
  if (consoleEventsSubscribed) {
    consoleEvents.unsubscribe()
    consoleEventsSubscribed = false
  }
  unregisterInteractionListeners()
  disposeHidInput()
  hidBus.disconnect()
}

consoleEvents = useConsoleEvents({
  onStreamConfigChanging: handleStreamConfigChanging,
  onStreamConfigApplied: handleStreamConfigApplied,
  onStreamStatsUpdate: handleStreamStatsUpdate,
  onStreamModeChanged: handleStreamModeChanged,
  onStreamModeSwitching: handleStreamModeSwitching,
  onStreamModeReady: handleStreamModeReady,
  onWebRTCReady: handleWebRTCReady,
  onStreamStateChanged: handleStreamStateChanged,
  onStreamDeviceLost: handleStreamDeviceLost,
  onStreamRecovered: handleStreamRecovered,
  onDeviceInfo: handleDeviceInfo,
  onConnectionStateChanged: handleConnectionStateChanged,
  onConnectionHealth: handleConnectionHealth,
  onFeatureFaultChanged: handleFeatureFaultChanged,
})

onMounted(async () => {
  const current = captureConsoleWork()
  systemStore.resetDeviceState()
  hidConfigStore.reset()
  setupVideoWatchers()

  stopHandles.push(watch([wsConnected, wsNetworkError], ([connected, netError], [_prevConnected, prevNetError]) => {
    systemStore.updateWsConnection(connected, netError)

    if (prevNetError === true && netError === false && connected === true) {
      refreshVideo()
    }
  }, { immediate: true }))

  stopHandles.push(watch([() => hidBus.connected.value, () => hidBus.networkError.value], ([connected, netError]) => {
    systemStore.updateHidEventBusConnection(connected, netError)
  }, { immediate: true }))

  await systemStore.fetchSystemInfo().catch(() => {})
  if (!current()) return
  await systemStore.fetchAllStates()
  if (!current()) return
  await videoStream.refreshVideoBackendStats().catch(() => {})
  if (!current()) return
  await hidConfigStore.refreshHid().then(() => {
    if (current()) syncMouseModeFromConfig()
  }).catch(() => {})
  if (!current()) return

  stopHandles.push(watch(() => hidConfigStore.hid?.mouse_absolute, () => {
    syncMouseModeFromConfig()
  }))

  const storedTheme = localStorage.getItem('theme')
  if (storedTheme === 'dark' || (!storedTheme && window.matchMedia('(prefers-color-scheme: dark)').matches)) {
    isDark.value = true
    document.documentElement.classList.add('dark')
  }

  try {
    const modeResp = await streamApi.getMode()
    if (!current()) return
    const serverMode = normalizeServerMode(modeResp?.mode)
    if (serverMode) {
      await restoreInitialMode(serverMode)
    }
  } catch (err) {
    console.warn('[Console] Failed to fetch stream mode on enter, fallback to WS events:', err)
  }
})

onActivated(() => {
  void activateConsoleView()
})

onDeactivated(() => {
  deactivateConsoleView()
})

onUnmounted(() => {
  consoleDisposed = true
  deactivateConsoleView()

  cleanupVideoStream()
  videoSession.clearWaiters()
  while (stopHandles.length) {
    stopHandles.pop()?.()
  }

  if (webrtc.isConnected.value || webrtc.isConnecting.value) {
    void webrtc.disconnect()
  }

})
</script>

<template>
  <div class="h-screen h-dvh flex flex-col bg-background">
    <header class="shrink-0 border-b border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900">
      <div class="px-2 sm:px-4">
        <div class="h-10 sm:h-14 flex items-center justify-between">
          <div class="flex items-center gap-2 sm:gap-6">
            <div class="flex items-center gap-1.5 sm:gap-2">
              <BrandMark size="md" class="hidden sm:block" />
              <BrandMark size="sm" class="sm:hidden" />
              <span class="font-bold text-sm sm:text-lg">One-KVM</span>
            </div>
            <div class="flex md:hidden items-center gap-1">
              <FeatureStatusStrip compact :limit="2" />
            </div>
          </div>
          <div class="flex items-center gap-1 sm:gap-2">
            <div class="hidden md:flex items-center gap-2">
              <FeatureStatusStrip />
            </div>
            <div class="h-6 w-px bg-slate-200 dark:bg-slate-700 hidden md:block mx-1" />
            <Button variant="ghost" size="icon" class="h-8 w-8 hidden md:flex" :aria-label="t('common.toggleTheme')" @click="toggleTheme">
              <Sun v-if="isDark" class="h-4 w-4" />
              <Moon v-else class="h-4 w-4" />
            </Button>
            <LanguageToggleButton class="h-8 w-8 hidden md:flex" />
            <DropdownMenu>
              <DropdownMenuTrigger as-child>
                <Button variant="outline" size="sm" class="gap-1 sm:gap-1.5 h-7 sm:h-9 px-2 sm:px-3">
                  <span class="text-xs max-w-[60px] sm:max-w-[100px] truncate">{{ authStore.user || 'admin' }}</span>
                  <ChevronDown class="h-3 w-3 sm:h-3.5 sm:w-3.5" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem class="md:hidden" @click="toggleTheme">
                  <Sun v-if="isDark" class="h-4 w-4 mr-2" />
                  <Moon v-else class="h-4 w-4 mr-2" />
                  {{ isDark ? t('settings.lightMode') : t('settings.darkMode') }}
                </DropdownMenuItem>
                <DropdownMenuItem as-child class="md:hidden p-0">
                  <LanguageToggleButton
                    label-mode="next"
                    size="sm"
                    variant="ghost"
                    class="w-full justify-start rounded-sm px-2 py-1.5 font-normal shadow-none"
                  />
                </DropdownMenuItem>
                <DropdownMenuSeparator class="md:hidden" />
                <DropdownMenuItem @click="changePasswordDialogOpen = true">
                  <KeyRound class="h-4 w-4 mr-2" />
                  {{ t('auth.changePassword') }}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem @click="logout">
                  <LogOut class="h-4 w-4 mr-2" />
                  {{ t('auth.logout') }}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>
      </div>
    </header>
    <ActionBar
      v-if="isConsoleActive"
      :key="String(route.params.bmcId)"
      :mouse-mode="mouseMode"
      :video-mode="videoMode"
      @toggle-fullscreen="toggleFullscreen"
      @toggle-stats="statsSheetOpen = true"
      @toggle-virtual-keyboard="handleToggleVirtualKeyboard"
      @toggle-mouse-mode="handleToggleMouseMode"
      @update:video-mode="handleVideoModeChange"
      @update-video-transport="handleVideoTransportChange"
      @power-short="handlePowerShort"
      @power-long="handlePowerLong"
      @reset="handleReset"
      @wol="handleWol"
    />
    <div class="flex-1 overflow-hidden relative">
      <div
        class="absolute inset-0 bg-slate-100/80 dark:bg-slate-800/40 opacity-80"
        style="
          background-image: radial-gradient(circle, rgb(148 163 184 / 0.4) 1px, transparent 1px);
          background-size: 20px 20px;
        "
      />
      <div class="relative h-full w-full">
        <VideoConsoleWindow
          :source-width="videoSourceSize?.width ?? null"
          :source-height="videoSourceSize?.height ?? null"
        >
          <div
            ref="videoContainerRef"
            class="relative h-full w-full bg-black overflow-hidden flex items-center justify-center cursor-none"
            tabindex="0"
            @mouseleave="handleMouseLeaveVideo"
            @mousemove="handleMouseMove"
            @mousedown="handleMouseDown"
            @mouseup="handleMouseUp"
            @wheel.prevent="handleWheel"
            @contextmenu="handleContextMenu"
          >
          <img
            v-if="isHttpMode && lifecyclePresentation.mountSurface && mjpegFrameUrl"
            ref="videoRef"
            :src="mjpegFrameUrl"
            class="w-full h-full object-contain transition-opacity duration-150"
            :class="mjpegFrameReceived && lifecyclePresentation.showSurface ? 'opacity-100' : 'opacity-0'"
            alt=""
            @load="handleVideoLoad"
            @error="handleVideoError"
          />
          <video
            v-if="!isHttpMode && !isWebTransportMode && lifecyclePresentation.mountSurface"
            ref="webrtcVideoRef"
            class="w-full h-full object-contain transition-opacity duration-150"
            :class="lifecyclePresentation.showSurface ? 'opacity-100' : 'opacity-0'"
            autoplay
            playsinline
            @resize="syncVideoSourceSize"
          />
          <canvas
            v-if="isWebTransportMode && lifecyclePresentation.mountSurface"
            ref="webTransportCanvasRef"
            class="w-full h-full object-contain transition-opacity duration-150"
            :class="lifecyclePresentation.showSurface ? 'opacity-100' : 'opacity-0'"
          />
          <img
            v-if="frameOverlayUrl && lifecyclePresentation.showSurface"
            :src="frameOverlayUrl"
            class="absolute inset-0 w-full h-full object-contain pointer-events-none"
            alt=""
          />
          <div
            v-if="cursorVisible && localCrosshairPos"
            class="pointer-events-none absolute z-[15] -translate-x-1/2 -translate-y-1/2"
            :style="{
              left: `${localCrosshairPos.x}px`,
              top: `${localCrosshairPos.y}px`,
            }"
            aria-hidden="true"
          >
            <svg
              width="23"
              height="23"
              viewBox="-11.5 -11.5 23 23"
              class="overflow-visible"
            >
              <g stroke-linecap="square">
                <line
                  x1="0"
                  y1="-10"
                  x2="0"
                  y2="10"
                  stroke="rgba(0,0,0,0.88)"
                  stroke-width="3"
                />
                <line
                  x1="-10"
                  y1="0"
                  x2="10"
                  y2="0"
                  stroke="rgba(0,0,0,0.88)"
                  stroke-width="3"
                />
                <line
                  x1="0"
                  y1="-10"
                  x2="0"
                  y2="10"
                  stroke="rgba(255,255,255,0.95)"
                  stroke-width="1"
                />
                <line
                  x1="-10"
                  y1="0"
                  x2="10"
                  y2="0"
                  stroke="rgba(255,255,255,0.95)"
                  stroke-width="1"
                />
              </g>
            </svg>
          </div>
          <Transition name="fade">
            <div
              v-if="lifecyclePresentation.showLoading"
              class="absolute inset-0 flex flex-col items-center justify-center bg-black/70 backdrop-blur-sm transition-opacity duration-300"
            >
              <div class="absolute inset-0 overflow-hidden pointer-events-none">
                <div class="absolute w-full h-0.5 bg-gradient-to-r from-transparent via-primary/40 to-transparent animate-pulse" style="top: 50%; animation-duration: 1.5s;" />
              </div>

              <Spinner class="h-10 w-10 sm:h-16 sm:w-16 text-white mb-2 sm:mb-4" />
              <p class="text-white/90 text-sm sm:text-lg font-medium text-center px-4 flex items-baseline justify-center gap-2 flex-wrap">
                <span>{{ webrtcLoadingMessage }}</span>
                <span
                  v-if="connectProgress"
                  class="text-white/55 text-xs sm:text-sm font-normal tabular-nums"
                  :aria-label="t('console.stepProgress', { current: connectProgress.current, total: connectProgress.total })"
                >
                  {{ connectProgress.current }}/{{ connectProgress.total }}
                </span>
              </p>
              <div
                v-if="connectProgress"
                class="mt-2 sm:mt-3 flex items-center gap-1"
                role="progressbar"
                :aria-valuenow="connectProgress.current"
                :aria-valuemin="0"
                :aria-valuemax="connectProgress.total"
                :aria-valuetext="t('console.stepProgress', { current: connectProgress.current, total: connectProgress.total })"
              >
                <span
                  v-for="i in connectProgress.total"
                  :key="i"
                  :class="cn(
                    'h-1 w-4 sm:w-6 rounded-full transition-colors duration-300',
                    i <= connectProgress.current
                      ? 'bg-primary'
                      : 'bg-white/15',
                  )"
                />
              </div>
              <p class="text-white/50 text-xs sm:text-sm mt-1 sm:mt-2">
                {{ t('console.pleaseWait') }}
              </p>
            </div>
          </Transition>
          <Transition name="fade">
            <div
              v-if="lifecyclePresentation.showSignalOverlay"
              class="absolute inset-0 flex flex-col items-center justify-center gap-3 p-4 transition-opacity duration-300 pointer-events-none"
              :class="{
                'bg-black/80 backdrop-blur-sm': signalOverlayInfo.tone === 'error',
                'bg-black/70 backdrop-blur-sm': signalOverlayInfo.tone !== 'error',
              }"
            >
              <MonitorOff
                class="h-10 w-10 sm:h-16 sm:w-16"
                :class="{
                  'text-slate-200': signalOverlayInfo.tone === 'info',
                  'text-red-300': signalOverlayInfo.tone === 'error',
                }"
              />
              <div class="text-center max-w-md">
                <p
                  class="font-semibold text-sm sm:text-lg text-white"
                >{{ signalOverlayInfo.title }}</p>
                <p
                  class="text-xs sm:text-sm mt-1 sm:mt-2"
                  :class="{
                    'text-slate-200/80': signalOverlayInfo.tone === 'info',
                    'text-red-100/80': signalOverlayInfo.tone === 'error',
                  }"
                >{{ signalOverlayInfo.detail }}</p>
                <p
                  v-if="signalOverlayInfo.hint"
                  class="text-[11px] sm:text-xs mt-2 text-white/50"
                >{{ signalOverlayInfo.hint }}</p>
              </div>
            </div>
          </Transition>
          <Transition name="fade">
            <div
              v-if="lifecyclePresentation.showError"
              class="absolute inset-0 flex flex-col items-center justify-center bg-black/85 text-white gap-4 transition-opacity duration-300 p-4"
            >
              <MonitorOff class="h-10 w-10 sm:h-16 sm:w-16 text-slate-400" />
              <div class="text-center max-w-md px-2">
                <p class="font-medium text-sm sm:text-lg mb-1 sm:mb-2">{{ t('console.connectionFailed') }}</p>
                <p class="text-xs sm:text-sm text-slate-300 mb-2 sm:mb-3">{{ t('console.connectionFailedDesc') }}</p>
                <div v-if="videoErrorMessage" class="bg-slate-800/60 rounded-lg p-3 text-left">
                  <p class="text-xs text-slate-400 mb-1">{{ t('console.errorDetails') }}:</p>
                  <p class="text-sm text-slate-300 font-mono break-all">{{ videoErrorMessage }}</p>
                </div>
              </div>
              <div class="flex gap-2">
                <Button variant="secondary" size="sm" @click="resetVideoStream">
                  <RefreshCw class="h-4 w-4 mr-2" />
                  {{ t('console.reconnect') }}
                </Button>
              </div>
            </div>
          </Transition>
          </div>
        </VideoConsoleWindow>
      </div>
    </div>
    <Teleport :to="virtualKeyboardAttached ? '#keyboard-anchor' : 'body'" :disabled="virtualKeyboardAttached">
      <VirtualKeyboard
        v-if="virtualKeyboardVisible"
        v-model:visible="virtualKeyboardVisible"
        v-model:attached="virtualKeyboardAttached"
        :caps-lock="keyboardLed.capsLock"
        :pressed-keys="pressedKeys"
        :consumer-enabled="virtualKeyboardConsumerEnabled"
        :capture-keyboard="captureKeyboard"
      />
    </Teleport>
    <div id="keyboard-anchor"></div>
    <InfoBar
      :pressed-keys="pressedKeys"
      :caps-lock="keyboardLed.capsLock"
      :num-lock="keyboardLed.numLock"
      :scroll-lock="keyboardLed.scrollLock"
      :keyboard-led-enabled="keyboardLedEnabled"
      :mouse-position="mousePosition"
      :debug-mode="false"
    />
    <StatsSheet
      v-model:open="statsSheetOpen"
      :video-mode="videoMode"
      :http-modes="httpModes"
      :mjpeg-fps="backendFps"
      :active-transport="activeTransport"
      :webrtc-stats="webrtc.stats.value"
      :webtransport-stats="webtransport.stats.value"
      :backend-perf="backendPerf ?? undefined"
      :webrtc-connection-id="webrtc.connectionId.value"
      :webtransport-connection-id="webtransport.connectionId.value"
    />
    <Dialog v-model:open="changePasswordDialogOpen">
      <DialogContent class="w-[95vw] max-w-md">
        <DialogHeader>
          <DialogTitle>{{ t('auth.changePassword') }}</DialogTitle>
        </DialogHeader>
        <div class="space-y-3 sm:space-y-4 py-2 sm:py-4">
          <div class="space-y-2">
            <Label for="currentPassword">{{ t('auth.currentPassword') }}</Label>
            <Input
              id="currentPassword"
              v-model="currentPassword"
              type="password"
              :placeholder="t('auth.currentPasswordPlaceholder')"
            />
          </div>
          <div class="space-y-2">
            <Label for="newPassword">{{ t('auth.newPassword') }}</Label>
            <Input
              id="newPassword"
              v-model="newPassword"
              type="password"
              :placeholder="t('auth.newPasswordPlaceholder')"
            />
          </div>
          <div class="space-y-2">
            <Label for="confirmPassword">{{ t('auth.confirmPassword') }}</Label>
            <Input
              id="confirmPassword"
              v-model="confirmPassword"
              type="password"
              :placeholder="t('auth.confirmPasswordPlaceholder')"
            />
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" @click="changePasswordDialogOpen = false">
            {{ t('common.cancel') }}
          </Button>
          <Button @click="handleChangePassword" :disabled="changingPassword">
            <Loader2 v-if="changingPassword" class="h-4 w-4 mr-2 animate-spin" />
            {{ t('common.confirm') }}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>

  </div>
</template>

<style scoped>
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.3s ease;
}

.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
</style>
