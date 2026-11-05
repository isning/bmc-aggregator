import { toast } from 'vue-sonner'
import i18n from '@/i18n'

// Toast debounce mechanism - prevent toast spam (5 seconds)
const toastDebounceMap = new Map<string, number>()
const TOAST_DEBOUNCE_TIME = 5000

function shouldShowToast(key: string): boolean {
  const now = Date.now()
  const lastToastTime = toastDebounceMap.get(key)

  if (!lastToastTime || now - lastToastTime >= TOAST_DEBOUNCE_TIME) {
    toastDebounceMap.set(key, now)
    return true
  }

  return false
}

function t(key: string, params?: Record<string, unknown>): string {
  return String(i18n.global.t(key, params ?? {}))
}

export class ApiError extends Error {
  status: number
  fault: FeatureFault | null

  constructor(status: number, message: string, fault: FeatureFault | null = null) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.fault = fault
  }
}

export interface FeatureFault {
  domain: 'lifecycle' | 'transport' | 'protocol' | 'operation' | 'storage' | 'decode' | 'encoder' | 'browser_transport'
  code: string
  message: string
  retryable: boolean
  scope: 'feature' | 'operation' | 'child'
  operation?: string
  kind?: string
  id?: string
  occurred_at_ms: number
  count: number
}

export interface ApiRequestConfig {
  /**
   * Enable toast notifications on errors.
   * Defaults to true to match existing behavior in api/index.ts.
   */
  toastOnError?: boolean
  /**
   * Toast debounce key. Defaults to `error_${endpoint}`.
   */
  toastKey?: string
}

export interface ApiResourceResponse<T> {
  data: T
  etag: string | null
}

