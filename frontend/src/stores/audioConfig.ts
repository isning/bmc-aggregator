import { DeviceScope } from '@/api/deviceScope'
import { RequestOrder } from '@/configuration/requestOrder'
import { currentBmcIdFromLocation } from '@/api/request'
import { defineStore } from 'pinia'
import { ref } from 'vue'
import { audioConfigApi } from '@/api'
import type { AudioConfig, AudioConfigUpdate } from '@/types/featureConfig'

function normalizeErrorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  return 'Unknown error'
}

export const useAudioConfigStore = defineStore('audio-config', () => {
  const audio = ref<AudioConfig | null>(null)
  const audioEtag = ref<string | null>(null)
  const audioLoading = ref(false)
  const audioError = ref<string | null>(null)
  const audioOrder = new RequestOrder()
  let audioPromise: Promise<AudioConfig> | null = null

  let scope = currentBmcIdFromLocation()
  const deviceScope = new DeviceScope(currentBmcIdFromLocation)

  function ensureScope() {
    if (scope !== currentBmcIdFromLocation()) reset()
  }

  async function refreshAudio() {
    ensureScope()
    if (audioLoading.value && audioPromise) return audioPromise
    const readTicket = audioOrder.read()
    const requestScope = deviceScope.capture()
    audioLoading.value = true
    audioError.value = null
    const request = audioConfigApi.get()
      .then((response) => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !audioOrder.accepts(readTicket)) return response.data
        audio.value = response.data
        audioEtag.value = response.etag
        return response.data
      })
      .catch((error) => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !audioOrder.accepts(readTicket)) throw error
        audioError.value = normalizeErrorMessage(error)
        throw error
      })
      .finally(() => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || audioPromise !== request) return
        audioLoading.value = false
        audioPromise = null
      })

    audioPromise = request
    return request
  }

  function ensureAudio() {
    ensureScope()
    if (audio.value) return Promise.resolve(audio.value)
    return refreshAudio()
  }

  async function updateAudio(update: AudioConfigUpdate) {
    ensureScope()
    if (!audioEtag.value) throw new Error('Audio configuration version is unavailable; reload before saving')
    const writeTicket = audioOrder.write()
    audioPromise = null
    audioLoading.value = false
    const requestScope = deviceScope.capture()
    const response = await audioConfigApi.update(audioEtag.value, update).catch((error) => {
      audioOrder.finish(writeTicket)
      throw error
    })
    if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !audioOrder.accepts(writeTicket)) return response.data
    audioOrder.finish(writeTicket)
    audio.value = response.data
    audioEtag.value = response.etag
    return response.data
  }

  function reset() {
    scope = currentBmcIdFromLocation()
    deviceScope.reset()
    audioOrder.reset()
    audio.value = null
    audioEtag.value = null
    audioError.value = null
    audioLoading.value = false
    audioPromise = null
  }

  return {
    audio,
    audioLoading,
    audioError,
    audioEtag,
    refreshAudio,
    ensureAudio,
    updateAudio,
    reset,
  }
})
