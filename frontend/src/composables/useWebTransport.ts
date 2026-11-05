import { computed, ref, shallowRef } from 'vue'
import { createWebTransportApi, type WebTransportCapabilitiesResponse } from '@/api'
import { captureDeviceRequest, currentBmcIdFromLocation } from '@/api/request'
import { generateUUID } from '@/lib/utils'

type WebTransportCtor = new (url: string, options?: Record<string, unknown>) => {
  ready: Promise<void>
  closed: Promise<void>
  close: () => void
  incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>
}

type WebCodecsDecoder = {
  configure: (config: Record<string, unknown>) => void
  decode: (chunk: unknown) => void
  close: () => void
  state?: string
  decodeQueueSize?: number
}

type WebCodecsFrame = CanvasImageSource & {
  displayWidth: number
  displayHeight: number
  close: () => void
}

type WebCodecsDecoderCtor = {
  new (callbacks: { output: (frame: WebCodecsFrame) => void; error: (error: Error) => void }): WebCodecsDecoder
  isConfigSupported?: (config: Record<string, unknown>) => Promise<{ supported: boolean }>
}

type EncodedVideoChunkCtor = new (init: {
  type: 'key' | 'delta'
  timestamp: number
  duration: number
  data: Uint8Array
}) => unknown

type WebTransportWindow = Window & {
  WebTransport?: WebTransportCtor
  VideoDecoder?: WebCodecsDecoderCtor
  EncodedVideoChunk?: EncodedVideoChunkCtor
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

interface FrameMetadata {
  codec: string
  timestamp_us: number
  duration_us: number
  keyframe: boolean
  width: number
  height: number
  trace_id?: number
  pixel_format?: string
}

const webcodecsCodecStrings: Record<string, string> = {
  h264: 'avc1.42E01F',
  h265: 'hev1.1.6.L93.B0',
  vp8: 'vp8',
  vp9: 'vp09.00.10.08',
  av1: 'av01.0.05M.08',
}

const WEBTRANSPORT_READY_TIMEOUT_MS = 8000

export interface WebTransportStats {
  framesDecoded: number
  framesDropped: number
  bytesReceived: number
  packetsReceived: number
  keyframesReceived: number
  frameWidth: number
  frameHeight: number
  framesPerSecond: number
  bitrateMbps: number
  codec: string
  pixelFormat: string
  lastTraceId: number
  lastFrameTimestampUs: number
  lastFrameDurationUs: number
  lastFrameAgeMs: number
  decoderQueueSize: number
  packetsBuffered: number
}

export function useWebTransport() {
  const state = ref<'idle' | 'connecting' | 'connected' | 'failed' | 'closed'>('idle')
  const error = ref('')
  const connectionId = ref('')
  const capabilities = shallowRef<WebTransportCapabilitiesResponse | null>(null)
  const canvasRef = shallowRef<HTMLCanvasElement | null>(null)
  const stats = ref<WebTransportStats>({
    framesDecoded: 0,
    framesDropped: 0,
    bytesReceived: 0,
    packetsReceived: 0,
    keyframesReceived: 0,
    frameWidth: 0,
    frameHeight: 0,
    framesPerSecond: 0,
    bitrateMbps: 0,
    codec: '',
    pixelFormat: '',
    lastTraceId: 0,
    lastFrameTimestampUs: 0,
    lastFrameDurationUs: 0,
    lastFrameAgeMs: 0,
    decoderQueueSize: 0,
    packetsBuffered: 0,
  })

  let transport: InstanceType<WebTransportCtor> | null = null
  let decoder: WebCodecsDecoder | null = null
  let abortController: AbortController | null = null
  let connectionGeneration = 0
  let capabilitiesDeviceId: string | null = null
  let lastStatsSampleAt = 0
  let lastStatsBytes = 0
  let lastStatsFrames = 0
  let lastFrameReceivedAt = 0

  function isActiveTransport(activeTransport: InstanceType<WebTransportCtor>, generation: number) {
    return generation === connectionGeneration && transport === activeTransport
  }

  function assertActiveGeneration(generation: number, deviceId: string | null) {
    if (generation !== connectionGeneration || deviceId !== currentBmcIdFromLocation()) {
      throw new Error('webtransport_connection_superseded')
    }
  }

  function resetStats(codec = '') {
    stats.value = {
      framesDecoded: 0,
      framesDropped: 0,
      bytesReceived: 0,
      packetsReceived: 0,
      keyframesReceived: 0,
      frameWidth: 0,
      frameHeight: 0,
      framesPerSecond: 0,
      bitrateMbps: 0,
      codec,
      pixelFormat: '',
      lastTraceId: 0,
      lastFrameTimestampUs: 0,
      lastFrameDurationUs: 0,
      lastFrameAgeMs: 0,
      decoderQueueSize: 0,
      packetsBuffered: 0,
    }
    lastStatsSampleAt = performance.now()
    lastStatsBytes = 0
    lastStatsFrames = 0
    lastFrameReceivedAt = 0
  }

  function updateDerivedStats(now = performance.now()) {
    if (lastFrameReceivedAt > 0) {
      stats.value.lastFrameAgeMs = Math.max(0, now - lastFrameReceivedAt)
    }
    const elapsed = (now - lastStatsSampleAt) / 1000
    if (elapsed < 0.5) return
    const bytesDelta = Math.max(0, stats.value.bytesReceived - lastStatsBytes)
    const framesDelta = Math.max(0, stats.value.framesDecoded - lastStatsFrames)
    stats.value.bitrateMbps = Math.round(((bytesDelta * 8) / elapsed / 1_000_000) * 100) / 100
    stats.value.framesPerSecond = Math.round((framesDelta / elapsed) * 10) / 10
    lastStatsSampleAt = now
    lastStatsBytes = stats.value.bytesReceived
    lastStatsFrames = stats.value.framesDecoded
  }

  function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
    let timeoutId: ReturnType<typeof setTimeout> | null = null
    return Promise.race([
      promise.finally(() => {
        if (timeoutId !== null) clearTimeout(timeoutId)
      }),
      new Promise<T>((_, reject) => {
        timeoutId = setTimeout(() => reject(new Error(message)), timeoutMs)
      }),
    ])
  }

  const browserSupported = computed(() => {
    return typeof window !== 'undefined'
      && 'WebTransport' in window
      && 'VideoDecoder' in window
      && window.isSecureContext
  })

  const isConnecting = computed(() => state.value === 'connecting')
  const isConnected = computed(() => state.value === 'connected')

  async function refreshCapabilities() {
    const deviceId = currentBmcIdFromLocation()
    const generation = connectionGeneration
    const api = createWebTransportApi(captureDeviceRequest().request)
    const response = await api.getWebTransportCapabilities()
    if (generation === connectionGeneration && deviceId === currentBmcIdFromLocation()) {
      capabilitiesDeviceId = deviceId
      capabilities.value = response
    }
    return response
  }

  async function isCodecSupported(codec: string): Promise<boolean> {
    const VideoDecoderCtor = (window as unknown as WebTransportWindow).VideoDecoder
    if (!VideoDecoderCtor?.isConfigSupported) return !!webcodecsCodecStrings[codec]
    const config = { codec: webcodecsCodecStrings[codec] ?? codec }
    const result = await VideoDecoderCtor.isConfigSupported(config).catch(() => ({ supported: false }))
    return !!result.supported
  }

  function configureDecoder(codec: string, generation: number) {
    const VideoDecoderCtor = (window as unknown as WebTransportWindow).VideoDecoder
    if (!VideoDecoderCtor) throw new Error('WebCodecs VideoDecoder is unavailable')
    closeDecoder()
    const nextDecoder = new VideoDecoderCtor({
      output: (frame: WebCodecsFrame) => {
        if (generation !== connectionGeneration) {
          frame.close()
          return
        }
        const canvas = canvasRef.value
        if (!canvas) {
          frame.close()
          return
        }
        if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
          canvas.width = frame.displayWidth
          canvas.height = frame.displayHeight
        }
        const ctx = canvas.getContext('2d')
        if (ctx) ctx.drawImage(frame, 0, 0, canvas.width, canvas.height)
        stats.value.framesDecoded += 1
        stats.value.frameWidth = frame.displayWidth
        stats.value.frameHeight = frame.displayHeight
        stats.value.decoderQueueSize = Number(decoder?.decodeQueueSize ?? 0)
        updateDerivedStats()
        frame.close()
      },
      error: (e: Error) => {
        if (generation === connectionGeneration) {
          const message = e?.message || 'WebCodecs decoding error'
          error.value = message
          state.value = 'failed'
          abortCurrentConnection(generation, message)
        }
      },
    })
    const config: Record<string, unknown> = {
      codec: webcodecsCodecStrings[codec] ?? codec,
      optimizeForLatency: true,
    }
    if (codec === 'h264') config.avc = { format: 'annexb' }
    decoder = nextDecoder
    nextDecoder.configure(config)
  }

  function closeDecoder() {
    const activeDecoder = decoder
    decoder = null
    if (!activeDecoder || activeDecoder.state === 'closed') return
    try {
      activeDecoder.close()
    } catch (e: unknown) {
      if (e instanceof DOMException && e.name === 'InvalidStateError') return
      if (errorMessage(e).includes('closed codec')) return
      throw e
    }
  }

  function closeTransport() {
    const activeTransport = transport
    transport = null
    try {
      activeTransport?.close()
    } catch {
      // WebTransport may already be closed by the browser while error cleanup races.
    }
  }

  function abortCurrentConnection(generation: number, message: string) {
    if (generation !== connectionGeneration) return
    error.value = message
    state.value = 'failed'
    abortController?.abort()
    abortController = null
    closeTransport()
    closeDecoder()
  }

  function webTransportOptions(hashes?: Array<{ algorithm: 'sha-256'; value: number[] }> | null) {
    if (!hashes?.length) return undefined
    return {
      serverCertificateHashes: hashes.map(hash => ({
        algorithm: hash.algorithm,
        value: new Uint8Array(hash.value).buffer,
      })),
    }
  }

  async function connect(codec: string, encoder?: string | null) {
    const deviceId = currentBmcIdFromLocation()
    const api = createWebTransportApi(captureDeviceRequest().request)
    const generation = connectionGeneration + 1
    await disconnect()
    assertActiveGeneration(generation, deviceId)
    error.value = ''
    state.value = 'connecting'
    try {
      const capability = capabilitiesDeviceId === deviceId && capabilities.value
        ? capabilities.value
        : await refreshCapabilities()
      assertActiveGeneration(generation, deviceId)
      if (!browserSupported.value) {
        throw new Error('webtransport_webcodecs_browser_unsupported')
      }
      if (!capability.enabled || !capability.url) {
        throw new Error(capability.reason || 'webtransport_unavailable')
      }
      if (!await isCodecSupported(codec)) {
        throw new Error('webcodecs_codec_unsupported')
      }
      assertActiveGeneration(generation, deviceId)

      connectionId.value = generateUUID()
      const response = await api.connectWebTransport({ connection_id: connectionId.value, codec, encoder })
      assertActiveGeneration(generation, deviceId)
      if (response.error || response.success === false) {
        throw new Error(response.error || 'webtransport_connect_failed')
      }

      configureDecoder(codec, generation)
      resetStats(codec)
      const WebTransportCtor = (window as unknown as WebTransportWindow).WebTransport
      if (!WebTransportCtor) throw new Error('WebTransport is unavailable')
      const options = webTransportOptions(response.server_certificate_hashes ?? capability.server_certificate_hashes)
      const nextTransport = (options
        ? new WebTransportCtor(response.url || capability.url, options)
        : new WebTransportCtor(response.url || capability.url)) as InstanceType<WebTransportCtor>
      assertActiveGeneration(generation, deviceId)
      transport = nextTransport
      // Observe closure before readiness: either promise may reject during setup.
      void nextTransport.closed.then(() => {
        if (!isActiveTransport(nextTransport, generation)) return
        abortController?.abort()
        abortController = null
        closeTransport()
        closeDecoder()
        state.value = 'closed'
      }, (reason: unknown) => {
        if (isActiveTransport(nextTransport, generation)) {
          abortCurrentConnection(generation, errorMessage(reason))
        }
      })
      await withTimeout(nextTransport.ready, WEBTRANSPORT_READY_TIMEOUT_MS, 'webtransport_ready_timeout')
      if (!isActiveTransport(nextTransport, generation)) {
        throw new Error('webtransport_connection_superseded')
      }
      state.value = 'connected'
      abortController = new AbortController()
      void readIncomingStreams(nextTransport, abortController.signal, generation)
      return true
    } catch (e: unknown) {
      if (generation === connectionGeneration) {
        abortCurrentConnection(generation, errorMessage(e))
      }
      throw e
    }
  }

  async function disconnect() {
    connectionGeneration += 1
    abortController?.abort()
    abortController = null
    closeTransport()
    closeDecoder()
    resetStats()
    connectionId.value = ''
    error.value = ''
    state.value = 'closed'
  }

  async function readIncomingStreams(activeTransport: InstanceType<WebTransportCtor>, signal: AbortSignal, generation: number) {
    const reader = activeTransport.incomingUnidirectionalStreams.getReader()
    try {
      while (!signal.aborted) {
        if (!isActiveTransport(activeTransport, generation)) break
        const { value, done } = await reader.read()
        if (done || !value) break
        void readFrameStream(value, signal, activeTransport, generation)
      }
    } catch (e: unknown) {
      if (!signal.aborted && isActiveTransport(activeTransport, generation)) {
        abortCurrentConnection(generation, errorMessage(e))
      }
    } finally {
      reader.releaseLock()
    }
  }

  async function readFrameStream(stream: ReadableStream<Uint8Array>, signal: AbortSignal, activeTransport: InstanceType<WebTransportCtor>, generation: number) {
    const reader = stream.getReader()
    let pending = new Uint8Array(0) as Uint8Array<ArrayBufferLike>
    try {
      while (!signal.aborted) {
        if (!isActiveTransport(activeTransport, generation)) break
        const { value, done } = await reader.read()
        if (done) break
        if (!isActiveTransport(activeTransport, generation)) break
        if (!value?.length) continue
        stats.value.bytesReceived += value.byteLength
        pending = appendBytes(pending, new Uint8Array(value))
        pending = drainMessages(pending, generation)
        stats.value.packetsBuffered = pending.byteLength
        updateDerivedStats()
      }
    } catch (e: unknown) {
      if (!signal.aborted && isActiveTransport(activeTransport, generation)) {
        abortCurrentConnection(generation, errorMessage(e))
      }
    } finally {
      reader.releaseLock()
    }
  }

  function drainMessages(input: Uint8Array<ArrayBufferLike>, generation: number): Uint8Array<ArrayBufferLike> {
    if (generation !== connectionGeneration) return new Uint8Array(0)
    let offset = 0
    while (input.byteLength - offset >= 8) {
      if (generation !== connectionGeneration) return new Uint8Array(0)
      const view = new DataView(input.buffer, input.byteOffset + offset, input.byteLength - offset)
      const metadataLength = view.getUint32(0, false)
      const payloadLength = view.getUint32(4, false)
      if (input.byteLength - offset < 8 + metadataLength + payloadLength) break
      const metadataStart = offset + 8
      const metadataEnd = metadataStart + metadataLength
      let metadata: FrameMetadata
      try {
        metadata = JSON.parse(new TextDecoder().decode(input.slice(metadataStart, metadataEnd))) as FrameMetadata
      } catch (e: unknown) {
        abortCurrentConnection(generation, `WebTransport metadata decode failed: ${errorMessage(e)}`)
        return new Uint8Array(0)
      }
      const nextOffset = metadataEnd + payloadLength
      const payload = input.slice(metadataEnd, nextOffset)
      try {
        decodeFrame(metadata, payload, generation)
      } catch (e: unknown) {
        abortCurrentConnection(generation, errorMessage(e))
        return new Uint8Array(0)
      }
      offset = nextOffset
    }
    return input.slice(offset)
  }

  function decodeFrame(metadata: FrameMetadata, payload: Uint8Array, generation: number) {
    if (generation !== connectionGeneration || !decoder || payload.byteLength === 0) return
    stats.value.packetsReceived += 1
    if (metadata.keyframe) stats.value.keyframesReceived += 1
    stats.value.codec = metadata.codec || stats.value.codec
    stats.value.frameWidth = metadata.width || stats.value.frameWidth
    stats.value.frameHeight = metadata.height || stats.value.frameHeight
    stats.value.pixelFormat = metadata.pixel_format || stats.value.pixelFormat
    stats.value.lastTraceId = metadata.trace_id ?? stats.value.lastTraceId
    stats.value.lastFrameTimestampUs = metadata.timestamp_us || stats.value.lastFrameTimestampUs
    stats.value.lastFrameDurationUs = metadata.duration_us || stats.value.lastFrameDurationUs
    lastFrameReceivedAt = performance.now()
    stats.value.lastFrameAgeMs = 0
    stats.value.decoderQueueSize = Number(decoder.decodeQueueSize ?? 0)
    const ChunkCtor = (window as unknown as WebTransportWindow).EncodedVideoChunk
    if (!ChunkCtor) throw new Error('WebCodecs EncodedVideoChunk is unavailable')
    const chunk = new ChunkCtor({
      type: metadata.keyframe ? 'key' : 'delta',
      timestamp: metadata.timestamp_us,
      duration: metadata.duration_us,
      data: payload,
    })
    try {
      decoder.decode(chunk)
    } catch (e: unknown) {
      abortCurrentConnection(generation, errorMessage(e))
    }
  }

  function appendBytes(a: Uint8Array<ArrayBufferLike>, b: Uint8Array<ArrayBufferLike>): Uint8Array<ArrayBufferLike> {
    if (a.byteLength === 0) return b
    const out = new Uint8Array(a.byteLength + b.byteLength)
    out.set(a, 0)
    out.set(b, a.byteLength)
    return out
  }

  function setCanvas(el: HTMLCanvasElement | null) {
    canvasRef.value = el
  }

  return {
    state,
    error,
    connectionId,
    capabilities,
    stats,
    browserSupported,
    isConnecting,
    isConnected,
    refreshCapabilities,
    isCodecSupported,
    connect,
    disconnect,
    setCanvas,
  }
}
