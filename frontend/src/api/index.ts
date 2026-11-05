import { ApiError, deviceApiBase, captureDeviceRequest, request, rootRequest, type ApiRequestConfig, type FeatureFault } from './request'
import type { BitratePreset } from '@/types/generated'

export interface VideoCodecInfo {
  id: string
  name: string
  protocol: VideoTransport
  transports?: VideoTransport[]
  hardware: boolean
  backend: string | null
  available: boolean
}

export type VideoTransport = 'http' | 'webrtc' | 'webtransport'

export interface FeatureResourceMetadata {
  epoch?: string
  revision: number
  stale: boolean
}

export interface FirmwareRecord {
  id: string
  name: string | null
  version: string | null
  updateable: boolean | null
  status: string | null
}

export interface FirmwareStatus {
  available: boolean
  online: boolean
  backend?: string
  inventory: FirmwareRecord[]
  resource?: FeatureResourceMetadata
}

export interface SensorRecord {
  id: string
  name: string | null
  category: string | null
  reading: number | null
  unit: string | null
  status: string | null
}

export interface HealthStatus {
  available: boolean
  online: boolean
  backend?: string
  health: {
    overall_status: string | null
    sensors: SensorRecord[]
  } | null
  resource?: FeatureResourceMetadata
}

export const firmwareApi = {
  status: () => request<FirmwareStatus>('/feature/firmware/status'),
}

export const healthApi = {
  status: () => request<HealthStatus>('/feature/health/status'),
}

export interface EncoderBackendInfo {
  id: string
  name: string
  is_hardware: boolean
  supported_formats: string[]
}

export interface AvailableCodecsResponse {
  success: boolean
  backends: EncoderBackendInfo[]
  codecs: VideoCodecInfo[]
}

export interface StreamConstraintsResponse {
  success: boolean
  allowed_codecs: string[]
  http_modes: string[]
  locked_codec: string | null
  disallow_mjpeg: boolean
  sources: Record<string, boolean>
  reason: string
  current_mode: string
}

export interface WebTransportCodecInfo {
  id: string
  name: string
  webcodecs_codec: string | null
}

export interface WebTransportCapabilitiesResponse {
  success: boolean
  transport: 'webtransport'
  decoder: 'webcodecs'
  enabled: boolean
  configured: boolean
  url: string | null
  server_certificate_hashes?: Array<{ algorithm: 'sha-256'; value: number[] }> | null
  requires_secure_context: boolean
  codecs: WebTransportCodecInfo[]
  encoders: EncoderBackendInfo[]
  reason?: string
}

export interface WebTransportConnectResponse {
  success: boolean
  transport: 'webtransport'
  decoder: 'webcodecs'
  connection_id?: string
  codec?: string
  encoder?: string
  url?: string | null
  server_certificate_hashes?: Array<{ algorithm: 'sha-256'; value: number[] }> | null
  error?: string
}

export interface VideoEncoderSelfCheckResponse {
  current_hardware_encoder: string
  codecs: Array<{ id: string; name: string }>
  rows: Array<{
    resolution_id: string
    resolution_label: string
    width: number
    height: number
    cells: Array<{
      codec_id: string
      ok: boolean
      elapsed_ms?: number | null
    }>
  }>
}

export interface VideoColorDepthProfile {
  id: string
  depth_bpp: number
  variant: boolean
}

export interface VideoColorDepthCapabilitiesResponse {
  supported: boolean
  profiles: VideoColorDepthProfile[]
  active_profile_id: string | null
}

export function createWebTransportApi(send: typeof request) {
  return {
    getWebTransportCapabilities: () =>
      send<WebTransportCapabilitiesResponse>('/feature/video/webtransport/capabilities'),

    connectWebTransport: (params: { connection_id: string; codec: string; encoder?: string | null }) =>
      send<WebTransportConnectResponse>('/feature/video/webtransport/connect', {
        method: 'POST',
        body: JSON.stringify(params),
      }),
  }
}

