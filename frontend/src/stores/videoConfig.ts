import { DeviceScope } from '@/api/deviceScope'
import { RequestOrder } from '@/configuration/requestOrder'
import { currentBmcIdFromLocation } from '@/api/request'
import { defineStore } from 'pinia'
import { ref } from 'vue'
import { streamConfigApi, videoInputConfigApi } from '@/api'
import type {
  VideoInputConfig,
  VideoInputConfigUpdate,
} from '@/api'
import type {
  StreamConfigResponse,
  StreamConfigUpdate,
} from '@/types/featureConfig'

function normalizeErrorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  return 'Unknown error'
}

export const useVideoConfigStore = defineStore('video-config', () => {
  const video = ref<VideoInputConfig | null>(null)
  const stream = ref<StreamConfigResponse | null>(null)
  const videoEtag = ref<string | null>(null)
  const streamEtag = ref<string | null>(null)

  const videoLoading = ref(false)
  const streamLoading = ref(false)

  const videoError = ref<string | null>(null)
  const streamError = ref<string | null>(null)

  const videoOrder = new RequestOrder()
  let videoPromise: Promise<VideoInputConfig> | null = null
  const streamOrder = new RequestOrder()
  let streamPromise: Promise<StreamConfigResponse> | null = null

  let scope = currentBmcIdFromLocation()
  const deviceScope = new DeviceScope(currentBmcIdFromLocation)

  function ensureScope() {
    if (scope !== currentBmcIdFromLocation()) reset()
  }

  async function refreshVideo(bmcId?: string | null) {
    ensureScope()
    if (videoLoading.value && videoPromise) return videoPromise
    const readTicket = videoOrder.read()
    const requestScope = deviceScope.capture()
    videoLoading.value = true
    videoError.value = null
    const request = videoInputConfigApi.get(bmcId)
      .then((response) => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !videoOrder.accepts(readTicket)) return response.data
        video.value = response.data
        videoEtag.value = response.etag
        return response.data
      })
      .catch((error) => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !videoOrder.accepts(readTicket)) throw error
        videoError.value = normalizeErrorMessage(error)
        throw error
      })
      .finally(() => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || videoPromise !== request) return
        videoLoading.value = false
        videoPromise = null
      })

    videoPromise = request
    return request
  }

  async function refreshStream() {
    ensureScope()
    if (streamLoading.value && streamPromise) return streamPromise
    const readTicket = streamOrder.read()
    const requestScope = deviceScope.capture()
    streamLoading.value = true
    streamError.value = null
    const request = streamConfigApi.get()
      .then((response) => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !streamOrder.accepts(readTicket)) return response.data
        stream.value = response.data
        streamEtag.value = response.etag
        return response.data
      })
      .catch((error) => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !streamOrder.accepts(readTicket)) throw error
        streamError.value = normalizeErrorMessage(error)
        throw error
      })
      .finally(() => {
        if (!requestScope.current() || scope !== currentBmcIdFromLocation() || streamPromise !== request) return
        streamLoading.value = false
        streamPromise = null
      })

    streamPromise = request
    return request
  }

  function ensureVideo(bmcId?: string | null) {
    ensureScope()
    if (video.value) return Promise.resolve(video.value)
    return refreshVideo(bmcId)
  }

  function ensureStream() {
    ensureScope()
    if (stream.value) return Promise.resolve(stream.value)
    return refreshStream()
  }

  async function updateVideo(update: VideoInputConfigUpdate, bmcId?: string | null) {
    ensureScope()
    if (!videoEtag.value) throw new Error('Video configuration version is unavailable; reload before saving')
    const writeTicket = videoOrder.write()
    videoPromise = null
    videoLoading.value = false
    const requestScope = deviceScope.capture()
    const response = await videoInputConfigApi.update(videoEtag.value, update, bmcId).catch((error) => {
      videoOrder.finish(writeTicket)
      throw error
    })
    if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !videoOrder.accepts(writeTicket)) return response.data
    videoOrder.finish(writeTicket)
    video.value = response.data
    videoEtag.value = response.etag
    return response.data
  }

  async function updateStream(update: StreamConfigUpdate) {
    ensureScope()
    if (!streamEtag.value) throw new Error('Stream configuration version is unavailable; reload before saving')
    const writeTicket = streamOrder.write()
    streamPromise = null
    streamLoading.value = false
    const requestScope = deviceScope.capture()
    const response = await streamConfigApi.update(streamEtag.value, update).catch((error) => {
      streamOrder.finish(writeTicket)
      throw error
    })
    if (!requestScope.current() || scope !== currentBmcIdFromLocation() || !streamOrder.accepts(writeTicket)) return response.data
    streamOrder.finish(writeTicket)
    stream.value = response.data
    streamEtag.value = response.etag
    return response.data
  }

  function reset() {
    scope = currentBmcIdFromLocation()
    deviceScope.reset()
    videoOrder.reset()
    video.value = null
    streamOrder.reset()
    stream.value = null
    videoEtag.value = null
    streamEtag.value = null
    videoError.value = null
    streamError.value = null
    videoLoading.value = false
    streamLoading.value = false
    videoPromise = null
    streamPromise = null
  }

  return {
    video,
    stream,
    videoLoading,
    streamLoading,
    videoError,
    streamError,
    videoEtag,
    streamEtag,
    refreshVideo,
    refreshStream,
    ensureVideo,
    ensureStream,
    updateVideo,
    updateStream,
    reset,
  }
})
