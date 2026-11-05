import { ref } from 'vue'
import { defineStore } from 'pinia'
import { audioApi, configApi } from '@/api'
import { useAudioConfigStore } from '@/stores/audioConfig'
import { getUnifiedAudio } from '@/composables/useUnifiedAudio'

interface AudioDevice {
  name: string
  description: string
}

export const useAudioStore = defineStore('feature-audio', () => {
  const devices = ref<AudioDevice[]>([])
  const loadingDevices = ref(false)
  const applying = ref(false)

  const audioEnabled = ref(false)
  const selectedDevice = ref('')
  const selectedQuality = ref<'voice' | 'balanced' | 'high'>('balanced')

  async function loadDevices() {
    loadingDevices.value = true
    try {
      const result = await configApi.listDevices()
      devices.value = result.audio
    } catch (e) {
      console.info('[AudioStore] Failed to load devices')
    } finally {
      loadingDevices.value = false
    }
  }

  function initializeFromCurrent() {
    const audioConfigStore = useAudioConfigStore()
    const audio = audioConfigStore.audio
    if (audio) {
      audioEnabled.value = audio.enabled
      selectedDevice.value = audio.device || ''
      selectedQuality.value = (audio.quality as 'voice' | 'balanced' | 'high') || 'balanced'
    }
  }

  async function applyConfig() {
    const audioConfigStore = useAudioConfigStore()
    const unifiedAudio = getUnifiedAudio()
    applying.value = true

    try {
      await audioConfigStore.updateAudio({
        enabled: audioEnabled.value,
        device: selectedDevice.value,
        quality: selectedQuality.value,
      })

      if (audioEnabled.value && selectedDevice.value) {
        try {
          await audioApi.start()
        } catch (startError) {
          console.info('[AudioStore] Audio start failed:', startError)
        }
      } else if (!audioEnabled.value) {
        unifiedAudio.setVolume(0)
        try {
          await audioApi.stop()
        } catch {
        }
        unifiedAudio.disconnect()
      }
    } catch (e) {
      console.info('[AudioStore] Failed to apply config:', e)
    } finally {
      applying.value = false
    }
  }

  function reset() {
    devices.value = []
    loadingDevices.value = false
    applying.value = false
    audioEnabled.value = false
    selectedDevice.value = ''
    selectedQuality.value = 'balanced'
  }

  const setAudioEnabled = (v: boolean) => { audioEnabled.value = v }
  const setSelectedDevice = (v: string) => { selectedDevice.value = v }
  const setSelectedQuality = (v: 'voice' | 'balanced' | 'high') => { selectedQuality.value = v }

  return {
    devices, loadingDevices, applying,
    audioEnabled, selectedDevice, selectedQuality,
    loadDevices, initializeFromCurrent, applyConfig,
    setAudioEnabled, setSelectedDevice, setSelectedQuality,
    reset,
  }
})
