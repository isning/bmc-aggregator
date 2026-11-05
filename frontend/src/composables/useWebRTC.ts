import { ref, computed, type Ref } from 'vue'
import { createWebrtcApi, type IceCandidate } from '@/api'
import { captureDeviceRequest, currentBmcIdFromLocation } from '@/api/request'
import { videoDebugLog } from '@/lib/debugLog'

export type WebRTCState = 'disconnected' | 'connecting' | 'connected' | 'failed'
export type WebRTCConnectStage =
  | 'idle'
  | 'fetching_ice_servers'
  | 'creating_peer_connection'
  | 'creating_offer'
  | 'waiting_server_answer'
  | 'setting_remote_description'
  | 'applying_ice_candidates'
  | 'waiting_connection'
  | 'connected'
  | 'disconnected'
  | 'failed'

export type IceCandidateType = 'host' | 'srflx' | 'prflx' | 'relay' | 'unknown'

export interface WebRTCStats {
  bytesReceived: number
  packetsReceived: number
  packetsLost: number
  framesDecoded: number
  framesDropped: number
  keyFramesDecoded: number
  frameWidth: number
  frameHeight: number
  framesPerSecond: number
  jitter: number
  roundTripTime: number
  totalDecodeTime: number
  totalProcessingDelay: number
  jitterBufferDelay: number
  jitterBufferEmittedCount: number
  freezeCount: number
  pliCount: number
  firCount: number
  nackCount: number
  qpSum: number
  codecId: string
  mimeType: string
  decoderImplementation: string
  localCandidateType: IceCandidateType
  remoteCandidateType: IceCandidateType
  transportProtocol: string
  isRelay: boolean
}

export interface WebRTCConnectOptions {
  codec?: string
  encoder?: string | null
}

async function fetchIceServers(api: ReturnType<typeof createWebrtcApi>, current: () => boolean): Promise<RTCIceServer[]> {
  try {
    const response = await api.getIceServers()
    if (!current()) return []
    if (response.mdns_mode) {
      allowMdnsHostCandidates = response.mdns_mode !== 'disabled'
    } else if (response.ice_servers) {
      allowMdnsHostCandidates = response.ice_servers.length === 0
    }

    if (response.ice_servers && response.ice_servers.length > 0) {
      const iceServers = response.ice_servers.map(server => ({
        urls: server.urls,
        username: server.username,
        credential: server.credential,
      }))
      console.log('[WebRTC] ICE servers loaded from API:', iceServers.length)
      return iceServers
    }
  } catch (err) {
    console.warn('[WebRTC] Failed to fetch ICE servers from API:', err)
  }

  if (!current()) return []
  const isLocalConnection = typeof window !== 'undefined' &&
    (window.location.hostname === 'localhost' ||
     window.location.hostname === '127.0.0.1' ||
     window.location.hostname.startsWith('192.168.') ||
     window.location.hostname.startsWith('10.'))

  if (isLocalConnection) {
    allowMdnsHostCandidates = false
    console.log('[WebRTC] Local connection detected, using backend ICE config only')
    return []
  }

  console.log('[WebRTC] No backend ICE servers available')
  return []
}

let peerConnection: RTCPeerConnection | null = null
let peerGeneration = 0
let peerDeviceId: string | null = null
let peerApi: ReturnType<typeof createWebrtcApi> | null = null
let statsInterval: number | null = null
let isConnecting = false
let connectInFlight: Promise<boolean> | null = null
let pendingIceCandidates: RTCIceCandidate[] = []
const seenRemoteCandidates = new Set<string>()
let offerSent = false
let cachedMediaStream: MediaStream | null = null
let lastVideoFramesDecoded = 0
let lastVideoBytesReceived = 0
let activeCodec: string | undefined
let activeEncoder: string | null | undefined

let allowMdnsHostCandidates = false

