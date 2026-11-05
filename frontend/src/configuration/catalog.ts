import { rootRequest, rootRequestMeta, type ApiResourceResponse } from '@/api/request'

export type ConfigValue = string | number | boolean

export interface ConfigOption {
  value: ConfigValue
  label_key: string
}

export type ConfigOptionSource =
  | { source: 'static'; options: ConfigOption[] }
  | { source: 'registered_resource'; resource_id: string }

export type ConfigField = {
  id: string
  title_key: string
  description_key?: string
  group: 'primary' | 'advanced'
  required: boolean
  default?: ConfigValue
} & (
  | { kind: 'text'; trim: boolean }
  | { kind: 'secret' }
  | { kind: 'integer'; min?: number; max?: number; step?: number }
  | { kind: 'decimal'; min?: number; max?: number; step?: number }
  | { kind: 'boolean' }
  | { kind: 'selection'; options: ConfigOptionSource; allow_custom: boolean }
)

export interface FeatureDescriptor {
  feature_id: string
  title_key: string
  description_key?: string
  binding: { kind: 'single' } | { kind: 'provider_set'; min: number; max?: number }
  allow_disabled: boolean
  feature_fields: ConfigField[]
}

export interface BackendDescriptor {
  feature_id: string
  backend_id: string
  display_name: string
  availability: { state: 'available' } | { state: 'unavailable'; reason_key: string }
  fields: ConfigField[]
  defaults: Record<string, ConfigValue>
}

export interface ConfigurationCatalog {
  version: string
  features: FeatureDescriptor[]
  backends: BackendDescriptor[]
  option_resources: Array<{ resource_id: string; title_key: string }>
  creation_profiles: Array<{ profile_id: string; title_key: string; description_key: string }>
}

export interface DiscoveredOption {
  value: ConfigValue
  label: string
}

export interface OptionSnapshot {
  resource_id: string
  epoch: string
  revision: number
  state: { status: 'ready' } | { status: 'failed'; code: string; message: string }
  options: DiscoveredOption[]
}

export interface BackendAssignment {
  backend_id: string
  params: Record<string, ConfigValue>
  secrets: Record<string, { configured: boolean }>
}

export type FeatureAssignment =
  | { kind: 'disabled' }
  | { kind: 'single'; backend: BackendAssignment }
  | { kind: 'provider_set'; providers: Array<BackendAssignment & { id: string }> }

export interface DeviceConfiguration {
  id: string
  name: string
  endpoint: { host: string; port: number }
  credentials: { username: string; password: { configured: boolean } }
  assignments: Record<string, FeatureAssignment>
  version: string
}

export interface DeviceConfigurationMutation {
  id?: string
  name?: string
  endpoint?: { host?: string; port?: number }
  credentials?: {
    username?: string
    password?: { operation: 'keep' } | { operation: 'replace'; value: string } | { operation: 'clear' }
  }
  assignments?: Record<string, FeatureAssignmentMutation>
}

export type FeatureAssignmentMutation =
  | { kind: 'disabled' }
  | { kind: 'single'; backend: BackendAssignmentMutation }
  | { kind: 'provider_set'; providers: Array<BackendAssignmentMutation & { id: string }> }

export interface BackendAssignmentMutation {
  backend_id: string
  params: Record<string, ConfigValue>
  secrets: Record<string, { operation: 'keep' } | { operation: 'replace'; value: string } | { operation: 'clear' }>
}

export interface DeviceCreationDraftRequest {
  id: string
  name: string
  host: string
  port?: number
  username?: string
}

export interface DeviceCreationDraft {
  draft: DeviceConfigurationMutation
}

export const configurationApi = {
  catalog: (): Promise<ApiResourceResponse<ConfigurationCatalog>> => rootRequestMeta('/configuration/catalog'),
  options: (resourceId: string): Promise<ApiResourceResponse<OptionSnapshot>> =>
    rootRequestMeta(`/configuration/options/${encodeURIComponent(resourceId)}`),
  devices: (): Promise<ApiResourceResponse<{ devices: DeviceConfiguration[] }>> => rootRequestMeta('/devices'),
  device: (id: string): Promise<ApiResourceResponse<{ device: DeviceConfiguration }>> =>
    rootRequestMeta(`/devices/${encodeURIComponent(id)}`),
  create: (mutation: DeviceConfigurationMutation, mutationId = crypto.randomUUID()) =>
    rootRequestMeta<{ success: boolean; mutation_id: string; revision: number; device: DeviceConfiguration }>('/devices', {
      method: 'POST',
      headers: { 'X-Mutation-Id': mutationId },
      body: JSON.stringify(mutation),
    }),
  update: (id: string, version: string, mutation: DeviceConfigurationMutation, mutationId = crypto.randomUUID()) =>
    rootRequestMeta<{ success: boolean; mutation_id: string; revision: number; device: DeviceConfiguration }>(
      `/devices/${encodeURIComponent(id)}`,
      {
        method: 'PATCH',
        headers: { 'If-Match': version, 'X-Mutation-Id': mutationId },
        body: JSON.stringify(mutation),
      },
    ),
  remove: (id: string, version: string, mutationId = crypto.randomUUID()) =>
    rootRequestMeta<{ success: boolean; mutation_id: string; revision: number }>(
      `/devices/${encodeURIComponent(id)}`,
      {
        method: 'DELETE',
        headers: { 'If-Match': version, 'X-Mutation-Id': mutationId },
      },
    ),
  creationDraft: (profileId: string, request: DeviceCreationDraftRequest) =>
    rootRequest<DeviceCreationDraft>(`/configuration/device-drafts/${encodeURIComponent(profileId)}`, {
      method: 'POST',
      body: JSON.stringify(request),
    }),
}

export function backendFor(catalog: ConfigurationCatalog, featureId: string, backendId: string) {
  return catalog.backends.find(backend => backend.feature_id === featureId && backend.backend_id === backendId)
}

export function featureTitle(feature: FeatureDescriptor): string {
  return humanize(feature.title_key.split('.').pop() ?? feature.feature_id)
}

export function humanize(value: string): string {
  return value.replace(/[_-]+/g, ' ').replace(/\b\w/g, character => character.toUpperCase())
}