export const streamApi = {
  status: () =>
    request<{
      streaming: boolean
      connected: boolean
      fault?: FeatureFault | null
      signal?: {
        state: 'unknown' | 'streaming' | 'no_signal' | 'device_lost'
        reason: string | null
      }
      active_instances?: number
      instances?: Array<{
        connection_id: string
        state: string
        online: boolean
        backend: string
        ref_count: number
        fault?: FeatureFault | null
      }>
    }>('/feature/video/status'),

  start: () =>
    request<{ success: boolean }>('/feature/video/start', { method: 'POST' }),

  stop: () =>
    request<{ success: boolean }>('/feature/video/stop', { method: 'POST' }),

  reset: () =>
    request<{ success: boolean; closed_webrtc_connections?: number; base_frame_replayed?: boolean }>('/feature/video/reset', { method: 'POST' }),

  getHttpStreamUrl: (clientId?: string) => {
    const base = `${deviceApiBase()}/feature/video/stream/http`
    return clientId ? `${base}?client_id=${clientId}` : base
  },

  getSnapshotUrl: () => `${deviceApiBase()}/snapshot`,

  getMode: () =>
    request<{ success: boolean; mode: string; transition_id?: string; switching?: boolean; message?: string }>('/feature/video/mode'),

  setMode: (mode: string) =>
    request<{ success: boolean; mode: string; transition_id?: string; switching?: boolean; message?: string }>('/feature/video/mode', {
      method: 'POST',
      body: JSON.stringify({ mode }),
    }),

  getCodecs: () =>
    request<AvailableCodecsResponse>('/feature/video/codecs'),

  getConstraints: () =>
    request<StreamConstraintsResponse>('/feature/video/constraints'),

  encoderSelfCheck: () =>
    request<VideoEncoderSelfCheckResponse>('/feature/video/encoder_self_check'),

  getColorDepthProfiles: () =>
    request<VideoColorDepthCapabilitiesResponse>('/feature/video/source-controls/color-depth'),

  setColorDepthProfile: (profile_id: string) =>
    request<{ success: boolean; profile_id: string }>('/feature/video/source-controls/color-depth', {
      method: 'POST',
      body: JSON.stringify({ profile_id }),
    }),

  trace: (connectionId?: string | null) =>
    request<{ success: boolean; trace: VideoTraceSnapshot }>(
      connectionId
        ? `/feature/video/trace?connection_id=${encodeURIComponent(connectionId)}`
        : '/feature/video/trace',
    ),

  ...createWebTransportApi(request),

  setBitratePreset: (bitrate_preset: BitratePreset) =>
    request<{ success: boolean; message?: string }>('/feature/video/bitrate', {
      method: 'POST',
      body: JSON.stringify({ bitrate_preset }),
    }),
}

export interface VideoTraceSnapshot {
  processor?: Record<string, unknown>
  mjpeg_encoder?: Record<string, unknown>
  backend?: Record<string, unknown>
  webrtc_sessions?: Array<{
    connection_id: string
    gstreamer?: Record<string, unknown>
  }>
  webtransport_sessions?: Array<{
    connection_id: string
    codec?: string
    encoder_backend?: string
    expected_caps?: Record<string, unknown>
    frames?: Record<string, unknown>
    encoder?: Record<string, unknown>
    stream?: Record<string, unknown>
  }>
}

export interface IceCandidate {
  candidate: string
  sdpMid?: string
  sdpMLineIndex?: number
  usernameFragment?: string
}

export interface IceServerConfig {
  urls: string[]
  username?: string
  credential?: string
}

interface WebRtcIceServersWireResponse {
  ice_servers?: IceServerConfig[]
  iceServers?: IceServerConfig[]
  mdns_mode?: string
}

interface WebRtcOfferWireResponse {
  connection_id?: string
  sdp?: string
  ice_candidates?: IceCandidate[]
  error?: string
}

interface WebRtcCommandResponse {
  ok?: boolean
  success?: boolean
  error?: string
}

function assertNoWebRtcError(response: { error?: string }, fallback: string): void {
  if (typeof response.error === 'string' && response.error.trim()) {
    throw new Error(response.error)
  }
  if (!response) {
    throw new Error(fallback)
  }
}