const state = ref<WebRTCState>('disconnected')
const videoTrack = ref<MediaStreamTrack | null>(null)
const audioTrack = ref<MediaStreamTrack | null>(null)
const stats = ref<WebRTCStats>({
  bytesReceived: 0,
  packetsReceived: 0,
  packetsLost: 0,
  framesDecoded: 0,
  framesDropped: 0,
  keyFramesDecoded: 0,
  frameWidth: 0,
  frameHeight: 0,
  framesPerSecond: 0,
  jitter: 0,
  roundTripTime: 0,
  totalDecodeTime: 0,
  totalProcessingDelay: 0,
  jitterBufferDelay: 0,
  jitterBufferEmittedCount: 0,
  freezeCount: 0,
  pliCount: 0,
  firCount: 0,
  nackCount: 0,
  qpSum: 0,
  codecId: '',
  mimeType: '',
  decoderImplementation: '',
  localCandidateType: 'unknown',
  remoteCandidateType: 'unknown',
  transportProtocol: '',
  isRelay: false,
})
const error = ref<string | null>(null)
const connectStage = ref<WebRTCConnectStage>('idle')
const connectionId = ref<string | null>(null)
const lastVideoActivityAt = ref(0)

function setConnectStage(stage: WebRTCConnectStage, details?: unknown) {
  connectStage.value = stage
  videoDebugLog(`WebRTC stage -> ${stage}`, details)
}

function createConnectionId(): string {
  try {
    if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
      return crypto.randomUUID()
    }
  } catch {
  }
  return `${Date.now()}-${Math.random().toString(16).slice(2)}`
}

function summarizeIceCandidate(candidate: RTCIceCandidate | IceCandidate | RTCIceCandidateInit | null) {
  if (!candidate) return null
  const candidateLine = candidate.candidate ?? ''
  const parts = candidateLine.trim().split(/\s+/)
  const typIndex = parts.indexOf('typ')
  const raddrIndex = parts.indexOf('raddr')
  const rportIndex = parts.indexOf('rport')

  return {
    type: typIndex >= 0 ? parts[typIndex + 1] : 'unknown',
    protocol: parts[2] ?? '',
    address: parts[4] ?? '',
    port: parts[5] ?? '',
    relatedAddress: raddrIndex >= 0 ? parts[raddrIndex + 1] : undefined,
    relatedPort: rportIndex >= 0 ? parts[rportIndex + 1] : undefined,
    sdpMid: candidate.sdpMid ?? undefined,
    sdpMLineIndex: candidate.sdpMLineIndex ?? undefined,
    usernameFragment: candidate.usernameFragment ?? undefined,
    raw: candidateLine,
  }
}

function isActivePeer(pc: RTCPeerConnection, generation: number): boolean {
  return peerConnection === pc && peerGeneration === generation
    && peerDeviceId === currentBmcIdFromLocation()
}

function isIceConnected(pc: RTCPeerConnection): boolean {
  return pc.iceConnectionState === 'connected' || pc.iceConnectionState === 'completed'
}

function hasRecentVideoActivity(maxAgeMs = 3000): boolean {
  return lastVideoActivityAt.value > 0 && Date.now() - lastVideoActivityAt.value <= maxAgeMs
}

function hasUsableRemoteVideo(pc: RTCPeerConnection | null = peerConnection): boolean {
  if (!pc || !videoTrack.value) return false
  return pc.connectionState === 'connected' || isIceConnected(pc) || hasRecentVideoActivity()
}

function resetVideoActivity() {
  lastVideoFramesDecoded = 0
  lastVideoBytesReceived = 0
  lastVideoActivityAt.value = 0
}

function markConnected(details?: unknown) {
  if (state.value !== 'connected') {
    setConnectStage('connected', details)
  }
  state.value = 'connected'
  error.value = null
  startStatsCollection()
}

