import {
  bootApi,
  identifyApi,
  msdApi,
  powerApi,
  firmwareApi,
  healthApi,
  type BootStatus,
  type IdentifyStatus,
  type MsdStatus,
  type MsdImageCatalog,
  type FirmwareStatus,
  type HealthStatus,
} from '@/api'
import { createFeatureDataResource } from './resource'

function revisionOf(value: { resource?: { revision?: number } }): number {
  return value.resource?.revision ?? 0
}

function epochOf(value: { resource?: { epoch?: string } }): string | undefined {
  return value.resource?.epoch
}

export const powerStatusResource = createFeatureDataResource({
  feature: 'power',
  load: powerApi.status,
  revision: revisionOf,
  epoch: epochOf,
  pollInterval: 3000,
})

export const bootStatusResource = createFeatureDataResource<BootStatus>({
  feature: 'boot',
  resource: 'options',
  eventValue: (data, revision, current) => ({
    ...current,
    ...(data as Pick<BootStatus, 'entries' | 'selection' | 'providers'>),
    resource: { ...current?.resource, revision, stale: false },
  }),
  load: bootApi.status,
  revision: revisionOf,
  epoch: epochOf,
  pollInterval: value => value === null || value.providers?.some(provider => provider.phase === 'loading') ? 2000 : false,
})

export const identifyStatusResource = createFeatureDataResource<IdentifyStatus>({
  feature: 'identify',
  load: identifyApi.status,
  revision: revisionOf,
  epoch: epochOf,
  pollInterval: 3000,
})

export const firmwareStatusResource = createFeatureDataResource<FirmwareStatus>({
  feature: 'firmware',
  resource: 'inventory',
  load: firmwareApi.status,
  revision: revisionOf,
  epoch: epochOf,
  eventValue: (data, revision, current) => ({
    available: current?.available ?? true,
    online: current?.online ?? true,
    backend: current?.backend,
    inventory: (data as { inventory?: FirmwareStatus['inventory'] })?.inventory ?? [],
    resource: { revision, stale: false },
  }),
})

export const healthStatusResource = createFeatureDataResource<HealthStatus>({
  feature: 'health',
  resource: 'health',
  load: healthApi.status,
  revision: revisionOf,
  epoch: epochOf,
  pollInterval: 10000,
  eventValue: (data, revision, current) => ({
    available: current?.available ?? true,
    online: current?.online ?? true,
    backend: current?.backend,
    health: data as HealthStatus['health'],
    resource: { revision, stale: false },
  }),
})

export const msdStatusResource = createFeatureDataResource<MsdStatus>({
  feature: 'msd',
  load: () => msdApi.status(true),
  revision: revisionOf,
  epoch: epochOf,
  pollInterval: 1000,
})

function createMsdCatalogResource(location: 'aggregator' | 'bmc') {
  return createFeatureDataResource<MsdImageCatalog>({
    feature: 'msd',
    resource: `images.${location}`,
    load: () => msdApi.listImageCatalog(location),
    revision: revisionOf,
    epoch: epochOf,
    pollInterval: 3000,
    eventValue: (data, revision) => ({
      location,
      images: Array.isArray(data) ? data : [],
      resource: { revision, stale: false },
    }),
  })
}

export const msdCatalogResources = {
  aggregator: createMsdCatalogResource('aggregator'),
  bmc: createMsdCatalogResource('bmc'),
}