export function createWebrtcApi(send: typeof request) {
  return {
    getIceServers: async () => {
      const response = await send<WebRtcIceServersWireResponse>('/feature/video/webrtc/ice-servers')
      return {
        ice_servers: response.ice_servers ?? response.iceServers ?? [],
        mdns_mode: response.mdns_mode ?? 'disabled',
      }
    },

    offer: async (sdp: string, connectionId?: string, codec?: string, encoder?: string | null) => {
      const response = await send<WebRtcOfferWireResponse>('/feature/video/webrtc/offer', {
        method: 'POST',
        body: JSON.stringify({ sdp, connection_id: connectionId, codec, encoder }),
      })
      assertNoWebRtcError(response, 'WebRTC offer failed')
      if (!response.connection_id || !response.sdp) {
        throw new Error('WebRTC answer missing connection_id or sdp')
      }
      return {
        connection_id: response.connection_id,
        sdp: response.sdp,
        ice_candidates: response.ice_candidates ?? [],
      } satisfies {
        connection_id: string
        sdp: string
        ice_candidates: IceCandidate[]
      }
    },

    addIceCandidate: async (candidate: IceCandidate, connectionId: string) => {
      const response = await send<WebRtcCommandResponse>('/feature/video/webrtc/ice', {
        method: 'POST',
        body: JSON.stringify({ candidate, connection_id: connectionId }),
      })
      assertNoWebRtcError(response, 'WebRTC ICE candidate failed')
      return { success: response.success ?? response.ok ?? false }
    },

    close: async (connectionId: string) => {
      const response = await send<WebRtcCommandResponse>(`/feature/video/webrtc/close?connection_id=${encodeURIComponent(connectionId)}`, {
        method: 'POST',
      })
      assertNoWebRtcError(response, 'WebRTC close failed')
      return { success: response.success ?? response.ok ?? true }
    },
  }
}

export const webrtcApi = createWebrtcApi(request)

export { request } from './request'

function sortSerialDevices(devices: Array<{ path: string; name: string }>) {
  return [...devices].sort((a, b) => a.path.localeCompare(b.path))
}

export const configApi = {
  listDevices: async () => {
    const result = await request<{
      video: Array<{
        path: string
        name: string
        driver: string
        formats: Array<{
          format: string
          description: string
          resolutions: Array<{
            width: number
            height: number
            fps: number[]
          }>
        }>
        usb_bus: string | null
        has_signal: boolean
      }>
      serial: Array<{ path: string; name: string }>
      audio: Array<{
        name: string
        description: string
        is_hdmi: boolean
        usb_bus: string | null
      }>
      udc: Array<{ name: string }>
    }>('/devices')

    return {
      ...result,
      serial: sortSerialDevices(result.serial),
    }
  },
}

export {
  streamConfigApi,
  videoInputConfigApi,
  hidConfigApi,
  powerConfigApi,
  audioConfigApi,
  systemApi,
} from './config'

export type {
  WebConfig,
  WebConfigUpdate,
  VideoInputConfig,
  VideoInputConfigUpdate,
} from './config'

export type {
  BitratePreset,
} from '@/types/generated'

export interface PlatformCapabilities {
  mode: string
  video: { available: boolean }
  hid: { available: boolean }
  msd: { available: boolean }
  power: { available: boolean }
  audio: { available: boolean }
  [key: string]: unknown
}

export interface DeviceInfo {
  hostname: string
  cpu_model: string
  cpu_usage: number
  memory_used: number
  memory_total: number
  network_addresses: Array<{
    interface: string
    ip: string
  }>
}

export const authApi = {
  check: () =>
    rootRequest<{ authenticated: boolean; user: string | null }>('/auth/check'),

  login: (username: string, password: string) =>
    rootRequest<{ success: boolean; message?: string }>('/auth/login', {
      method: 'POST',
      body: JSON.stringify({ username, password }),
    }),

  logout: () =>
    rootRequest<{ success: boolean }>('/auth/logout', {
      method: 'POST',
    }),

  changePassword: (currentPassword: string, newPassword: string) =>
    rootRequest<{ success: boolean; message?: string }>('/auth/change-password', {
      method: 'POST',
      body: JSON.stringify({ current_password: currentPassword, new_password: newPassword }),
    }),

  changeUsername: (username: string, password: string) =>
    rootRequest<{ success: boolean; message?: string }>('/auth/change-username', {
      method: 'POST',
      body: JSON.stringify({ username, password }),
    }),
}