function createPeerConnection(iceServers: RTCIceServer[], generation: number, api: ReturnType<typeof createWebrtcApi>): RTCPeerConnection {
  const config: RTCConfiguration = { iceServers }

  const pc = new RTCPeerConnection(config)

  pc.onconnectionstatechange = () => {
    if (!isActivePeer(pc, generation)) return
    switch (pc.connectionState) {
      case 'connecting':
        if (state.value !== 'connected') state.value = 'connecting'
        break
      case 'connected':
        markConnected({ source: 'connectionstate' })
        break
      case 'disconnected':
      case 'closed':
        if (hasUsableRemoteVideo(pc)) {
          videoDebugLog('Ignoring WebRTC disconnected/closed state while video is active', {
            connectionState: pc.connectionState,
            iceConnectionState: pc.iceConnectionState,
          })
          break
        }
        state.value = 'disconnected'
        setConnectStage('disconnected')
        stopStatsCollection()
        break
      case 'failed':
        if (hasUsableRemoteVideo(pc)) {
          videoDebugLog('Ignoring WebRTC failed state while video is active', {
            connectionState: pc.connectionState,
            iceConnectionState: pc.iceConnectionState,
          })
          break
        }
        state.value = 'failed'
        setConnectStage('failed')
        error.value = 'Connection failed'
        stopStatsCollection()
        break
    }
  }

  pc.oniceconnectionstatechange = () => {
    if (!isActivePeer(pc, generation)) return
    if (isIceConnected(pc) && videoTrack.value) {
      markConnected({
        source: 'iceconnectionstate',
        iceConnectionState: pc.iceConnectionState,
      })
    }
  }

  pc.onicecandidate = async (event) => {
    if (!isActivePeer(pc, generation)) return
    if (!event.candidate) {
      return
    }
    if (shouldSkipLocalCandidate(event.candidate)) {
      return
    }

    if (!offerSent) {
      pendingIceCandidates.push(event.candidate)
      return
    }

    try {
      if (!connectionId.value) {
        pendingIceCandidates.push(event.candidate)
        return
      }

      await api.addIceCandidate({
        candidate: event.candidate.candidate,
        sdpMid: event.candidate.sdpMid ?? undefined,
        sdpMLineIndex: event.candidate.sdpMLineIndex ?? undefined,
        usernameFragment: event.candidate.usernameFragment ?? undefined,
      }, connectionId.value)
    } catch (err) {
      videoDebugLog('Failed to send local ICE candidate', {
        candidate: summarizeIceCandidate(event.candidate),
        error: err,
      })
    }
  }

  pc.ontrack = (event) => {
    if (!isActivePeer(pc, generation)) return
    const track = event.track

    if (track.kind === 'video') {
      videoTrack.value = track
      if (pc.connectionState === 'connected' || isIceConnected(pc)) {
        markConnected({ source: 'track', kind: track.kind })
      }
    } else if (track.kind === 'audio') {
      audioTrack.value = track
    }
  }

  return pc
}

function shouldSkipLocalCandidate(candidate: RTCIceCandidate): boolean {
  if (allowMdnsHostCandidates) return false
  const value = candidate.candidate || ''
  return value.includes(' typ host') && value.includes('.local')
}

async function addRemoteIceCandidate(candidate: IceCandidate, pc: RTCPeerConnection | null = peerConnection): Promise<boolean> {
  if (!pc) return false
  if (!candidate.candidate) return false
  if (seenRemoteCandidates.has(candidate.candidate)) {
    return false
  }
  seenRemoteCandidates.add(candidate.candidate)

  const iceCandidate: RTCIceCandidateInit = {
    candidate: candidate.candidate,
    sdpMid: candidate.sdpMid ?? undefined,
    sdpMLineIndex: candidate.sdpMLineIndex ?? undefined,
    usernameFragment: candidate.usernameFragment ?? undefined,
  }

  try {
    await pc.addIceCandidate(iceCandidate)
    return true
  } catch (err) {
    videoDebugLog('Failed to apply remote ICE candidate', {
      candidate: summarizeIceCandidate(candidate),
      error: err,
    })
    return false
  }
}

