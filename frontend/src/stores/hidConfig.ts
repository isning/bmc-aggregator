import { DeviceScope } from '@/api/deviceScope'
import { RequestOrder } from '@/configuration/requestOrder'
import { currentBmcIdFromLocation } from '@/api/request'
import { defineStore } from 'pinia'
import { ref } from 'vue'
import { hidConfigApi } from '@/api'
import type { HidConfig, HidConfigUpdate } from '@/types/featureConfig'

function normalizeErrorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  return 'Unknown error'
}

export const useHidConfigStore = defineStore('hid-config', () => {
  const hid = ref<HidConfig | null>(null)
  const hidEtag = ref<string | null>(null)
  const hidLoading = ref(false)
  const hidError = ref<string | null>(null)
  const hidOrder = new RequestOrder()
  let hidPromise: Promise<HidConfig> | null = null

  let scope = currentBmcIdFromLocation()
  const deviceScope = new DeviceScope(currentBmcIdFromLocation)

  function ensureScope() {
    if (scope !== currentBmcIdFromLocation()) reset()
  }

  async function refreshHid() {
    ensureScope()
    if (hidLoading.value && hidPromise) return hidPromise
    const readTicket = hidOrder.read()
    const requestScope = deviceScope.capture()
    hidLoading.value = true
    hidError.value = null
    const request = hidConfigApi.get()
      .then((response) => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !hidOrder.accepts(readTicket)) return response.data
        hid.value = response.data
        hidEtag.value = response.etag
        return response.data
      })
      .catch((error) => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !hidOrder.accepts(readTicket)) throw error
        hidError.value = normalizeErrorMessage(error)
        throw error
      })
      .finally(() => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || hidPromise !== request) return
        hidLoading.value = false
        hidPromise = null
      })

    hidPromise = request
    return request
  }

  function ensureHid() {
    ensureScope()
    if (hid.value) return Promise.resolve(hid.value)
    return refreshHid()
  }

  async function updateHid(update: HidConfigUpdate) {
    ensureScope()
    if (!hidEtag.value) throw new Error('HID configuration version is unavailable; reload before saving')
    const writeTicket = hidOrder.write()
    hidPromise = null
    hidLoading.value = false
    const requestScope = deviceScope.capture()
    const response = await hidConfigApi.update(hidEtag.value, update).catch((error) => {
      hidOrder.finish(writeTicket)
      throw error
    })
    if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !hidOrder.accepts(writeTicket)) return response.data
    hidOrder.finish(writeTicket)
    hid.value = response.data
    hidEtag.value = response.etag
    return response.data
  }

  function reset() {
    scope = currentBmcIdFromLocation()
    deviceScope.reset()
    hidOrder.reset()
    hid.value = null
    hidEtag.value = null
    hidError.value = null
    hidPromise = null
    hidLoading.value = false
  }

  return {
    hid,
    hidLoading,
    hidError,
    hidEtag,
    refreshHid,
    ensureHid,
    updateHid,
    reset,
  }
})