export interface MsdImage {
  id: string
  name: string
  size: number | null
  deletable?: boolean
  path?: string
  created_at?: number
  modified_at?: number
  digests?: Array<{
    algorithm: 'md5' | 'sha1' | 'sha256' | 'sha512'
    value: string
  }>
}

export type MsdMediaKind = 'optical' | 'floppy' | 'disk' | 'flash'

export type MsdStorageLocation = 'aggregator' | 'bmc' | 'remote'
export type MsdStorageOperation = 'select' | 'list' | 'upload' | 'delete'

export interface MsdStorageCapabilities {
  location: MsdStorageLocation
  operations: MsdStorageOperation[]
  media_types: MsdMediaKind[]
}

export interface MsdCapabilities {
  virtual_drive: boolean
  slots: MsdSlotCapabilities[]
  storage_locations: MsdStorageCapabilities[]
  unavailable_storage_locations: Array<{
    location: MsdStorageLocation
    reason: 'media_not_present' | 'not_initialized' | 'protocol_unavailable'
    hint_key: string
  }>
  target_management: MsdTargetManagementCapabilities
}

export interface MsdTargetManagementCapabilities {
  media_types: MsdMediaKind[]
  deletable: boolean
  limits?: Array<{ media_type: MsdMediaKind; max_targets: number }>
  max_targets: number | null
}

export interface MsdSlotCapabilities {
  id: string
  label?: string
  media_types: MsdMediaKind[]
  dynamic?: boolean
}

export type MsdMediaReference =
  | { location: 'aggregator'; image_id: string }
  | { location: 'bmc'; image_id: string }
  | { location: 'remote'; url: string }

export interface MsdAttachment {
  slot_id: string
  mode: 'image' | 'drive'
  media: MsdMediaReference | null
  media_type: MsdMediaKind | null
}

export interface FeatureResourceMeta {
  epoch?: string
  revision: number
  stale: boolean
}

export interface FeatureConnectionSnapshot {
  connection_id?: string
  state?: string
  online?: boolean
  fps?: number
  width?: number
  height?: number
  resolution?: [number, number]
  fault?: FeatureFault | null
}

export interface MsdStatus {
  available?: boolean
  capabilities: MsdCapabilities
  attachments: MsdAttachment[]
  occupancy?: {
    state: 'idle' | 'managed' | 'external'
    details_available: boolean
    active_sessions?: number
  }
  backend_status: Record<string, unknown> | null
  resource?: FeatureResourceMeta
  fault?: FeatureFault | null
  instances?: FeatureConnectionSnapshot[]
}

export interface MsdImageCatalog {
  location: MsdStorageLocation
  images: MsdImage[]
  resource?: FeatureResourceMeta
}

export interface DriveFile {
  name: string
  path: string
  size: number
  is_dir: boolean
  modified_at?: number
}

export interface MsdDownloadStatus {
  ok: boolean
  download_id: string
  filename: string
  bytes_downloaded: number
  total_bytes: number | null
  download_speed_bps: number
  status: 'downloading' | 'cancelling' | 'paused' | 'completed' | 'cancelled' | 'failed'
}

export interface MsdDriveInfo {
  capacity: number
  used: number
  free: number
  storage_base: string
  supported: boolean
}

export interface MsdTransferStatus {
  id: string
  filename: string
  location: MsdStorageLocation
  media_type: MsdMediaKind | null
  length: number
  offset: number
  state: 'uploading' | 'finalizing' | 'completed' | 'cancelled' | 'failed'
  image_id: string | null
  digests: NonNullable<MsdImage['digests']>
}

export interface HidStatus {
  available: boolean
  online: boolean
  backend?: string
  initialized?: boolean
  supports_absolute_mouse?: boolean
  keyboard_leds?: {
    capabilities?: { reports_state?: boolean }
    state?: { num_lock?: boolean; caps_lock?: boolean; scroll_lock?: boolean }
  }
  device?: string | null
  error?: string | null
  error_code?: string | null
  fault?: FeatureFault | null
  instances?: FeatureConnectionSnapshot[]
}

