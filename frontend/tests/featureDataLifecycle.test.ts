import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { bootEmptyState } from '../src/features/boot/presentation.ts'
import { FeatureLeaseRegistry, isNewerRevision, RevisionEpoch } from '../src/feature-data/core.ts'

test('feature leases are reference counted and preserve legacy owners', () => {
  const leases = new FeatureLeaseRegistry()
  leases.setLegacy(['video'])
  const releasePowerA = leases.acquire('power')
  const releasePowerB = leases.acquire('power')
  const releaseBoot = leases.acquire('boot')
  assert.deepEqual(leases.values(), ['boot', 'power', 'video'])

  releasePowerA()
  assert.deepEqual(leases.values(), ['boot', 'power', 'video'])
  releasePowerA()
  assert.deepEqual(leases.values(), ['boot', 'power', 'video'])
  releasePowerB()
  releaseBoot()
  assert.deepEqual(leases.values(), ['video'])
})

test('only strictly newer revisions replace a resource snapshot', () => {
  assert.equal(isNewerRevision(4, 5), true)
  assert.equal(isNewerRevision(5, 5), false)
  assert.equal(isNewerRevision(6, 5), false)
})

test('resource epochs accept replacements and reject delayed retired events', () => {
  const epochs = new RevisionEpoch()
  assert.equal(epochs.adopt('10'), 'changed')
  assert.equal(epochs.adopt('10'), 'same')
  assert.equal(epochs.adopt('12'), 'changed')
  assert.equal(epochs.adopt('11'), 'retired')
  assert.equal(epochs.adopt('10'), 'retired')
  epochs.reset()
  assert.equal(epochs.adopt('1'), 'changed')
})

test('resource scheduler deduplicates requests and never uses overlapping intervals', () => {
  const source = readFileSync(new URL('../src/feature-data/resource.ts', import.meta.url), 'utf8')
  assert.match(source, /if \(inFlight\) return inFlight/)
  assert.match(source, /window\.setTimeout/)
  assert.doesNotMatch(source, /setInterval/)
  assert.match(source, /websocket\.connected/)
  assert.match(source, /feature\.data_resync_required/)
  assert.match(source, /scope !== currentBmcIdFromLocation\(\)/)
  assert.match(source, /if \(current !== null\) ws\.connect\(\)/)
  assert.match(source, /if \(event\.reconnected\)/)
  assert.match(source, /RevisionEpoch/)
  assert.match(source, /adoptEpoch/)
  assert.match(source, /revision\.value = 0/)
  assert.match(source, /function stop\(\)[\s\S]*deviceScope\.reset\(\)[\s\S]*inFlight = null/)
})

test('global WebSocket is single-flight and device-route aware', () => {
  const source = readFileSync(new URL('../src/composables/useWebSocket.ts', import.meta.url), 'utf8')
  assert.match(source, /wsInstance\.readyState === WebSocket\.CONNECTING/)
  assert.match(source, /wsUrl === url/)
  assert.match(source, /if \(wsInstance !== socket\) return/)
  assert.match(source, /staleSocket\.close\(\)/)
})

test('feature panels do not own backend polling loops', () => {
  for (const file of ['PowerPopover.vue', 'BootPopover.vue', 'IdentifyPopover.vue']) {
    const source = readFileSync(new URL(`../src/components/${file}`, import.meta.url), 'utf8')
    assert.doesNotMatch(source, /setInterval/)
  }
})

test('boot discovery retries a missing initial snapshot and stops at a terminal state', () => {
  const source = readFileSync(new URL('../src/feature-data/resources.ts', import.meta.url), 'utf8')
  assert.match(source, /value === null \|\| value\.providers\?\.some\(provider => provider\.phase === 'loading'\)/)
})

test('boot uses one provider-driven entry selector without entry-family branches', () => {
  const source = readFileSync(new URL('../src/components/BootPopover.vue', import.meta.url), 'utf8')
  assert.match(source, /const entries = computed<BootOptionEntry\[\]>/)
  assert.match(source, /selectedEntry\.value\?\.supported_modes/)
  assert.match(source, /bootApi\.set\(entryId\.value, mode\.value, mutationId\)/)
  assert.doesNotMatch(source, /category|firmware|TARGET_CHOICE_PREFIX|\befi\b/i)
})

test('boot empty state follows provider lifecycle instead of assuming device entries exist', () => {
  assert.equal(bootEmptyState({
    entries: [{ id: 'provider:entry', provider: 'provider', supported_modes: ['once'], enabled: true, selectable: true }],
    providers: [],
  }), null)
  assert.equal(bootEmptyState({
    entries: [],
    providers: [{ provider: 'provider', phase: 'loading', complete: false }],
  }), 'discovering')
  assert.equal(bootEmptyState({
    entries: [],
    providers: [{ provider: 'provider', phase: 'ready', complete: true }],
  }), 'empty')
  assert.equal(bootEmptyState({ entries: [] }), 'unavailable')
})

test('virtual media download status polling is non-overlapping and cancellable', () => {
  const source = readFileSync(new URL('../src/features/msd/MsdDialog.vue', import.meta.url), 'utf8')
  assert.doesNotMatch(source, /setInterval/)
  assert.match(source, /downloadPollGeneration/)
  assert.match(source, /refreshDownloadStatus\(\)\.finally/)
  assert.match(source, /window\.clearTimeout\(downloadPollTimer\)/)
})