function startStatsCollection() {
  if (statsInterval) return

  let reading = false
  statsInterval = window.setInterval(async () => {
    const pc = peerConnection
    const generation = peerGeneration
    if (reading || !pc || !isActivePeer(pc, generation)) return

    reading = true
    try {
      const report = await pc.getStats()
      if (!isActivePeer(pc, generation)) return

      const candidates: Record<string, { type: IceCandidateType; protocol: string }> = {}
      const codecs: Record<string, { mimeType: string }> = {}
      const candidatePairs: Record<string, { state?: string; nominated?: boolean; localCandidateId?: string; remoteCandidateId?: string; currentRoundTripTime?: number; totalRoundTripTime?: number }> = {}
      let selectedCandidatePairId = ''
      let selectedPairLocalId = ''
      let selectedPairRemoteId = ''
      let selectedPairRtt = 0
      let remoteInboundRtt = 0
      let foundActivePair = false

      report.forEach((stat) => {
        if (stat.type === 'codec') {
          codecs[stat.id] = {
            mimeType: stat.mimeType || '',
          }
        }

        if (stat.type === 'local-candidate' || stat.type === 'remote-candidate') {
          candidates[stat.id] = {
            type: (stat.candidateType as IceCandidateType) || 'unknown',
            protocol: stat.protocol || '',
          }
        }

        if (stat.type === 'transport' && stat.selectedCandidatePairId) {
          selectedCandidatePairId = stat.selectedCandidatePairId
        }

        if (stat.type === 'candidate-pair') {
          candidatePairs[stat.id] = stat
          const isActive = stat.nominated === true ||
                          (stat.state === 'succeeded' && stat.selected === true) ||
                          stat.id === selectedCandidatePairId ||
                          (stat.state === 'in-progress' && !foundActivePair)

          const hasData = (stat.bytesReceived > 0 || stat.bytesSent > 0)

          if ((isActive || (stat.state === 'succeeded' && hasData)) && !foundActivePair) {
            selectedPairRtt = stat.currentRoundTripTime || stat.totalRoundTripTime || selectedPairRtt
            selectedPairLocalId = stat.localCandidateId || ''
            selectedPairRemoteId = stat.remoteCandidateId || ''
            if (stat.nominated === true || stat.selected === true || stat.id === selectedCandidatePairId) {
              foundActivePair = true
            }
          }
        }

        if (stat.type === 'remote-inbound-rtp' && stat.kind === 'video') {
          remoteInboundRtt = stat.roundTripTime || remoteInboundRtt
        }

        if (stat.type === 'inbound-rtp' && stat.kind === 'video') {
          const bytesReceived = stat.bytesReceived || 0
          const framesDecoded = stat.framesDecoded || 0
          const frameWidth = stat.frameWidth || 0
          const frameHeight = stat.frameHeight || 0
          if (
            framesDecoded > lastVideoFramesDecoded ||
            bytesReceived > lastVideoBytesReceived ||
            (frameWidth > 0 && frameHeight > 0)
          ) {
            lastVideoActivityAt.value = Date.now()
          }
          lastVideoFramesDecoded = Math.max(lastVideoFramesDecoded, framesDecoded)
          lastVideoBytesReceived = Math.max(lastVideoBytesReceived, bytesReceived)
          stats.value.bytesReceived = bytesReceived
          stats.value.packetsReceived = stat.packetsReceived || 0
          stats.value.packetsLost = stat.packetsLost || 0
          stats.value.framesDecoded = framesDecoded
          stats.value.framesDropped = stat.framesDropped || 0
          stats.value.keyFramesDecoded = stat.keyFramesDecoded || 0
          stats.value.frameWidth = frameWidth
          stats.value.frameHeight = frameHeight
          stats.value.framesPerSecond = stat.framesPerSecond || 0
          stats.value.jitter = stat.jitter || 0
          stats.value.totalDecodeTime = stat.totalDecodeTime || 0
          stats.value.totalProcessingDelay = stat.totalProcessingDelay || 0
          stats.value.jitterBufferDelay = stat.jitterBufferDelay || 0
          stats.value.jitterBufferEmittedCount = stat.jitterBufferEmittedCount || 0
          stats.value.freezeCount = stat.freezeCount || 0
          stats.value.pliCount = stat.pliCount || 0
          stats.value.firCount = stat.firCount || 0
          stats.value.nackCount = stat.nackCount || 0
          stats.value.qpSum = stat.qpSum || 0
          stats.value.codecId = stat.codecId || ''
          stats.value.mimeType = stat.mimeType || codecs[stat.codecId]?.mimeType || stats.value.mimeType
          stats.value.decoderImplementation = stat.decoderImplementation || ''
        }
      })

      const pair = selectedCandidatePairId ? candidatePairs[selectedCandidatePairId] : undefined
      if (pair) {
        selectedPairRtt = pair.currentRoundTripTime || pair.totalRoundTripTime || selectedPairRtt
        selectedPairLocalId = pair.localCandidateId || selectedPairLocalId
        selectedPairRemoteId = pair.remoteCandidateId || selectedPairRemoteId
      }
      stats.value.roundTripTime = selectedPairRtt || remoteInboundRtt || stats.value.roundTripTime

      const localCandidate = selectedPairLocalId ? candidates[selectedPairLocalId] : undefined
      const remoteCandidate = selectedPairRemoteId ? candidates[selectedPairRemoteId] : undefined

      if (localCandidate) {
        stats.value.localCandidateType = localCandidate.type
        stats.value.transportProtocol = localCandidate.protocol
      }
      if (remoteCandidate) {
        stats.value.remoteCandidateType = remoteCandidate.type
      }

      stats.value.isRelay = stats.value.localCandidateType === 'relay' || stats.value.remoteCandidateType === 'relay'
      if (hasUsableRemoteVideo(peerConnection) && state.value !== 'connected') {
        markConnected({ source: 'stats' })
      }
    } catch {
    } finally {
      reading = false
    }
  }, 1000)
}