export const hidApi = {
  status: () => request<HidStatus>('/feature/hid/status'),
  keyboard: (type: string, key: string, modifier: number) =>
    request<{ success: boolean }>('/feature/hid/keyboard', { method: 'POST', body: JSON.stringify({ type, key, modifier }) }),
  mouse: (data: Record<string, unknown>) =>
    request<{ success: boolean }>('/feature/hid/mouse', { method: 'POST', body: JSON.stringify(data) }),
  reset: (config?: ApiRequestConfig) =>
    request<{ success: boolean }>('/feature/hid/reset', { method: 'POST' }, config),
  consumer: (usage: number) =>
    request<{ success: boolean }>('/feature/hid/consumer', { method: 'POST', body: JSON.stringify({ usage }) }),
  batch: (events: Array<{ action: string; params: Record<string, unknown> }>, delayMs?: number) =>
    request<{ success: boolean }>('/feature/hid/batch', {
      method: 'POST',
      body: JSON.stringify({ events, delay_ms: delayMs ?? 5 }),
    }),
}

export const msdApi = {
  status: (discover = false) => request<MsdStatus>(`/feature/msd/status${discover ? '?discover=true' : ''}`),
  listImageCatalog: (location: MsdStorageLocation) =>
    request<MsdImageCatalog>(
      `/feature/msd/images?location=${encodeURIComponent(location)}`,
    ),
  listImages: async (location: MsdStorageLocation) =>
    (await msdApi.listImageCatalog(location)).images,
  uploadImage: async (location: MsdStorageLocation, file: File, mediaType?: MsdMediaKind, onProgress?: (pct: number) => void): Promise<MsdImage> => {
    const { base, request: scopedRequest } = captureDeviceRequest()
    const storageKey = `msd-upload:${base}:${location}:${file.name}:${file.size}:${file.lastModified}`
    let transfer: MsdTransferStatus | null = null
    const retainedId = localStorage.getItem(storageKey)
    if (retainedId) {
      try {
        const retained = await scopedRequest<MsdTransferStatus>(`/feature/msd/images/uploads/${encodeURIComponent(retainedId)}`, {}, { toastOnError: false })
        if (retained.length === file.size && retained.location === location && !['cancelled', 'failed'].includes(retained.state)) {
          transfer = retained
        } else {
          localStorage.removeItem(storageKey)
        }
      } catch (error) {
        if (!(error instanceof ApiError) || error.status !== 404) throw error
        localStorage.removeItem(storageKey)
      }
    }
    if (!transfer) {
      transfer = await scopedRequest<MsdTransferStatus>('/feature/msd/images/uploads', {
        method: 'POST',
        body: JSON.stringify({ filename: file.name, location, media_type: mediaType, length: file.size }),
      })
      localStorage.setItem(storageKey, transfer.id)
    }
    const chunkBytes = 8 * 1024 * 1024
    while (transfer.offset < transfer.length) {
      const end = Math.min(transfer.offset + chunkBytes, transfer.length)
      transfer = await scopedRequest<MsdTransferStatus>(`/feature/msd/images/uploads/${encodeURIComponent(transfer.id)}`, {
        method: 'PATCH',
        headers: {
          'Content-Type': 'application/offset+octet-stream',
          'Upload-Offset': String(transfer.offset),
        },
        body: file.slice(transfer.offset, end),
      })
      onProgress?.(Math.round((transfer.offset / transfer.length) * 100))
    }
    if (transfer.state === 'finalizing') {
      transfer = await scopedRequest<MsdTransferStatus>(`/feature/msd/images/uploads/${encodeURIComponent(transfer.id)}`, { method: 'POST' })
    }
    if (transfer.state !== 'completed' || !transfer.image_id) {
      throw new Error('Upload did not reach the completed state')
    }
    localStorage.removeItem(storageKey)
    return {
      id: transfer.image_id,
      name: transfer.filename,
      size: transfer.length,
      deletable: true,
      digests: transfer.digests,
    }
  },
  connect: (
    slotId: string,
    mode: 'image' | 'drive',
    source?: MsdMediaReference,
    mediaType?: MsdMediaKind,
    mutationId?: string,
  ) =>
    request<{ success: boolean }>('/feature/msd/connect', {
      method: 'POST',
      body: JSON.stringify(mode === 'image'
          ? {
            slot_id: slotId,
            mode,
            source,
            mutation_id: mutationId,
            ...(mediaType ? { media_type: mediaType } : {}),
          }
        : { slot_id: slotId, mode, mutation_id: mutationId }),
    }),
  disconnect: (slotId: string, mutationId?: string) => request<{ success: boolean }>('/feature/msd/disconnect', {
    method: 'POST',
    body: JSON.stringify({ slot_id: slotId, mutation_id: mutationId }),
  }),
  createTarget: (mediaType: MsdMediaKind, label?: string, mutationId?: string) =>
    request<{ ok: boolean; target: MsdSlotCapabilities }>('/feature/msd/targets/create', {
      method: 'POST',
      body: JSON.stringify({ media_type: mediaType, ...(label ? { label } : {}), mutation_id: mutationId }),
    }),
  deleteTarget: (targetId: string, mutationId?: string) => request<{ success: boolean }>('/feature/msd/targets/delete', {
    method: 'POST',
    body: JSON.stringify({ target_id: targetId, mutation_id: mutationId }),
  }),
  deleteImage: (location: MsdStorageLocation, id: string) =>
    request<{ success: boolean }>('/feature/msd/images/delete', {
      method: 'POST',
      body: JSON.stringify({ location, image_id: id }),
    }),
  deleteDriveFile: (path: string) =>
    request<{ success: boolean }>('/feature/msd/drive/files/delete', { method: 'POST', body: JSON.stringify({ path }) }),
  driveInfo: () => request<MsdDriveInfo>('/feature/msd/drive'),
  initDrive: (sizeMb: number, mutationId = crypto.randomUUID()) =>
    request<{ success: boolean; mutation_id: string; revision: number }>('/feature/msd/drive/init', { method: 'POST', body: JSON.stringify({ size_mb: sizeMb, mutation_id: mutationId }) }),
  deleteDrive: () => request<{ success: boolean }>('/feature/msd/drive/delete', { method: 'POST' }),
  listDriveFiles: async (path: string) => {
    const response = await request<{ items: DriveFile[] }>(`/feature/msd/drive/files?path=${encodeURIComponent(path)}`)
    return response.items
  },
  uploadDriveFile: (file: File, path: string, onProgress?: (pct: number) => void) => {
    void onProgress
    const form = new FormData()
    form.append('file', file)
    form.append('path', path)
    return request<{ success: boolean }>('/feature/msd/drive/files/upload', { method: 'POST', body: form })
  },
  createDirectory: (path: string) =>
    request<{ success: boolean }>('/feature/msd/drive/mkdir', { method: 'POST', body: JSON.stringify({ path }) }),
  downloadDriveFile: (path: string) => `${deviceApiBase()}/feature/msd/drive/files/download?path=${encodeURIComponent(path)}`,
  downloadFromUrl: async (url: string, filename?: string, headers: Record<string, string> = {}) => {
    const scope = captureDeviceRequest()
    const result = await scope.request<Pick<MsdDownloadStatus, 'ok' | 'download_id' | 'filename' | 'status'>>(
      `/feature/msd/images/download?location=aggregator&url=${encodeURIComponent(url)}${filename ? `&filename=${encodeURIComponent(filename)}` : ''}`,
      { headers: { 'X-Bmc-Download-Headers': JSON.stringify(headers) } },
    )
    localStorage.setItem(scope.storageKey('msd-download'), result.download_id)
    return result
  },
  resumePendingDownload: async () => {
    const scope = captureDeviceRequest()
    const key = scope.storageKey('msd-download')
    const downloadId = localStorage.getItem(key)
    if (!downloadId) return null
    try {
      const status = await scope.request<MsdDownloadStatus>(`/feature/msd/images/download/status?download_id=${encodeURIComponent(downloadId)}`, {}, { toastOnError: false })
      if (status.status === 'paused' || status.status === 'failed') {
        await scope.request<Pick<MsdDownloadStatus, 'ok' | 'download_id' | 'filename' | 'status'>>(
          `/feature/msd/images/download?location=aggregator&resume_id=${encodeURIComponent(downloadId)}`,
          {}, { toastOnError: false },
        )
        return await scope.request<MsdDownloadStatus>(`/feature/msd/images/download/status?download_id=${encodeURIComponent(downloadId)}`, {}, { toastOnError: false })
      }
      if (status.status === 'completed' || status.status === 'cancelled') localStorage.removeItem(key)
      return status
    } catch (error) {
      if (!(error instanceof ApiError) || error.status !== 404) throw error
      localStorage.removeItem(key)
      return null
    }
  },
  clearPendingDownload: () => localStorage.removeItem(captureDeviceRequest().storageKey('msd-download')),
  downloadStatus: (downloadId: string) =>
    request<MsdDownloadStatus>(`/feature/msd/images/download/status?download_id=${encodeURIComponent(downloadId)}`),
  cancelDownload: async (downloadId: string) => {
    const scope = captureDeviceRequest()
    const result = await scope.request<{ ok: boolean }>('/feature/msd/images/download/cancel', { method: 'POST', body: JSON.stringify({ download_id: downloadId }) }, { toastOnError: false })
    if (result.ok !== true) throw new Error('The download cancellation was not confirmed; check its status before retrying')
    localStorage.removeItem(scope.storageKey('msd-download'))
    return result
  },
}

