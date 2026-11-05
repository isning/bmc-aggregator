import type { HidConfig, HidConfigUpdate, AudioConfig, AudioConfigUpdate, StreamConfigResponse, StreamConfigUpdate } from '@/types/featureConfig'
import type {
  WebConfigResponse,
  WebConfigUpdate,
} from '@/types/generated'

import { deviceApiBase, requestWithBaseMeta, request, requestMeta, rootRequest, type FeatureFault } from './request'

export interface VideoInputConfig {
  device: string | null
  format: string | null
  width: number
  height: number
  fps: number
  quality: number
}

export interface VideoInputConfigUpdate {
  device?: string | null
  format?: string | null
  width?: number | null
  height?: number | null
  fps?: number | null
  quality?: number | null
}

export const streamConfigApi = {
  get: () => requestMeta<StreamConfigResponse>('/feature/video/config'),

  update: (version: string, config: StreamConfigUpdate) =>
    requestMeta<StreamConfigResponse>('/feature/video/config', {
      method: 'PATCH',
      headers: { 'If-Match': version, 'X-Mutation-Id': crypto.randomUUID() },
      body: JSON.stringify(config),
    }),
}

export const videoInputConfigApi = {
  get: (bmcId?: string | null) => requestWithBaseMeta<VideoInputConfig>(deviceApiBase(bmcId), '/feature/video/input-config'),

  update: (version: string, config: VideoInputConfigUpdate, bmcId?: string | null) =>
    requestWithBaseMeta<VideoInputConfig>(deviceApiBase(bmcId), '/feature/video/input-config', {
      method: 'PATCH',
      headers: { 'If-Match': version, 'X-Mutation-Id': crypto.randomUUID() },
      body: JSON.stringify(config),
    }),
}

export const hidConfigApi = {
  get: () => requestMeta<HidConfig>('/feature/hid/config'),

  update: (version: string, config: HidConfigUpdate) =>
    requestMeta<HidConfig>('/feature/hid/config', {
      method: 'PATCH',
      headers: { 'If-Match': version, 'X-Mutation-Id': crypto.randomUUID() },
      body: JSON.stringify(config),
    }),
}


export interface WolHistoryEntry {
  mac_address: string
  updated_at: number
}

export interface WolHistoryResponse {
  history: WolHistoryEntry[]
}

export const powerConfigApi = {
  sendWol: (macAddress: string) =>
    request<{ success: boolean; message?: string }>('/feature/power/wol', {
      method: 'POST',
      body: JSON.stringify({ mac_address: macAddress }),
    }),

  getWolHistory: (limit = 5) =>
    request<WolHistoryResponse>(`/feature/power/wol/history?limit=${Math.max(1, Math.min(50, limit))}`),
}

export const audioConfigApi = {
  get: () => requestMeta<AudioConfig>('/feature/audio/config'),

  update: (version: string, config: AudioConfigUpdate) =>
    requestMeta<AudioConfig>('/feature/audio/config', {
      method: 'PATCH',
      headers: { 'If-Match': version, 'X-Mutation-Id': crypto.randomUUID() },
      body: JSON.stringify(config),
    }),
}

export type WebConfig = WebConfigResponse

export type { WebConfigUpdate }


export interface DeviceFeatureInfo {
  id: string
  backend: string
  instance_id: string
  state: string
  healthy: boolean
  ref_count: number
}

export interface DeviceFeaturesResponse {
  features: DeviceFeatureInfo[]
  [key: string]: unknown
}

export interface FeatureStatusInfo {
  id: string
  available: boolean
  online: boolean
  state: string
  backend: string
  connection_id: string
  ref_count: number
  fault?: FeatureFault | null
  active_instances?: number
  instances?: Array<{
    connection_id: string
    state: string
    online: boolean
    backend: string
    ref_count: number
    fault?: FeatureFault | null
  }>
}

export interface FeatureStatusesResponse {
  features: FeatureStatusInfo[]
}

export type RequirementId = 'firmware_inventory_collection'
export type RequirementCategory = 'permission' | 'configuration' | 'license' | 'service' | 'inventory' | 'hardware' | 'session'
export type RequirementState = 'satisfied' | 'action_required' | 'pending' | 'unavailable' | 'unknown'
export type RequirementImpact = 'blocking' | 'degraded' | 'informational'
export type RemediationAction = 'enable_inventory_collection'
export type RestartRequirement = 'none' | 'next_host_post' | 'host_restart' | 'bmc_restart'

export interface BackendRequirement {
  feature: string
  backend: string
  id: RequirementId
  category: RequirementCategory
  state: RequirementState
  impact: RequirementImpact
  title_key: string
  message_key: string
  remediation?: {
    action: RemediationAction
    confirmation: 'none' | 'required' | 'destructive'
    restart_requirement: RestartRequirement
  }
}

export interface RequirementsResponse {
  requirements: BackendRequirement[]
  summary: {
    total: number
    action_required: number
    pending: number
    unavailable: number
    unknown: number
  }
}

export const systemApi = {
  info: () =>
    rootRequest<{ version: string; mode: string; arch: string; os: string; memory: string; cpu: string }>('/system/info'),
  setupStatus: () =>
    rootRequest<{
      setup_required: boolean
      platform: import('@/api').PlatformCapabilities
    }>('/setup/status'),
  setup: (data: Record<string, unknown>) =>
    rootRequest<{ success: boolean; message?: string }>('/setup', {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  features: () =>
    request<DeviceFeaturesResponse>('/features'),
  featureStatuses: () =>
    request<FeatureStatusesResponse>('/features/status'),
  requirements: () =>
    request<RequirementsResponse>('/requirements', {}, { toastOnError: false }),
  remediateRequirement: (feature: string, requirementId: RequirementId, action: RemediationAction) =>
    request<{ success: boolean; result: { changed: boolean; state: RequirementState; restart_requirement: RestartRequirement } }>(
      '/requirements/remediate',
      {
        method: 'POST',
        body: JSON.stringify({ feature, requirement_id: requirementId, action }),
      },
    ),
}