function stopStatsCollection() {
  if (statsInterval) {
    clearInterval(statsInterval)
    statsInterval = null
  }
}

async function connect(options: WebRTCConnectOptions = {}): Promise<boolean> {
  if (connectInFlight) {
    return connectInFlight
  }

  const task = (async () => {
    if (isConnecting) {
      return state.value === 'connected' || hasUsableRemoteVideo()
    }

    const requestedCodec = options.codec
    const requestedEncoder = options.encoder ?? null
    const canReuseCurrentConnection =
      peerConnection &&
      isActivePeer(peerConnection, peerGeneration) &&
      (state.value === 'connected' || hasUsableRemoteVideo()) &&
      activeCodec === requestedCodec &&
      activeEncoder === requestedEncoder

    if (canReuseCurrentConnection) {
      return true
    }

    if (peerConnection) {
      const closingGeneration = peerGeneration + 1
      await disconnect()
      if (peerGeneration !== closingGeneration) return false
    }
    isConnecting = true
    const localGeneration = ++peerGeneration
    const deviceId = currentBmcIdFromLocation()
    const current = () => peerGeneration === localGeneration && deviceId === currentBmcIdFromLocation()
    const assertCurrent = () => {
      if (!current()) throw new Error('WebRTC connection superseded')
    }

    pendingIceCandidates = []
    seenRemoteCandidates.clear()
    offerSent = false
    connectionId.value = null
    resetVideoActivity()

    let localPc: RTCPeerConnection | null = null
    try {
      const api = createWebrtcApi(captureDeviceRequest().request)
      state.value = 'connecting'
      error.value = null
      setConnectStage('fetching_ice_servers')

      const iceServers = await fetchIceServers(api, current)
      assertCurrent()
      setConnectStage('creating_peer_connection', { iceServerCount: iceServers.length })

      localPc = createPeerConnection(iceServers, localGeneration, api)
      peerConnection = localPc
      peerDeviceId = deviceId
      peerApi = api

      localPc.addTransceiver('video', { direction: 'recvonly' })
      localPc.addTransceiver('audio', { direction: 'recvonly' })
      setConnectStage('creating_offer')

      const offer = await localPc.createOffer()
      assertCurrent()
      await localPc.setLocalDescription(offer)
      assertCurrent()
      setConnectStage('waiting_server_answer')

      const requestedConnectionId = createConnectionId()
      const response = await api.offer(
        offer.sdp!,
        requestedConnectionId,
        options.codec,
        options.encoder,
      )
      if (!current()) {
        // A cancelled offer may still create a remote session; release only its original device.
        void api.close(response.connection_id).catch(() => {})
        assertCurrent()
      }
      connectionId.value = response.connection_id
      offerSent = true

      // Flush any ICE candidates gathered before the offer completed.
      const flush = [...pendingIceCandidates]
      pendingIceCandidates = []
      for (const c of flush) {
        assertCurrent()
        if (!connectionId.value) break
        try {
          await api.addIceCandidate({
            candidate: c.candidate,
            sdpMid: c.sdpMid ?? undefined,
            sdpMLineIndex: c.sdpMLineIndex ?? undefined,
            usernameFragment: c.usernameFragment ?? undefined,
          }, connectionId.value)
        } catch (_) { /* candidate may be stale, ignore */ }
      }

      assertCurrent()
      const answer: RTCSessionDescriptionInit = {
        type: 'answer',
        sdp: response.sdp,
      }
      await localPc.setRemoteDescription(answer)
      assertCurrent()
      startStatsCollection()

      setConnectStage('applying_ice_candidates', {
        answerCandidates: response.ice_candidates?.length ?? 0,
      })
      let appliedAnswerCandidates = 0
      for (const candidate of response.ice_candidates ?? []) {
        assertCurrent()
        if (await addRemoteIceCandidate(candidate, localPc)) {
          assertCurrent()
          appliedAnswerCandidates += 1
        }
      }
      try {
        await localPc.addIceCandidate(null)
      } catch (err) {
        videoDebugLog('Failed to apply remote ICE end-of-candidates from answer response', {
          appliedAnswerCandidates,
          error: err,
        })
      }

      assertCurrent()
      const connectionTimeout = 5000
      const iceConnectedTimeout = 12000
      const pollInterval = 100
      let waited = 0
      setConnectStage('waiting_connection', {
        connectionTimeout,
        iceConnectedTimeout,
        pollInterval,
      })

      while (localPc && isActivePeer(localPc, localGeneration)) {
        const pcState = localPc.connectionState
        const iceState = localPc.iceConnectionState
        const timeoutForState = iceState === 'connected' || iceState === 'completed'
          ? iceConnectedTimeout
          : connectionTimeout
        if (waited >= timeoutForState) break

        if (pcState === 'connected' || hasUsableRemoteVideo(localPc)) {
          markConnected({ source: 'connect-wait', waited })
          activeCodec = requestedCodec
          activeEncoder = requestedEncoder
          isConnecting = false
          return true
        }
        if ((pcState === 'failed' || pcState === 'closed') && !hasUsableRemoteVideo(localPc)) {
          throw new Error('Connection failed during ICE negotiation')
        }
        await new Promise(resolve => setTimeout(resolve, pollInterval))
        waited += pollInterval
      }

      if (localPc && isActivePeer(localPc, localGeneration) && hasUsableRemoteVideo(localPc)) {
        markConnected({ source: 'connect-timeout-media-active', waited })
        activeCodec = requestedCodec
        activeEncoder = requestedEncoder
        isConnecting = false
        return true
      }

      videoDebugLog('WebRTC connect timed out waiting for ICE/DTLS', {
        waited,
        connectionState: localPc?.connectionState,
        iceConnectionState: localPc?.iceConnectionState,
        iceGatheringState: localPc?.iceGatheringState,
        signalingState: localPc?.signalingState,
      })
      throw new Error('Connection timeout waiting for ICE negotiation')
    } catch (err) {
      if (peerGeneration === localGeneration) {
        const details = {
          error: err,
          connectionState: localPc?.connectionState,
          iceConnectionState: localPc?.iceConnectionState,
          iceGatheringState: localPc?.iceGatheringState,
          signalingState: localPc?.signalingState,
        }
        // disconnect clears local state synchronously; publish failure afterwards.
        const closing = disconnect()
        state.value = 'failed'
        setConnectStage('failed', details)
        error.value = err instanceof Error ? err.message : 'Connection failed'
        await closing
      } else {
        videoDebugLog('Ignoring stale WebRTC connect failure', { error: err })
      }
      return false
    }
  })()

  connectInFlight = task
  try {
    return await task
  } finally {
    if (connectInFlight === task) connectInFlight = null
  }
}