export const powerApi = {
  status: () => request<{
    available: boolean
    backend: string
    initialized: boolean
    power_status: 'on' | 'off' | 'unknown'
    capabilities: { actions: PowerAction[] }
    fault?: FeatureFault | null
    resource?: FeatureResourceMeta
  }>('/feature/power/status'),
  power: (action: string, mutationId?: string) =>
    request<{ success: boolean }>(`/feature/power/control`, { method: 'POST', body: JSON.stringify({ action, mutation_id: mutationId }) }),
}

export type PowerAction = 'on' | 'off' | 'reset' | 'power_cycle' | 'graceful_shutdown' | 'graceful_restart'

export type BootMode = 'once' | 'persistent'
export type BootProviderPhase = 'loading' | 'ready' | 'faulted' | 'unsupported'
export interface BootOptionEntry {
  id: string
  provider: string
  label?: string
  group?: string
  badges?: string[]
  supported_modes: BootMode[]
  enabled: boolean
  selectable: boolean
}
export interface BootProviderFault { code: string; message: string; retryable: boolean }
export interface BootProviderStatus { provider: string; phase: BootProviderPhase; complete: boolean; fault?: BootProviderFault }
export interface BootStatus {
  entries: BootOptionEntry[]
  selection?: { entry_id: string; mode: BootMode } | null
  providers?: BootProviderStatus[]
  resource?: FeatureResourceMeta
}
export const bootApi = {
  status: () => request<BootStatus>('/feature/boot/status', {}, { toastOnError: false }),
  set: (entryId: string, mode: BootMode, mutationId?: string) => request('/feature/boot/selection', { method: 'POST', body: JSON.stringify({ entry_id: entryId, mode, mutation_id: mutationId }) }),
}

export type IdentifyMode = 'off' | 'temporary' | 'indefinite'
export interface IdentifyStatus {
  identify: { mode: IdentifyMode }
  capabilities: { modes: IdentifyMode[]; max_temporary_seconds: number; writable: boolean }
  resource?: FeatureResourceMeta
}
export const identifyApi = {
  status: () => request<IdentifyStatus>('/feature/identify/status', {}, { toastOnError: false }),
  set: (mode: IdentifyMode, duration_seconds?: number, mutationId?: string) => request('/feature/identify/set', { method: 'POST', body: JSON.stringify({ mode, duration_seconds, mutation_id: mutationId }) }),
}

export const audioApi = {
  start: () => request<{ success: boolean }>('/feature/audio/start', { method: 'POST' }),
  stop: () => request<{ success: boolean }>('/feature/audio/stop', { method: 'POST' }),
}

export type { StreamConfigResponse, StreamConfigUpdate } from '@/types/featureConfig'
