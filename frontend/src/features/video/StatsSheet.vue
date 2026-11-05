<script setup lang="ts">
import { ref, watch, onMounted, onUnmounted, nextTick, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { ScrollArea } from '@/components/ui/scroll-area'
import { streamApi, type VideoTraceSnapshot } from '@/api'
import type { WebRTCStats } from '@/composables/useWebRTC'
import type { WebTransportStats } from '@/composables/useWebTransport'
import { formatFpsValue } from '@/lib/fps'

const { t } = useI18n()

const props = defineProps<{
  open: boolean
  videoMode: string
  httpModes: string[]
  mjpegFps?: number
  wsLatency?: number
  webrtcStats?: WebRTCStats
  webtransportStats?: WebTransportStats
  backendPerf?: Record<string, number>
  activeTransport: 'http' | 'webrtc' | 'webtransport'
  webrtcConnectionId?: string | null
  webtransportConnectionId?: string | null
}>()

const emit = defineEmits<{
  (e: 'update:open', value: boolean): void
}>()

const stabilityChartRef = ref<HTMLDivElement | null>(null)
const delayChartRef = ref<HTMLDivElement | null>(null)
const packetLossChartRef = ref<HTMLDivElement | null>(null)
const fpsChartRef = ref<HTMLDivElement | null>(null)

let stabilityChart: uPlot | null = null
let delayChart: uPlot | null = null
let packetLossChart: uPlot | null = null
let fpsChart: uPlot | null = null

const isHttp = computed(() => props.activeTransport === 'http' || props.httpModes.includes(props.videoMode))
const isWebRTC = computed(() => props.activeTransport === 'webrtc' && !isHttp.value)
const isWebTransport = computed(() => props.activeTransport === 'webtransport' && !isHttp.value)
const transportLabel = computed(() => {
  if (isWebTransport.value) return 'WebTransport'
  if (isWebRTC.value) return 'WebRTC'
  return 'MJPEG'
})
const hasPacketLossStats = computed(() => isWebRTC.value)
const hasJitterStats = computed(() => isWebRTC.value)
const hasNetworkDelayStats = computed(() => isWebRTC.value || props.wsLatency !== undefined)

function activeEncodeAvgMsValue(): number {
  if (isWebTransport.value) {
    return webTransportTraceNum('encoder.encode_time_avg_ms')
  }
  if (isWebRTC.value) {
    return gstNum('encoder.webrtc_encode_time_avg_ms')
  }
  return mjpegEncodeAvgMsValue()
}

function activeEncodeMaxMsValue(): number {
  if (isWebTransport.value) {
    return webTransportTraceNum('encoder.encode_time_max_ms')
  }
  if (isWebRTC.value) {
    return gstNum('encoder.webrtc_encode_time_max_ms')
  }
  return mjpegEncodeMaxMsValue()
}

function mjpegEncodeCountValue(): number {
  return mjpegTraceEncodeCountValue() || mjpegBackendEncodeCountValue()
}

function mjpegTraceEncodeCountValue(): number {
  return traceNum('mjpeg_encoder.encode_count')
}

function mjpegBackendEncodeCountValue(): number {
  return perfVal('encode_count')
}

function mjpegEncodeAvgMsValue(): number {
  return mjpegTraceEncodeCountValue() > 0
    ? traceNum('mjpeg_encoder.encode_time_avg_ms')
    : perfVal('encode_time_avg_ms')
}

function mjpegEncodeMaxMsValue(): number {
  return mjpegTraceEncodeCountValue() > 0
    ? traceNum('mjpeg_encoder.encode_time_max_ms')
    : perfVal('encode_time_max_ms')
}

function activeEncodeCountValue(): number {
  if (isWebTransport.value) {
    return webTransportTraceNum('encoder.encode_count')
  }
  if (isWebRTC.value) {
    return gstNum('encoder.webrtc_encode_count')
  }
  return mjpegEncodeCountValue()
}

const backendLatencyMs = computed(() => {
  const pipe = props.backendPerf?.pipeline_latency_avg_ms ?? 0
  const enc = activeEncodeAvgMsValue()
  if (pipe <= 0 && enc <= 0) return 0
  return pipe + enc
})

const networkLatencyMs = computed<number | undefined>(() => {
  if (isWebRTC.value) {
    if (props.webrtcStats?.roundTripTime === undefined || props.webrtcStats?.roundTripTime === null) return undefined
    if (props.webrtcStats.roundTripTime <= 0) return undefined
    return props.webrtcStats.roundTripTime * 1000
  }
  if (isWebTransport.value) return undefined
  if (props.wsLatency === undefined || props.wsLatency === null) return undefined
  if (props.wsLatency <= 0) return undefined
  return props.wsLatency
})

const totalLatencyMs = computed(() => {
  const pipe = props.backendPerf?.pipeline_latency_avg_ms ?? 0
  const snap = props.backendPerf?.fb_snapshot_avg_ms ?? 0
  const enc = activeEncodeAvgMsValue()
  const net = networkLatencyMs.value ?? 0
  return pipe + snap + enc + net
})

const MAX_POINTS = 120
const timestamps = ref<number[]>([])
const jitterHistory = ref<number[]>([])
const delayHistory = ref<number[]>([])
const packetLossHistory = ref<number[]>([])
const fpsHistory = ref<number[]>([])
const bitrateHistory = ref<number[]>([])
const traceSnapshot = ref<VideoTraceSnapshot | null>(null)
const traceError = ref<string | null>(null)

let lastPacketsLost = 0
let lastBytesReceived = 0
let lastTimestamp = 0

function formatTime(ts: number): string {
  const date = new Date(ts * 1000)
  return date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
}

const chartColors = {
  line: '#3b82f6',
  fill: 'rgba(59, 130, 246, 0.1)',
  grid: 'rgba(148, 163, 184, 0.1)',
  axis: '#64748b',
  text: '#94a3b8',
}

function createChartOptions(
  container: HTMLElement,
  _yLabel: string,
  yFormatter: (v: number) => string
): uPlot.Options {
  const width = container.clientWidth || 300

  return {
    width,
    height: 100,
    cursor: {
      show: true,
      x: true,
      y: false,
      drag: { x: false, y: false },
    },
    legend: { show: false },
    scales: {
      x: { time: false },
      y: { auto: true, range: (_u, min, max) => [Math.max(0, min - 1), max + 1] },
    },
    axes: [
      {
        show: true,
        stroke: chartColors.axis,
        grid: { show: false },
        ticks: { show: false },
        gap: 4,
        size: 20,
        values: (_, splits) => splits.map(v => formatTime(v)),
        font: '10px system-ui',
      },
      {
        show: true,
        side: 1, // Right side
        stroke: chartColors.axis,
        size: 55,
        gap: 8,
        grid: { stroke: chartColors.grid, width: 1 },
        values: (_, splits) => splits.map(v => yFormatter(v)),
        font: '10px system-ui',
      },
    ],
    series: [
      {},
      {
        stroke: chartColors.line,
        width: 1.5,
        fill: chartColors.fill,
        paths: uPlot.paths.spline?.() || undefined,
      },
    ],
  }
}

const activeTooltip = ref<{
  chartId: string
  time: string
  value: string
  unit: string
  left: number
  top: number
  visible: boolean
}>({
  chartId: '',
  time: '',
  value: '',
  unit: '',
  left: 0,
  top: 0,
  visible: false,
})

function createTooltipPlugin(chartId: string, unit: string): uPlot.Plugin {
  return {
    hooks: {
      setCursor: [
        (u) => {
          const idx = u.cursor.idx
          if (idx !== null && idx !== undefined && u.cursor.left !== undefined && u.cursor.top !== undefined) {
            const ts = u.data[0]?.[idx]
            const val = u.data[1]?.[idx]
            if (ts !== undefined && ts !== null && val !== undefined && val !== null) {
              const date = new Date(ts * 1000)
              activeTooltip.value = {
                chartId,
                time: date.toLocaleTimeString('zh-CN'),
                value: val.toFixed(1),
                unit,
                left: u.cursor.left,
                top: u.cursor.top,
                visible: true,
              }
            }
          }
        },
      ],
      ready: [
        (u) => {
          const over = u.over
          over.addEventListener('mouseleave', () => {
            if (activeTooltip.value.chartId === chartId) {
              activeTooltip.value.visible = false
            }
          })
        },
      ],
    },
  }
}

function initCharts() {
  if (!props.open) return

  nextTick(() => {
    if (timestamps.value.length === 0) {
      const now = Date.now() / 1000
      for (let i = MAX_POINTS - 1; i >= 0; i--) {
        timestamps.value.push(now - i)
      }
      jitterHistory.value = new Array(MAX_POINTS).fill(0)
      delayHistory.value = new Array(MAX_POINTS).fill(0)
      packetLossHistory.value = new Array(MAX_POINTS).fill(0)
      fpsHistory.value = new Array(MAX_POINTS).fill(0)
      bitrateHistory.value = new Array(MAX_POINTS).fill(0)
    }

    if (stabilityChartRef.value && !stabilityChart) {
      const opts = createChartOptions(stabilityChartRef.value, 'ms', (v) => `${v.toFixed(0)} ms`)
      opts.plugins = [createTooltipPlugin('stability', 'ms')]
      stabilityChart = new uPlot(
        opts,
        [timestamps.value, jitterHistory.value],
        stabilityChartRef.value
      )
    }

    if (delayChartRef.value && !delayChart) {
      const opts = createChartOptions(delayChartRef.value, 'ms', (v) => `${v.toFixed(0)} ms`)
      opts.plugins = [createTooltipPlugin('delay', 'ms')]
      delayChart = new uPlot(
        opts,
        [timestamps.value, delayHistory.value],
        delayChartRef.value
      )
    }

    // Packet Loss Chart
    if (packetLossChartRef.value && !packetLossChart) {
      const opts = createChartOptions(packetLossChartRef.value, '', (v) => `${v.toFixed(0)} 个`)
      opts.plugins = [createTooltipPlugin('packetLoss', '个')]
      packetLossChart = new uPlot(
        opts,
        [timestamps.value, packetLossHistory.value],
        packetLossChartRef.value
      )
    }

    if (fpsChartRef.value && !fpsChart) {
      const opts = createChartOptions(fpsChartRef.value, 'fps', (v) => `${v.toFixed(0)} fps`)
      opts.plugins = [createTooltipPlugin('fps', 'fps')]
      fpsChart = new uPlot(
        opts,
        [timestamps.value, fpsHistory.value],
        fpsChartRef.value
      )
    }
  })
}

function destroyCharts() {
  stabilityChart?.destroy()
  stabilityChart = null
  delayChart?.destroy()
  delayChart = null
  packetLossChart?.destroy()
  packetLossChart = null
  fpsChart?.destroy()
  fpsChart = null
}

function addDataPoint() {
  const now = Date.now() / 1000

  timestamps.value.push(now)
  if (timestamps.value.length > MAX_POINTS) {
    timestamps.value.shift()
  }

  if (isWebRTC.value && props.webrtcStats) {
    const jitter = (props.webrtcStats.jitter || 0) * 1000
    jitterHistory.value.push(jitter)

    const rtt = (props.webrtcStats.roundTripTime || 0) * 1000
    delayHistory.value.push(rtt)

    const currentLost = props.webrtcStats.packetsLost || 0
    const lostDelta = lastPacketsLost > 0 ? Math.max(0, currentLost - lastPacketsLost) : 0
    lastPacketsLost = currentLost
    packetLossHistory.value.push(lostDelta)

    fpsHistory.value.push(props.webrtcStats.framesPerSecond || 0)

    const currentBytes = props.webrtcStats.bytesReceived || 0
    const currentTime = Date.now()
    if (lastTimestamp > 0 && currentBytes > lastBytesReceived) {
      const timeDiff = (currentTime - lastTimestamp) / 1000
      const bytesDiff = currentBytes - lastBytesReceived
      const bitrate = (bytesDiff * 8) / (timeDiff * 1000000)
      bitrateHistory.value.push(Math.round(bitrate * 100) / 100)
    } else {
      bitrateHistory.value.push(bitrateHistory.value[bitrateHistory.value.length - 1] || 0)
    }
    lastBytesReceived = currentBytes
    lastTimestamp = currentTime
  } else if (isWebTransport.value && props.webtransportStats) {
    jitterHistory.value.push(0)
    delayHistory.value.push(0)
    packetLossHistory.value.push(0)
    fpsHistory.value.push(props.webtransportStats.framesPerSecond || 0)
    bitrateHistory.value.push(props.webtransportStats.bitrateMbps || webTransportTraceNum('encoder.bitrate_mbps'))
  } else {
    jitterHistory.value.push(0)
    delayHistory.value.push(props.wsLatency ?? 0)
    packetLossHistory.value.push(0)
    fpsHistory.value.push(props.mjpegFps || 0)
    bitrateHistory.value.push(0)
  }

  if (jitterHistory.value.length > MAX_POINTS) jitterHistory.value.shift()
  if (delayHistory.value.length > MAX_POINTS) delayHistory.value.shift()
  if (packetLossHistory.value.length > MAX_POINTS) packetLossHistory.value.shift()
  if (fpsHistory.value.length > MAX_POINTS) fpsHistory.value.shift()
  if (bitrateHistory.value.length > MAX_POINTS) bitrateHistory.value.shift()

  updateCharts()
}

function updateCharts() {
  stabilityChart?.setData([timestamps.value, jitterHistory.value])
  delayChart?.setData([timestamps.value, delayHistory.value])
  packetLossChart?.setData([timestamps.value, packetLossHistory.value])
  fpsChart?.setData([timestamps.value, fpsHistory.value])
}

let dataInterval: number | null = null
let traceInterval: number | null = null
let chartInitTimeout: number | null = null

function cancelChartInit() {
  if (chartInitTimeout !== null) {
    window.clearTimeout(chartInitTimeout)
    chartInitTimeout = null
  }
}

function scheduleChartInit(delayMs: number) {
  cancelChartInit()
  chartInitTimeout = window.setTimeout(() => {
    chartInitTimeout = null
    if (!props.open) return
    initCharts()
    startDataCollection()
  }, delayMs)
}

function startDataCollection() {
  if (dataInterval) return
  dataInterval = window.setInterval(addDataPoint, 1000)
  startTraceCollection()
}

function stopDataCollection() {
  if (dataInterval) {
    clearInterval(dataInterval)
    dataInterval = null
  }
  stopTraceCollection()
}

function formatCandidateType(type: string): string {
  const typeMap: Record<string, string> = {
    host: 'Host (Local)',
    srflx: 'STUN (NAT)',
    prflx: 'Peer Reflexive',
    relay: 'TURN Relay',
    unknown: '-',
  }
  return typeMap[type] || type
}

function perfVal(key: string): number {
  const traceBackend = traceSnapshot.value?.backend
  const fromTrace = typeof traceBackend?.[key] === 'number' ? traceBackend[key] : undefined
  return fromTrace ?? props.backendPerf?.[key] ?? 0
}

function nestedValue(root: unknown, path: string): unknown {
  return path.split('.').reduce<unknown>((value, key) => {
    if (!value || typeof value !== 'object') return undefined
    return (value as Record<string, unknown>)[key]
  }, root)
}

function traceNum(path: string, fallback = 0): number {
  const value = nestedValue(traceSnapshot.value, path)
  return typeof value === 'number' ? value : fallback
}

function traceStr(path: string, fallback = '-'): string {
  const value = nestedValue(traceSnapshot.value, path)
  return typeof value === 'string' && value.length > 0 ? value : fallback
}

function findTraceSession<T extends { connection_id: string }>(
  sessions: T[] | undefined,
  connectionId?: string | null,
): T | null {
  if (!sessions?.length) return null
  if (!connectionId) return sessions[0] ?? null
  return sessions.find(session => session.connection_id === connectionId) ?? null
}

const activeWebRtcTraceSession = computed(() => (
  findTraceSession(traceSnapshot.value?.webrtc_sessions, props.webrtcConnectionId)
))

const activeGstSession = computed(() => activeWebRtcTraceSession.value?.gstreamer ?? null)

function gstNum(path: string, fallback = 0): number {
  const value = nestedValue(activeGstSession.value, path)
  return typeof value === 'number' ? value : fallback
}

function gstStr(path: string, fallback = '-'): string {
  const value = nestedValue(activeGstSession.value, path)
  return typeof value === 'string' && value.length > 0 ? value : fallback
}

function fmtMs(val: number): string {
  if (val <= 0) return '-'
  return val < 1 ? `${(val * 1000).toFixed(0)} µs` : `${val.toFixed(1)} ms`
}

function fmtEncodeMs(val: number, samples: number): string {
  if (samples <= 0) return '-'
  return fmtMs(Math.max(val, 0.001))
}

function activeEncodeAvgMsText(): string {
  return fmtEncodeMs(activeEncodeAvgMsValue(), activeEncodeCountValue())
}

function activeEncodeMaxMsText(): string {
  return fmtEncodeMs(activeEncodeMaxMsValue(), activeEncodeCountValue())
}

function fmtNet(val: number | undefined): string {
  if (val === undefined) return '-'
  if (val <= 0) return '< 1 ms'
  return val < 1 ? `${(val * 1000).toFixed(0)} µs` : `${val.toFixed(1)} ms`
}

function fmtMbps(val: number | undefined): string {
  if (val === undefined || val <= 0) return '-'
  return `${val.toFixed(2)} Mbps`
}

function fmtCount(val: number | undefined): string {
  if (val === undefined || Number.isNaN(val)) return '-'
  return val.toLocaleString()
}

function fmtVal(val: number): string {
  if (val <= 0) return '-'
  return val.toLocaleString()
}

function encodeSampleStatus(samples: number): string {
  if (samples > 0) return `${fmtCount(samples)} 样本`
  if (!traceSnapshot.value) return '等待 trace'
  if (isWebRTC.value && !props.webrtcConnectionId) return '等待 connection id'
  if (isWebTransport.value && !props.webtransportConnectionId) return '等待 connection id'
  if (isWebRTC.value && !activeWebRtcTraceSession.value) return 'trace 连接未命中'
  if (isWebTransport.value && !activeWebTransportTraceSession.value) return 'trace 连接未命中'
  return '等待编码样本'
}

async function refreshTraceSnapshot() {
  try {
    const connectionId = isWebRTC.value
      ? props.webrtcConnectionId
      : isWebTransport.value
        ? props.webtransportConnectionId
        : null
    const response = await streamApi.trace(connectionId)
    traceSnapshot.value = response.trace
    traceError.value = null
  } catch (err) {
    traceError.value = err instanceof Error ? err.message : 'trace unavailable'
  }
}

function startTraceCollection() {
  if (traceInterval) return
  void refreshTraceSnapshot()
  traceInterval = window.setInterval(() => {
    void refreshTraceSnapshot()
  }, 1000)
}

function stopTraceCollection() {
  if (traceInterval) {
    clearInterval(traceInterval)
    traceInterval = null
  }
}

function resetDataSeries() {
  timestamps.value = []
  jitterHistory.value = []
  delayHistory.value = []
  packetLossHistory.value = []
  fpsHistory.value = []
  bitrateHistory.value = []
  lastBytesReceived = 0
  lastPacketsLost = 0
  lastTimestamp = 0
}

const activeWebTransportTraceSession = computed(() => {
  return findTraceSession(traceSnapshot.value?.webtransport_sessions, props.webtransportConnectionId)
})

function webTransportTraceNum(path: string, fallback = 0): number {
  const value = nestedValue(activeWebTransportTraceSession.value, path)
  return typeof value === 'number' ? value : fallback
}

function webTransportTraceStr(path: string, fallback = '-'): string {
  const value = nestedValue(activeWebTransportTraceSession.value, path)
  return typeof value === 'string' && value.length > 0 ? value : fallback
}

interface CurrentStats {
  jitter?: number
  delay?: number
  fps: number
  resolution: string
  bitrateMbps?: number
  packetsLost?: number
  packetsReceived?: number
  framesDecoded?: number
  framesDropped?: number
  keyFramesDecoded?: number
  codec: string
  decoder: string
  decodeTime?: number
  jitterBufferDelay?: number
  freezes?: number
  pli?: number
  fir?: number
  nack?: number
  isRelay: boolean
  transport: string
  localType: string
  remoteType: string
  backendLatency: number
  pixelFormat?: string
  lastTraceId?: number
  lastFrameAgeMs?: number
  decoderQueueSize?: number
  packetsBuffered?: number
}

const currentStats = computed<CurrentStats>(() => {
  const s = props.webrtcStats
  if (isWebRTC.value && s) {
    const lastBitrate = bitrateHistory.value[bitrateHistory.value.length - 1]
    const bitrate = lastBitrate && lastBitrate > 0 ? lastBitrate : undefined
    return {
      jitter: s.jitter > 0 ? Math.round(s.jitter * 1000 * 10) / 10 : undefined,
      delay: s.roundTripTime > 0 ? Math.round(s.roundTripTime * 1000) : undefined,
      fps: s.framesPerSecond || 0,
      resolution: s.frameWidth && s.frameHeight
        ? `${s.frameWidth}x${s.frameHeight}`
        : '-',
      bitrateMbps: bitrate,
      packetsLost: s.packetsLost || 0,
      packetsReceived: s.packetsReceived || 0,
      framesDecoded: s.framesDecoded || 0,
      framesDropped: s.framesDropped || 0,
      keyFramesDecoded: s.keyFramesDecoded || 0,
      codec: s.mimeType || '-',
      decoder: s.decoderImplementation || '-',
      decodeTime: s.framesDecoded > 0 && s.totalDecodeTime > 0 ? (s.totalDecodeTime / s.framesDecoded) * 1000 : undefined,
      jitterBufferDelay: s.jitterBufferEmittedCount > 0 && s.jitterBufferDelay > 0 ? (s.jitterBufferDelay / s.jitterBufferEmittedCount) * 1000 : undefined,
      freezes: s.freezeCount || 0,
      pli: s.pliCount || 0,
      fir: s.firCount || 0,
      nack: s.nackCount || 0,
      isRelay: s.isRelay || false,
      transport: (s.transportProtocol || '-').toUpperCase(),
      localType: formatCandidateType(s.localCandidateType || 'unknown'),
      remoteType: formatCandidateType(s.remoteCandidateType || 'unknown'),
      backendLatency: backendLatencyMs.value,
    }
  }
  const wt = props.webtransportStats
  if (isWebTransport.value && wt) {
    return {
      jitter: undefined,
      delay: undefined,
      fps: wt.framesPerSecond || 0,
      resolution: wt.frameWidth && wt.frameHeight
        ? `${wt.frameWidth}x${wt.frameHeight}`
        : traceNum('processor.width') && traceNum('processor.height')
          ? `${traceNum('processor.width')}x${traceNum('processor.height')}`
          : '-',
      bitrateMbps: wt.bitrateMbps > 0
        ? wt.bitrateMbps
        : webTransportTraceNum('encoder.bitrate_mbps') > 0
          ? webTransportTraceNum('encoder.bitrate_mbps')
          : undefined,
      packetsLost: undefined,
      packetsReceived: wt.packetsReceived || 0,
      framesDecoded: wt.framesDecoded || 0,
      framesDropped: wt.framesDropped || 0,
      keyFramesDecoded: wt.keyframesReceived || 0,
      codec: wt.codec || webTransportTraceStr('codec'),
      decoder: 'WebCodecs',
      decodeTime: undefined,
      jitterBufferDelay: undefined,
      freezes: undefined,
      pli: undefined,
      fir: undefined,
      nack: undefined,
      isRelay: false,
      transport: 'QUIC',
      localType: '-',
      remoteType: '-',
      backendLatency: backendLatencyMs.value,
      pixelFormat: wt.pixelFormat || webTransportTraceStr('expected_caps.pixel_format'),
      lastTraceId: wt.lastTraceId || webTransportTraceNum('frames.last_trace_id'),
      lastFrameAgeMs: wt.lastFrameAgeMs,
      decoderQueueSize: wt.decoderQueueSize,
      packetsBuffered: wt.packetsBuffered,
    }
  }
  return {
    jitter: undefined,
    delay: props.wsLatency,
    fps: props.mjpegFps || 0,
    resolution: traceNum('processor.width') && traceNum('processor.height')
      ? `${traceNum('processor.width')}x${traceNum('processor.height')}`
      : '-',
    bitrateMbps: undefined,
    packetsLost: undefined,
    packetsReceived: undefined,
    framesDecoded: undefined,
    framesDropped: undefined,
    keyFramesDecoded: undefined,
    codec: 'MJPEG',
    decoder: 'HTMLImageElement',
    decodeTime: undefined,
    jitterBufferDelay: undefined,
    freezes: undefined,
    pli: undefined,
    fir: undefined,
    nack: undefined,
    isRelay: false,
    transport: 'HTTP',
    localType: '-',
    remoteType: '-',
    backendLatency: backendLatencyMs.value,
  }
})

watch(() => props.open, (isOpen) => {
  if (isOpen) {
    resetDataSeries()
    scheduleChartInit(150)
  } else {
    cancelChartInit()
    stopDataCollection()
    destroyCharts()
  }
})

watch(() => [
  props.activeTransport,
  props.videoMode,
  props.webrtcConnectionId,
  props.webtransportConnectionId,
], () => {
  if (!props.open) return
  resetDataSeries()
  cancelChartInit()
  destroyCharts()
  scheduleChartInit(50)
})

function handleResize() {
  if (!props.open) return
  cancelChartInit()
  destroyCharts()
  scheduleChartInit(50)
}

onMounted(() => {
  window.addEventListener('resize', handleResize)
  if (props.open) {
    initCharts()
    startDataCollection()
  }
})

onUnmounted(() => {
  window.removeEventListener('resize', handleResize)
  cancelChartInit()
  stopDataCollection()
  destroyCharts()
})
</script>

<template>
  <Sheet :open="props.open" @update:open="emit('update:open', $event)">
    <SheetContent
      side="right"
      class="w-[90vw] max-w-[440px] p-0 border-l border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-950"
    >
      <!-- Header -->
      <SheetHeader class="px-6 py-3 border-b border-slate-200 dark:border-slate-800">
        <div class="flex items-center gap-2">
          <SheetTitle class="text-base">{{ t('stats.title') }}</SheetTitle>
          <span class="text-xs px-2 py-0.5 rounded bg-slate-100 dark:bg-slate-800 text-muted-foreground">
            {{ transportLabel }}
          </span>
        </div>
      </SheetHeader>

      <ScrollArea class="h-[calc(100dvh-60px)]">
        <div class="px-6 py-4 space-y-6">
          <!-- Video Section Header -->
          <div>
            <h3 class="text-sm font-medium">{{ t('stats.video') }}</h3>
            <p class="text-xs text-muted-foreground mt-0.5">
              {{ t('stats.videoDesc') }}
            </p>
          </div>

          <!-- Network Stability (Jitter) -->
          <div class="space-y-2" v-if="hasJitterStats">
            <div class="flex items-center justify-between">
              <h4 class="text-sm font-medium">{{ t('stats.stability') }}</h4>
            </div>
            <p class="text-xs text-muted-foreground">
              {{ t('stats.stabilityDesc') }}
            </p>
            <div class="relative">
              <div
                ref="stabilityChartRef"
                class="w-full rounded-lg bg-slate-50 dark:bg-slate-900/50 p-2"
              />
              <div
                v-if="activeTooltip.visible && activeTooltip.chartId === 'stability'"
                class="chart-tooltip"
                :style="{ left: `${activeTooltip.left + 60}px`, top: `${activeTooltip.top - 40}px` }"
              >
                <div class="text-xs font-medium">{{ activeTooltip.time }}</div>
                <div class="text-xs text-blue-500">{{ activeTooltip.value }} {{ activeTooltip.unit }}</div>
              </div>
            </div>
          </div>

          <!-- Playback Delay -->
          <div class="space-y-2" v-if="hasNetworkDelayStats">
            <div class="flex items-center justify-between">
              <h4 class="text-sm font-medium">{{ t('stats.delay') }}</h4>
              <span class="text-xs text-muted-foreground">
                {{ fmtNet(currentStats.delay) }}
              </span>
            </div>
            <p class="text-xs text-muted-foreground">
              {{ t('stats.delayDesc') }}
            </p>
            <div class="relative">
              <div
                ref="delayChartRef"
                class="w-full rounded-lg bg-slate-50 dark:bg-slate-900/50 p-2"
              />
              <div
                v-if="activeTooltip.visible && activeTooltip.chartId === 'delay'"
                class="chart-tooltip"
                :style="{ left: `${activeTooltip.left + 60}px`, top: `${activeTooltip.top - 40}px` }"
              >
                <div class="text-xs font-medium">{{ activeTooltip.time }}</div>
                <div class="text-xs text-blue-500">{{ activeTooltip.value }} {{ activeTooltip.unit }}</div>
              </div>
            </div>
          </div>

          <!-- Packet Loss -->
          <div class="space-y-2" v-if="hasPacketLossStats">
            <div class="flex items-center justify-between">
              <h4 class="text-sm font-medium">{{ t('stats.packetLoss') }}</h4>
              <span class="text-xs text-muted-foreground">
                {{ currentStats.packetsLost }} {{ t('stats.total') }}
              </span>
            </div>
            <p class="text-xs text-muted-foreground">
              {{ t('stats.packetLossDesc') }}
            </p>
            <div class="relative">
              <div
                ref="packetLossChartRef"
                class="w-full rounded-lg bg-slate-50 dark:bg-slate-900/50 p-2"
              />
              <div
                v-if="activeTooltip.visible && activeTooltip.chartId === 'packetLoss'"
                class="chart-tooltip"
                :style="{ left: `${activeTooltip.left + 60}px`, top: `${activeTooltip.top - 40}px` }"
              >
                <div class="text-xs font-medium">{{ activeTooltip.time }}</div>
                <div class="text-xs text-blue-500">{{ activeTooltip.value }} {{ activeTooltip.unit }}</div>
              </div>
            </div>
          </div>

          <!-- FPS -->
          <div class="space-y-2">
            <div class="flex items-center justify-between">
              <h4 class="text-sm font-medium">{{ t('stats.frameRate') }}</h4>
              <span class="text-xs text-muted-foreground">
                {{ formatFpsValue(currentStats.fps) }} fps
              </span>
            </div>
            <p class="text-xs text-muted-foreground">
              {{ t('stats.frameRateDesc') }}
            </p>
            <div class="relative">
              <div
                ref="fpsChartRef"
                class="w-full rounded-lg bg-slate-50 dark:bg-slate-900/50 p-2"
              />
              <div
                v-if="activeTooltip.visible && activeTooltip.chartId === 'fps'"
                class="chart-tooltip"
                :style="{ left: `${activeTooltip.left + 60}px`, top: `${activeTooltip.top - 40}px` }"
              >
                <div class="text-xs font-medium">{{ activeTooltip.time }}</div>
                <div class="text-xs text-blue-500">{{ activeTooltip.value }} {{ activeTooltip.unit }}</div>
              </div>
            </div>
          </div>

          <!-- Backend Pipeline Latency -->
          <div class="space-y-3 pt-2 border-t border-slate-200 dark:border-slate-800">
            <h4 class="text-sm font-medium">后端流水线</h4>
            <p class="text-xs text-muted-foreground">
              从物理 BMC 设备到浏览器的完整流水线延迟。
            </p>

            <!-- Flow diagram -->
            <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
              <div class="flex items-center justify-between text-xs text-muted-foreground mb-1.5">
                <span>BMC 设备</span>
                <span>帧缓冲</span>
                <span>编码器</span>
                <span>网络</span>
              </div>
              <div class="flex items-center gap-0.5 text-xs">
                <div class="flex-[3] h-8 rounded bg-blue-100 dark:bg-blue-900/30 flex items-center justify-center font-medium text-blue-700 dark:text-blue-300 min-w-0 truncate px-1">
                  {{ fmtMs(perfVal('pipeline_latency_avg_ms')) }}
                </div>
                <span class="text-muted-foreground shrink-0 mx-0.5">→</span>
                <div class="flex-[3] h-8 rounded bg-cyan-100 dark:bg-cyan-900/30 flex items-center justify-center font-medium text-cyan-700 dark:text-cyan-300 min-w-0 truncate px-1">
                  {{ fmtMs(perfVal('fb_snapshot_avg_ms')) }}
                </div>
                <span class="text-muted-foreground shrink-0 mx-0.5">→</span>
                <div class="flex-[3] h-8 rounded bg-emerald-100 dark:bg-emerald-900/30 flex items-center justify-center font-medium text-emerald-700 dark:text-emerald-300 min-w-0 truncate px-1">
                  {{ activeEncodeAvgMsText() }}
                </div>
                <span class="text-muted-foreground shrink-0 mx-0.5">→</span>
                <div class="flex-[3] h-8 rounded bg-purple-100 dark:bg-purple-900/30 flex items-center justify-center font-medium text-purple-700 dark:text-purple-300 min-w-0 truncate px-1">
                  {{ fmtNet(networkLatencyMs) }}
                </div>
              </div>
              <div class="flex items-center justify-between text-[10px] text-muted-foreground mt-1">
                <span class="flex-[3] text-center">拼贴延迟</span>
                <span class="flex-[3] text-center">帧缓冲快照</span>
                <span class="flex-[3] text-center">{{ isWebRTC ? 'GStreamer 编码' : isWebTransport ? 'WebTransport 编码' : 'MJPEG 编码' }}</span>
                <span class="flex-[3] text-center">{{ isWebRTC ? '网络 RTT' : '网络' }}</span>
              </div>
              <div class="flex items-center justify-between text-[9px] text-muted-foreground/60 mt-0.5">
                <span class="flex-[3] text-center">BMC 采集 → 帧缓冲</span>
                <span class="flex-[3] text-center">memcpy + 管道发送</span>
                <span class="flex-[3] text-center">{{ isWebRTC ? 'GStreamer pipeline' : isWebTransport ? '编码包 → WebCodecs' : '帧 → HTTP 输出' }}</span>
                <span class="flex-[3] text-center">{{ isWebRTC ? '服务器 → 浏览器往返' : '无 RTT 数据源' }}</span>
              </div>

              <!-- Summation -->
              <div class="mt-2.5 pt-2 border-t border-slate-200 dark:border-slate-700 flex items-center justify-center gap-1.5 text-xs">
                <span class="text-muted-foreground">拼贴</span>
                <span class="text-muted-foreground">+</span>
                <span class="text-muted-foreground">快照</span>
                <span class="text-muted-foreground">+</span>
                <span class="text-muted-foreground">编码</span>
                <span class="text-muted-foreground">+</span>
                <span class="text-muted-foreground">网络</span>
                <span class="text-muted-foreground">=</span>
                <span class="font-bold text-amber-600 dark:text-amber-400">{{ fmtMs(totalLatencyMs) }}</span>
                <span class="text-[10px] text-muted-foreground ml-0.5">端到端</span>
              </div>
            </div>

            <!-- Latency detail grid -->
            <div class="grid grid-cols-2 gap-2">
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-2.5">
                <p class="text-[10px] text-muted-foreground">连接耗时</p>
                <p class="text-xs font-medium mt-0.5">{{ fmtMs(perfVal('connect_duration_ms')) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-2.5">
                <p class="text-[10px] text-muted-foreground">拼贴速率</p>
                <p class="text-xs font-medium mt-0.5">{{ fmtVal(perfVal('tile_rate')) }} /s</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-2.5">
                <p class="text-[10px] text-muted-foreground">拼贴延迟 (最大)</p>
                <p class="text-xs font-medium mt-0.5">{{ fmtMs(perfVal('pipeline_latency_max_ms')) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-2.5">
                <p class="text-[10px] text-muted-foreground">编码耗时 (最大)</p>
                <p class="text-xs font-medium mt-0.5">{{ activeEncodeMaxMsText() }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-2.5">
                <p class="text-[10px] text-muted-foreground">编码样本</p>
                <p class="text-xs font-medium mt-0.5">{{ encodeSampleStatus(activeEncodeCountValue()) }}</p>
              </div>
            </div>

            <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3 space-y-3">
              <div class="flex items-center justify-between gap-3">
                <h5 class="text-xs font-medium">链路 Trace</h5>
                <span class="text-[10px] text-muted-foreground truncate">
                  {{ traceError || `trace #${traceNum('processor.last_trace_id')}` }}
                </span>
              </div>
              <div class="grid grid-cols-3 gap-2 text-xs">
                <div>
                  <p class="text-[10px] text-muted-foreground">Processor</p>
                  <p class="font-medium mt-0.5">{{ traceStr('processor.pixel_format') }} {{ traceNum('processor.width') }}x{{ traceNum('processor.height') }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Receivers</p>
                  <p class="font-medium mt-0.5">{{ traceNum('processor.broadcast_receivers') }} raw / {{ traceNum('processor.webrtc_connections') }} rtc / {{ traceNum('processor.webtransport_connections') }} wt</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Last age</p>
                  <p class="font-medium mt-0.5">{{ fmtMs(traceNum('processor.last_frame_age_ms')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Raw frames</p>
                  <p class="font-medium mt-0.5">{{ fmtVal(traceNum('processor.raw_frames_published')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Replayed base</p>
                  <p class="font-medium mt-0.5">{{ fmtVal(traceNum('processor.raw_frames_replayed')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Raw MB</p>
                  <p class="font-medium mt-0.5">{{ (traceNum('processor.raw_bytes_in') / 1048576).toFixed(1) }}</p>
                </div>
              </div>
            </div>

            <div v-if="isWebRTC" class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3 space-y-3">
              <div class="flex items-center justify-between gap-3">
                <h5 class="text-xs font-medium">GStreamer Pipeline</h5>
                <span class="text-[10px] text-muted-foreground">{{ gstStr('pipeline_state') }}</span>
              </div>
              <div class="grid grid-cols-3 gap-2 text-xs">
                <div>
                  <p class="text-[10px] text-muted-foreground">Caps</p>
                  <p class="font-medium mt-0.5">{{ gstStr('expected_caps.pixel_format') }} {{ gstNum('expected_caps.width') }}x{{ gstNum('expected_caps.height') }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Raw → appsrc</p>
                  <p class="font-medium mt-0.5">{{ fmtMs(gstNum('frames.last_raw_to_appsrc_ms')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Appsrc queue</p>
                  <p class="font-medium mt-0.5">{{ fmtVal(gstNum('appsrc.current_level_bytes')) }} B</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Pushed</p>
                  <p class="font-medium mt-0.5">{{ fmtVal(gstNum('frames.pushed')) }} / {{ fmtVal(gstNum('frames.received')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Lagged</p>
                  <p class="font-medium mt-0.5" :class="gstNum('frames.lagged') > 0 ? 'text-amber-600 dark:text-amber-400' : ''">{{ fmtVal(gstNum('frames.lagged')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Caps mismatch</p>
                  <p class="font-medium mt-0.5" :class="gstNum('frames.caps_mismatch') > 0 ? 'text-red-600 dark:text-red-400' : ''">{{ fmtVal(gstNum('frames.caps_mismatch')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Encode avg</p>
                  <p class="font-medium mt-0.5">{{ fmtEncodeMs(gstNum('encoder.webrtc_encode_time_avg_ms'), gstNum('encoder.webrtc_encode_count')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Encode max</p>
                  <p class="font-medium mt-0.5">{{ fmtEncodeMs(gstNum('encoder.webrtc_encode_time_max_ms'), gstNum('encoder.webrtc_encode_count')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Encode samples</p>
                  <p class="font-medium mt-0.5">{{ encodeSampleStatus(gstNum('encoder.webrtc_encode_count')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Push errors</p>
                  <p class="font-medium mt-0.5" :class="gstNum('appsrc.push_errors') > 0 ? 'text-red-600 dark:text-red-400' : ''">{{ fmtVal(gstNum('appsrc.push_errors')) }}</p>
                </div>
              </div>
            </div>

            <div v-if="isWebTransport" class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3 space-y-3">
              <div class="flex items-center justify-between gap-3">
                <h5 class="text-xs font-medium">WebTransport Pipeline</h5>
                <span class="text-[10px] text-muted-foreground truncate">
                  {{ webTransportTraceStr('encoder_backend') }} / {{ webTransportTraceStr('codec') }}
                </span>
              </div>
              <div class="grid grid-cols-3 gap-2 text-xs">
                <div>
                  <p class="text-[10px] text-muted-foreground">Caps</p>
                  <p class="font-medium mt-0.5">{{ webTransportTraceStr('expected_caps.pixel_format') }} {{ webTransportTraceNum('expected_caps.width') }}x{{ webTransportTraceNum('expected_caps.height') }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Raw frames</p>
                  <p class="font-medium mt-0.5">{{ fmtVal(webTransportTraceNum('frames.raw_received')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Lagged</p>
                  <p class="font-medium mt-0.5" :class="webTransportTraceNum('frames.lagged') > 0 ? 'text-amber-600 dark:text-amber-400' : ''">{{ fmtVal(webTransportTraceNum('frames.lagged')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Packets</p>
                  <p class="font-medium mt-0.5">{{ fmtVal(webTransportTraceNum('encoder.packets')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Keyframes</p>
                  <p class="font-medium mt-0.5">{{ fmtVal(webTransportTraceNum('encoder.keyframes')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Bitrate</p>
                  <p class="font-medium mt-0.5">{{ fmtMbps(webTransportTraceNum('encoder.bitrate_mbps')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Encode avg</p>
                  <p class="font-medium mt-0.5">{{ fmtEncodeMs(webTransportTraceNum('encoder.encode_time_avg_ms'), webTransportTraceNum('encoder.encode_count')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Encode max</p>
                  <p class="font-medium mt-0.5">{{ fmtEncodeMs(webTransportTraceNum('encoder.encode_time_max_ms'), webTransportTraceNum('encoder.encode_count')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Encode samples</p>
                  <p class="font-medium mt-0.5">{{ encodeSampleStatus(webTransportTraceNum('encoder.encode_count')) }}</p>
                </div>
                <div>
                  <p class="text-[10px] text-muted-foreground">Errors</p>
                  <p
                    class="font-medium mt-0.5"
                    :class="webTransportTraceNum('encoder.encode_errors') + webTransportTraceNum('stream.write_errors') > 0 ? 'text-red-600 dark:text-red-400' : ''"
                  >
                    {{ fmtVal(webTransportTraceNum('encoder.encode_errors')) }} enc / {{ fmtVal(webTransportTraceNum('stream.write_errors')) }} write
                  </p>
                </div>
              </div>
            </div>
          </div>

          <!-- Additional Stats -->
          <div class="space-y-3 pt-2 border-t border-slate-200 dark:border-slate-800">
            <h4 class="text-sm font-medium">{{ t('stats.additional') }}</h4>
            <div class="grid grid-cols-2 gap-3">
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">{{ t('stats.resolution') }}</p>
                <p class="text-sm font-medium mt-1">{{ currentStats.resolution }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">{{ t('stats.bitrate') }}</p>
                <p class="text-sm font-medium mt-1">{{ fmtMbps(currentStats.bitrateMbps) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Codec</p>
                <p class="text-sm font-medium mt-1 truncate">{{ currentStats.codec }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Decoder</p>
                <p class="text-sm font-medium mt-1 truncate">{{ currentStats.decoder }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Frames decoded</p>
                <p class="text-sm font-medium mt-1">{{ fmtCount(currentStats.framesDecoded) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Dropped frames</p>
                <p class="text-sm font-medium mt-1">{{ fmtCount(currentStats.framesDropped) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Keyframes</p>
                <p class="text-sm font-medium mt-1">{{ fmtCount(currentStats.keyFramesDecoded) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Packets received</p>
                <p class="text-sm font-medium mt-1">{{ fmtCount(currentStats.packetsReceived) }}</p>
              </div>
            </div>

            <!-- Connection Info -->
            <h4 v-if="isWebRTC" class="text-sm font-medium pt-2">{{ t('stats.connection') }}</h4>
            <div v-if="isWebRTC" class="grid grid-cols-2 gap-3">
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">{{ t('stats.connectionType') }}</p>
                <p class="text-sm font-medium mt-1 flex items-center gap-1.5">
                  <span
                    :class="[
                      'inline-block w-2 h-2 rounded-full',
                      currentStats.isRelay ? 'bg-amber-500' : 'bg-green-500'
                    ]"
                  />
                  {{ currentStats.isRelay ? t('stats.relay') : t('stats.p2p') }}
                </p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">{{ t('stats.transport') }}</p>
                <p class="text-sm font-medium mt-1">{{ currentStats.transport }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">{{ t('stats.localCandidate') }}</p>
                <p class="text-sm font-medium mt-1">{{ currentStats.localType }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">{{ t('stats.remoteCandidate') }}</p>
                <p class="text-sm font-medium mt-1">{{ currentStats.remoteType }}</p>
              </div>
            </div>

            <div v-if="isWebRTC" class="grid grid-cols-2 gap-3">
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Decode avg</p>
                <p class="text-sm font-medium mt-1">{{ fmtNet(currentStats.decodeTime) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Jitter buffer</p>
                <p class="text-sm font-medium mt-1">{{ fmtNet(currentStats.jitterBufferDelay) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Freezes</p>
                <p class="text-sm font-medium mt-1">{{ fmtCount(currentStats.freezes) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">PLI / FIR / NACK</p>
                <p class="text-sm font-medium mt-1">{{ fmtCount(currentStats.pli) }} / {{ fmtCount(currentStats.fir) }} / {{ fmtCount(currentStats.nack) }}</p>
              </div>
            </div>

            <div v-if="isWebTransport" class="grid grid-cols-2 gap-3">
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Pixel format</p>
                <p class="text-sm font-medium mt-1">{{ currentStats.pixelFormat || '-' }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Last trace</p>
                <p class="text-sm font-medium mt-1">{{ fmtCount(currentStats.lastTraceId) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Frame age</p>
                <p class="text-sm font-medium mt-1">{{ fmtNet(currentStats.lastFrameAgeMs) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Decoder queue</p>
                <p class="text-sm font-medium mt-1">{{ fmtCount(currentStats.decoderQueueSize) }}</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Buffered bytes</p>
                <p class="text-sm font-medium mt-1">{{ fmtCount(currentStats.packetsBuffered) }} B</p>
              </div>
              <div class="rounded-lg bg-slate-50 dark:bg-slate-900/50 p-3">
                <p class="text-xs text-muted-foreground">Connection id</p>
                <p class="text-sm font-medium mt-1 truncate">{{ props.webtransportConnectionId || '-' }}</p>
              </div>
            </div>
          </div>
        </div>
      </ScrollArea>
    </SheetContent>
  </Sheet>
</template>

<style>
/* Override uPlot styles for dark mode */
.dark .u-wrap {
  background: transparent !important;
}

.dark .u-over {
  background: transparent !important;
}

/* Chart cursor line */
.u-cursor-x {
  border-right: 1px dashed #64748b !important;
}

.u-cursor-y {
  display: none !important;
}

/* Chart tooltip */
.chart-tooltip {
  position: absolute;
  z-index: 50;
  pointer-events: none;
  padding: 6px 10px;
  border-radius: 6px;
  background: rgba(15, 23, 42, 0.9);
  color: white;
  box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1);
  white-space: nowrap;
  transform: translateX(-50%);
}

.dark .chart-tooltip {
  background: rgba(30, 41, 59, 0.95);
  border: 1px solid rgba(71, 85, 105, 0.5);
}
</style>