async function disconnect() {
  stopStatsCollection()

  isConnecting = false
  connectInFlight = null
  pendingIceCandidates = []
  seenRemoteCandidates.clear()
  offerSent = false

  const closingConnectionId = connectionId.value
  const closingPc = peerConnection
  const closingApi = peerApi
  peerGeneration++
  peerConnection = null
  peerApi = null
  peerDeviceId = null

  if (closingPc) {
    closingPc.onconnectionstatechange = null
    closingPc.oniceconnectionstatechange = null
    closingPc.onicecandidate = null
    closingPc.ontrack = null
    closingPc.close()
  }

  videoTrack.value = null
  audioTrack.value = null
  cachedMediaStream = null
  activeCodec = undefined
  activeEncoder = undefined
  connectionId.value = null
  resetVideoActivity()
  state.value = 'disconnected'
  setConnectStage('disconnected')
  error.value = null

  stats.value = {
    bytesReceived: 0,
    packetsReceived: 0,
    packetsLost: 0,
    framesDecoded: 0,
    framesDropped: 0,
    keyFramesDecoded: 0,
    frameWidth: 0,
    frameHeight: 0,
    framesPerSecond: 0,
    jitter: 0,
    roundTripTime: 0,
    totalDecodeTime: 0,
    totalProcessingDelay: 0,
    jitterBufferDelay: 0,
    jitterBufferEmittedCount: 0,
    freezeCount: 0,
    pliCount: 0,
    firCount: 0,
    nackCount: 0,
    qpSum: 0,
    codecId: '',
    mimeType: '',
    decoderImplementation: '',
    localCandidateType: 'unknown',
    remoteCandidateType: 'unknown',
    transportProtocol: '',
    isRelay: false,
  }

  try {
    if (closingConnectionId && closingApi) {
      await closingApi.close(closingConnectionId)
    }
  } catch (err) {
    videoDebugLog('Failed to close backend WebRTC session', {
      error: err,
    })
  }

}

