import { defineStore } from 'pinia'
import { ref } from 'vue'
import { useHidConfigStore } from '@/stores/hidConfig'

type MouseMode = 'absolute' | 'relative'

function storedMouseMode(): MouseMode {
  return localStorage.getItem('mouseMode') === 'relative' ? 'relative' : 'absolute'
}

export const useHidStore = defineStore('feature-hid', () => {
  const mouseMode = ref<MouseMode>(storedMouseMode())
  const cursorVisible = ref(localStorage.getItem('hidShowCursor') !== 'false')

  function setMouseMode(mode: MouseMode) {
    mouseMode.value = mode
    localStorage.setItem('mouseMode', mode)
  }

  function setCursorVisible(visible: boolean) {
    cursorVisible.value = visible
    localStorage.setItem('hidShowCursor', visible ? 'true' : 'false')
  }

  function syncMouseModeFromConfig() {
    const hidConfigStore = useHidConfigStore()
    const mouseAbsolute = hidConfigStore.hid?.mouse_absolute
    if (typeof mouseAbsolute !== 'boolean') return
    const nextMode: MouseMode = mouseAbsolute ? 'absolute' : 'relative'
    if (mouseMode.value !== nextMode) {
      mouseMode.value = nextMode
      localStorage.setItem('mouseMode', nextMode)
    }
  }

  function reset() {
    mouseMode.value = storedMouseMode()
    cursorVisible.value = localStorage.getItem('hidShowCursor') !== 'false'
  }

  return {
    mouseMode,
    cursorVisible,
    setMouseMode,
    setCursorVisible,
    syncMouseModeFromConfig,
    reset,
  }
})