export function currentBmcIdFromLocation(): string | null {
  const match = window.location.pathname.match(/^\/devices\/([^/?#]+)/)
  const rawId = match?.[1]
  if (!rawId) return null

  try {
    return decodeURIComponent(rawId)
  } catch {
    return rawId
  }
}

export function deviceApiBase(bmcId = currentBmcIdFromLocation()): string {
  if (!bmcId) {
    throw new ApiError(0, 'No BMC device is selected in the current route')
  }
  return `/bmc/${encodeURIComponent(bmcId)}/api`
}

/** Pin every stage and recovery record of an operation to its original device. */
export function captureDeviceRequest() {
  const base = deviceApiBase()
  return {
    base,
    storageKey: (name: string) => `${name}:${base}`,
    request: <T>(endpoint: string, options: RequestInit = {}, config?: ApiRequestConfig) =>
      requestWithBase<T>(base, endpoint, options, config),
  }
}

function getToastKey(endpoint: string, config?: ApiRequestConfig): string {
  return config?.toastKey ?? `error_${endpoint}`
}

function getErrorMessage(data: unknown, fallback: string): string {
  if (typeof data === 'string' && data.trim()) return data.trim()
  if (data && typeof data === 'object') {
    const record = data as Record<string, unknown>
    const fault = record.fault as Record<string, unknown> | null | undefined
    if (fault && typeof fault.message === 'string' && fault.message.trim()) return fault.message
    const message = record.message
    if (typeof message === 'string' && message.trim()) return message
  }
  return fallback
}

function getFeatureFault(data: unknown): FeatureFault | null {
  if (!data || typeof data !== 'object') return null
  const fault = (data as Record<string, unknown>).fault
  if (!fault || typeof fault !== 'object') return null
  const record = fault as Record<string, unknown>
  if (typeof record.code !== 'string' || typeof record.message !== 'string') return null
  return fault as FeatureFault
}

export async function request<T>(
  endpoint: string,
  options: RequestInit = {},
  config: ApiRequestConfig = {}
): Promise<T> {
  return requestWithBase<T>(deviceApiBase(), endpoint, options, config)
}

export async function requestMeta<T>(
  endpoint: string,
  options: RequestInit = {},
  config: ApiRequestConfig = {},
): Promise<ApiResourceResponse<T>> {
  return requestWithBaseMeta<T>(deviceApiBase(), endpoint, options, config)
}

export async function requestWithBase<T>(
  base: string,
  endpoint: string,
  options: RequestInit = {},
  config: ApiRequestConfig = {}
): Promise<T> {
  const response = await requestWithBaseMeta<T>(base, endpoint, options, config)
  return response.data
}

export async function requestWithBaseMeta<T>(
  base: string,
  endpoint: string,
  options: RequestInit = {},
  config: ApiRequestConfig = {},
): Promise<ApiResourceResponse<T>> {
  const url = `${base}${endpoint}`
  const toastOnError = config.toastOnError !== false
  const toastKey = getToastKey(endpoint, config)

  try {
    const headers = new Headers(options.headers)
    if (!(options.body instanceof FormData) && !headers.has('Content-Type')) {
      headers.set('Content-Type', 'application/json')
    }
    const response = await fetch(url, {
      ...options,
      headers,
      credentials: 'include',
    })

    const contentType = response.headers.get('content-type')?.toLowerCase() ?? ''
    const raw = response.status === 204 ? '' : await response.text()
    const hasBody = raw.trim().length > 0
    let data: unknown = null
    if (hasBody) {
      if (contentType.includes('json')) {
        try {
          data = JSON.parse(raw) as unknown
        } catch {
          data = null
        }
      } else {
        data = raw
      }
    }

    // Handle HTTP errors (in case backend returns non-2xx)
    if (!response.ok) {
      const message = getErrorMessage(data, `HTTP ${response.status}`)
      const fault = getFeatureFault(data)
      const normalized = message.toLowerCase()
      const isNotAuthenticated = normalized.includes('not authenticated')
      const isSessionExpired = normalized.includes('session expired')
      const isLoggedInElsewhere = normalized.includes('logged in elsewhere')
      const isAuthIssue = response.status === 401 && (isNotAuthenticated || isSessionExpired || isLoggedInElsewhere)
      if (toastOnError && shouldShowToast(toastKey) && !isAuthIssue) {
        toast.error(t('api.operationFailed'), {
          description: message,
          duration: 4000,
        })
      }
      throw new ApiError(response.status, message, fault)
    }

    // Handle backend "success=false" convention (even when HTTP is 200)
    if (data && typeof data === 'object' && typeof (data as Record<string, unknown>).success === 'boolean' && !(data as Record<string, unknown>).success) {
      const message = getErrorMessage(data, t('api.operationFailedDesc'))
      const fault = getFeatureFault(data)

      if (toastOnError && shouldShowToast(toastKey)) {
        toast.error(t('api.operationFailed'), {
          description: message,
          duration: 4000,
        })
      }

      throw new ApiError(response.status, message, fault)
    }

    // Empty operation responses are valid; populated API responses must be JSON.
    if (!hasBody) {
      return {
        data: undefined as T,
        etag: response.headers.get('etag'),
      }
    }

    if (data === null || !contentType.includes('json')) {
      const message = t('api.parseResponseFailed')
      if (toastOnError && shouldShowToast(toastKey)) {
        toast.error(t('api.operationFailed'), {
          description: message,
          duration: 4000,
        })
      }
      throw new ApiError(response.status, message)
    }

    return {
      data: data as T,
      etag: response.headers.get('etag'),
    }
  } catch (error) {
    if (error instanceof ApiError) throw error

    if (options.signal?.aborted || (error instanceof DOMException && error.name === 'AbortError')) {
      throw new ApiError(0, t('api.requestAborted'))
    }

    if (toastOnError && shouldShowToast('network_error')) {
      toast.error(t('api.networkError'), {
        description: t('api.networkErrorDesc'),
        duration: 4000,
      })
    }

    throw new ApiError(0, t('api.networkError'))
  }
}

export async function rootRequest<T>(
  endpoint: string,
  options: RequestInit = {},
  config: ApiRequestConfig = {}
): Promise<T> {
  const normalized = endpoint.startsWith('/') ? endpoint : `/${endpoint}`
  return requestWithBase<T>('/api', normalized, options, config)
}

export async function rootRequestMeta<T>(
  endpoint: string,
  options: RequestInit = {},
  config: ApiRequestConfig = {},
): Promise<ApiResourceResponse<T>> {
  const normalized = endpoint.startsWith('/') ? endpoint : `/${endpoint}`
  return requestWithBaseMeta<T>('/api', normalized, options, config)
}