function getMediaStream(): MediaStream | null {
  if (!videoTrack.value && !audioTrack.value) {
    return null
  }

  if (cachedMediaStream) {
    const currentVideoTracks = cachedMediaStream.getVideoTracks()
    const currentAudioTracks = cachedMediaStream.getAudioTracks()

    const videoMatches = videoTrack.value
      ? currentVideoTracks.includes(videoTrack.value)
      : currentVideoTracks.length === 0
    const audioMatches = audioTrack.value
      ? currentAudioTracks.includes(audioTrack.value)
      : currentAudioTracks.length === 0

    if (videoMatches && audioMatches) {
      return cachedMediaStream
    }

    currentVideoTracks.forEach(t => cachedMediaStream!.removeTrack(t))
    currentAudioTracks.forEach(t => cachedMediaStream!.removeTrack(t))

    if (videoTrack.value) cachedMediaStream.addTrack(videoTrack.value)
    if (audioTrack.value) cachedMediaStream.addTrack(audioTrack.value)

    return cachedMediaStream
  }

  cachedMediaStream = new MediaStream()
  if (videoTrack.value) {
    cachedMediaStream.addTrack(videoTrack.value)
  }
  if (audioTrack.value) {
    cachedMediaStream.addTrack(audioTrack.value)
  }
  return cachedMediaStream
}

export function useWebRTC() {
  return {
    state: state as Ref<WebRTCState>,
    videoTrack,
    audioTrack,
    stats,
    error,
    connectStage,
    connectionId,

    connect,
    disconnect,
    getMediaStream,

    isConnected: computed(() => state.value === 'connected'),
    isConnecting: computed(() => state.value === 'connecting'),
    hasVideo: computed(() => videoTrack.value !== null),
    hasAudio: computed(() => audioTrack.value !== null),
  }
}

if (typeof window !== 'undefined') {
  window.addEventListener('beforeunload', () => {
    disconnect()
  })
}
