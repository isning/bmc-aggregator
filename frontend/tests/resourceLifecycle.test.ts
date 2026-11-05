import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import test from 'node:test'
import ts from 'typescript'
import { effectScope, ref } from 'vue'
import { createPinia, setActivePinia } from 'pinia'

const vueUrl = pathToFileURL(resolve('node_modules/vue/dist/vue.runtime.esm-bundler.js')).href
const piniaUrl = pathToFileURL(resolve('node_modules/pinia/dist/pinia.mjs')).href
const moduleUrl = (source: string): string => {
  if (source.includes("from '@/api/deviceScope'")) {
    source = source.replace("from '@/api/deviceScope'", `from '${moduleUrl(readFileSync('src/api/deviceScope.ts', 'utf8'))}'`)
  }
  return 'data:text/javascript;base64,' + Buffer.from(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  }).outputText).toString('base64')
}

class FixtureApiError extends Error {
  readonly status: number
  constructor(status: number) { super(`HTTP ${status}`); this.status = status }
}

function msdOperationSource(name: string): string {
  const ast = ts.createSourceFile('index.ts', readFileSync('src/api/index.ts', 'utf8'), ts.ScriptTarget.Latest, true)
  for (const statement of ast.statements) {
    if (!ts.isVariableStatement(statement)) continue
    for (const declaration of statement.declarationList.declarations) {
      if (declaration.name.getText(ast) !== 'msdApi' || !declaration.initializer || !ts.isObjectLiteralExpression(declaration.initializer)) continue
      const member = declaration.initializer.properties.find(property => property.name?.getText(ast) === name)
      if (member && ts.isPropertyAssignment(member)) return member.initializer.getText(ast)
    }
  }
  throw new Error(`Missing MSD operation: ${name}`)
}

const requestAst = ts.createSourceFile('request.ts', readFileSync('src/api/request.ts', 'utf8'), ts.ScriptTarget.Latest, true)
const scopeFactory = requestAst.statements.find(statement => ts.isFunctionDeclaration(statement) && statement.name?.text === 'captureDeviceRequest')!.getText(requestAst)

// Execute production modules with only HTTP and socket boundaries replaced.
test('an inactive consumer never releases another consumer subscription', async () => {
  const core = moduleUrl(readFileSync('src/feature-data/core.ts', 'utf8'))
  const source = readFileSync('src/feature-data/resource.ts', 'utf8')
    .replace("from 'vue'", `from '${vueUrl}'`)
    .replace("import { currentBmcIdFromLocation } from '@/api/request'", "const currentBmcIdFromLocation = () => 'lab'")
    .replace("import { useWebSocket } from '@/composables/useWebSocket'", 'const useWebSocket = () => ({on(){},off(){},connect(){},acquireFeature(){return () => {}}})')
    .replace("from './core'", `from '${core}'`)
  const { createFeatureDataResource, useFeatureDataSubscription } = await import(moduleUrl(source))
  const resource = createFeatureDataResource({ feature: 'power', load: async () => ({ revision: 1 }), revision: (value: { revision: number }) => value.revision })
  const owner = effectScope()
  owner.run(() => useFeatureDataSubscription(resource, ref(true)))
  const inactive = effectScope()
  inactive.run(() => useFeatureDataSubscription(resource, ref(false)))
  assert.equal(resource.subscribers.value, 1)
  inactive.stop()
  assert.equal(resource.subscribers.value, 1)
  owner.stop()
  assert.equal(resource.subscribers.value, 0)
})

for (const feature of ['hid', 'audio', 'video']) {
  test(`${feature} ignores requests completed after resetting device scope`, async () => {
    setActivePinia(createPinia())
    let complete!: (value: unknown) => void
    const api = { get: () => new Promise(resolve => { complete = resolve }) }
    const key = `__lifecycle_${feature}`
    Object.assign(globalThis, { [key]: api })
    const apiNames = feature === 'video' ? 'streamConfigApi, videoInputConfigApi' : `${feature}ConfigApi`
    const source = readFileSync(`src/stores/${feature}Config.ts`, 'utf8')
      .replace("import { currentBmcIdFromLocation } from '@/api/request'", "const currentBmcIdFromLocation = () => 'lab'")
      .replace("from 'vue'", `from '${vueUrl}'`)
      .replace("from 'pinia'", `from '${piniaUrl}'`)
      .replace(`import { ${apiNames} } from '@/api'`, `const { ${apiNames} } = { ${apiNames.split(', ').map(name => `${name}: globalThis.${key}`).join(', ')} }`)
    const resolvedSource = source.replace("from '@/configuration/requestOrder'", `from '${moduleUrl(readFileSync('src/configuration/requestOrder.ts', 'utf8'))}'`)
    const module = await import(moduleUrl(resolvedSource))
    const title = feature[0]!.toUpperCase() + feature.slice(1)
    const store = module[`use${title}ConfigStore`]()
    const field = feature
    const method = title
    const pending = store[`refresh${method}`]()
    store.reset()
    complete({ data: { oldDevice: true }, etag: 'old-version' })
    await pending
    assert.equal(store[field], null)
    assert.equal(store[`${field}Etag`], null)
    assert.equal(store[`${field}Loading`], false)
    Reflect.deleteProperty(globalThis, key)
  })
}

for (const feature of ['hid', 'audio', 'video', 'stream']) {
  test(`${feature} recovers from a missing version and ignores an old GET after PATCH`, async () => {
    setActivePinia(createPinia())
    let complete!: (value: unknown) => void
    let reads = 0
    const api = {
      get: () => ++reads === 1 ? Promise.resolve({ data: { version: 1 }, etag: 'v1' }) : new Promise(resolve => { complete = resolve }),
      update: async () => ({ data: { version: 2 }, etag: 'v2' }),
    }
    const key = `__ordering_${feature}`
    Object.assign(globalThis, { [key]: api })
    const apiNames = (feature === 'video' || feature === 'stream') ? 'streamConfigApi, videoInputConfigApi' : `${feature}ConfigApi`
    const source = readFileSync(`src/stores/${feature === 'stream' ? 'video' : feature}Config.ts`, 'utf8')
      .replace("import { currentBmcIdFromLocation } from '@/api/request'", "const currentBmcIdFromLocation = () => 'lab'")
      .replace("from '@/configuration/requestOrder'", `from '${moduleUrl(readFileSync('src/configuration/requestOrder.ts', 'utf8'))}'`)
      .replace("from 'vue'", `from '${vueUrl}'`)
      .replace("from 'pinia'", `from '${piniaUrl}'`)
      .replace(`import { ${apiNames} } from '@/api'`, `const { ${apiNames} } = { ${apiNames.split(', ').map(name => `${name}: globalThis.${key}`).join(', ')} }`)
    const module = await import(moduleUrl(source))
    const title = feature[0]!.toUpperCase() + feature.slice(1)
    const store = module[`use${feature === 'stream' ? 'Video' : title}ConfigStore`]()
    const field = feature
    const method = title
    await assert.rejects(store[`update${method}`]({}), /version is unavailable/)
    await store[`refresh${method}`]()
    const pending = store[`refresh${method}`]()
    const save = store[`update${method}`]({})
    await assert.rejects(store[`update${method}`]({}), /already in progress/)
    await save
    complete({ data: { version: 1 }, etag: 'v1' })
    await pending
    assert.deepEqual(store[field], { version: 2 })
    assert.equal(store[`${field}Etag`], 'v2')
    Reflect.deleteProperty(globalThis, key)
  })
}

test('Boot options events update a ready resource without polling', async () => {
  const handlers: Record<string, (event: unknown) => void> = {}
  Object.assign(globalThis, { window: { setTimeout, clearTimeout }, __bootOrdering: {
    ws: { on(name: string, handler: (event: unknown) => void) { handlers[name] = handler }, off() {}, connect() {}, acquireFeature() { return () => {} } },
    api: { status: async () => ({ entries: [{ id: 'old' }], providers: [{ phase: 'ready' }], resource: { revision: 1, epoch: '1' } }) },
  } })
  const core = moduleUrl(readFileSync('src/feature-data/core.ts', 'utf8'))
  const resource = moduleUrl(readFileSync('src/feature-data/resource.ts', 'utf8')
    .replace("from 'vue'", `from '${vueUrl}'`)
    .replace("import { currentBmcIdFromLocation } from '@/api/request'", "const currentBmcIdFromLocation = () => 'lab'")
    .replace("import { useWebSocket } from '@/composables/useWebSocket'", 'const useWebSocket = () => globalThis.__bootOrdering.ws')
    .replace("from './core'", `from '${core}'`))
  const source = readFileSync('src/feature-data/resources.ts', 'utf8')
    .replace(/^import\s*\{[\s\S]*?\}\s*from '@\/api'/, 'const bootApi=globalThis.__bootOrdering.api, identifyApi=bootApi, msdApi=bootApi, powerApi=bootApi, firmwareApi=bootApi, healthApi=bootApi')
    .replace("from './resource'", `from '${resource}'`)
  const { bootStatusResource } = await import(moduleUrl(source))
  bootStatusResource.start()
  try {
    await bootStatusResource.refresh()
    handlers['feature.data_changed']!({ feature: 'boot', resource: 'options', epoch: '1', revision: 2, data: { entries: [{ id: 'new' }], providers: [] } })
    assert.equal(bootStatusResource.data.value.entries[0].id, 'new')
    assert.equal(bootStatusResource.revision.value, 2)
  } finally {
    bootStatusResource.stop()
    Reflect.deleteProperty(globalThis, '__bootOrdering')
  }
})

test('a resumed upload keeps its original device after navigation', async () => {
  const upload = msdOperationSource('uploadImage')
  let route = 'device-A'
  let release!: () => void
  const calls: string[] = []
  Object.assign(globalThis, { __uploadOrdering: {
    deviceApiBase: () => `/bmc/${route}/api`,
    ApiError: FixtureApiError,
    localStorage: { getItem: () => 'retained-id', removeItem() {}, setItem() {} },
    requestWithBase: async (base: string, endpoint: string, options: RequestInit = {}) => {
      calls.push(`${base}:${options.method ?? 'GET'}`)
      if (!options.method) return new Promise((_, reject) => { release = () => reject(new FixtureApiError(404)) })
      if (options.method === 'POST') return { id: 'new-upload', offset: 0, length: 4, state: 'uploading' }
      return { id: 'new-upload', offset: 4, length: 4, state: 'completed', image_id: 'test.iso', filename: 'test.iso', digests: [] }
    },
  } })
  const { runUpload } = await import(moduleUrl(`const {deviceApiBase,localStorage,requestWithBase,ApiError}=globalThis.__uploadOrdering; ${scopeFactory}; export const runUpload=${upload}`))
  const pending = runUpload('aggregator', { name: 'test.iso', size: 4, lastModified: 0, slice() { return 'content' } })
  route = 'device-B'
  release()
  await pending
  assert.ok(calls.length >= 3)
  assert.ok(calls.every(call => call.startsWith('/bmc/device-A/api:')))
  Reflect.deleteProperty(globalThis, '__uploadOrdering')
})

for (const [method, field, result] of [
  ['fetchHidState', 'hid', { available: true, online: true, backend: 'old' }],
  ['fetchAtxState', 'atx', { available: true, backend: 'old', power_status: 'on' }],
  ['fetchMsdState', 'msd', { available: true }],
  ['fetchFeatures', 'features', { features: [{ id: 'old', state: 'connected' }] }],
] as const) {
  test(`${method} cannot repopulate the system store after changing devices`, async () => {
    setActivePinia(createPinia())
    let complete!: (value: unknown) => void
    const load = () => new Promise(resolve => { complete = resolve })
    Object.assign(globalThis, { __systemScope: {
      api: { status: load, features: load },
      resources: { msdStatusResource: { data: ref(null), refresh: load }, powerStatusResource: { data: ref(null), refresh: load } },
    } })
    const source = readFileSync('src/stores/system.ts', 'utf8')
      .replace("from 'vue'", `from '${vueUrl}'`)
      .replace("from 'pinia'", `from '${piniaUrl}'`)
      .replace("import { currentBmcIdFromLocation } from '@/api/request'", "const currentBmcIdFromLocation = () => 'lab'")
      .replace(/import\s*\{\s*systemApi,[\s\S]*?\}\s*from '@\/api'/, 'const systemApi=globalThis.__systemScope.api, streamApi=systemApi, hidApi=systemApi, msdApi=systemApi')
      .replace("import { msdStatusResource, powerStatusResource } from '@/feature-data/resources'", 'const {msdStatusResource,powerStatusResource}=globalThis.__systemScope.resources')
    const { useSystemStore } = await import(moduleUrl(source + `\n// ${method}`))
    const store = useSystemStore()
    const pending = store[method]()
    store.resetDeviceState()
    complete(result)
    await pending
    assert.deepEqual(store[field], field === 'features' ? [] : null)
    Reflect.deleteProperty(globalThis, '__systemScope')
  })
}

test('a resource resumes after missing a server restart while unsubscribed', async () => {
  Object.assign(globalThis, { window: { setTimeout, clearTimeout } })
  const core = moduleUrl(readFileSync('src/feature-data/core.ts', 'utf8'))
  const source = readFileSync('src/feature-data/resource.ts', 'utf8')
    .replace("from 'vue'", `from '${vueUrl}'`)
    .replace("import { currentBmcIdFromLocation } from '@/api/request'", "const currentBmcIdFromLocation = () => 'lab'")
    .replace("import { useWebSocket } from '@/composables/useWebSocket'", 'const useWebSocket=()=>({on(){},off(){},connect(){},acquireFeature(){return()=>{}}})')
    .replace("from './core'", `from '${core}'`)
  const { createFeatureDataResource } = await import(moduleUrl(source))
  let epoch = '50'
  const resource = createFeatureDataResource({ feature: 'health', load: async () => ({ epoch, revision: 1 }), epoch: (value: {epoch: string}) => value.epoch, revision: (value: {revision: number}) => value.revision })
  resource.start()
  await resource.refresh()
  resource.stop()
  epoch = '1'
  resource.start()
  try {
    await resource.refresh()
    assert.equal(resource.data.value.epoch, '1')
    assert.equal(resource.phase.value, 'ready')
  } finally { resource.stop() }
})

for (const method of ['downloadFromUrl', 'resumePendingDownload', 'cancelDownload']) {
  test(`${method} retains its original device and recovery key during navigation`, async () => {
    const operation = msdOperationSource(method)
    let route = 'A'
    let complete!: (value: unknown) => void
    const bases: string[] = []
    const keys: string[] = []
    Object.assign(globalThis, { __downloadScope: {
      deviceApiBase: () => `/bmc/${route}/api`,
      localStorage: { getItem: (key: string) => { keys.push(key); return 'A-job' }, setItem: (key: string) => keys.push(key), removeItem: (key: string) => keys.push(key) },
      requestWithBase: async (base: string) => {
        bases.push(base)
        if (bases.length === 1) return new Promise(resolve => { complete = resolve })
        return { ok: true, download_id: 'A-job', status: 'running' }
      },
    } })
    const { run } = await import(moduleUrl(`const {deviceApiBase,localStorage,requestWithBase}=globalThis.__downloadScope; ${scopeFactory};export const run=${operation}`))
    const pending = run('https://example.invalid/image.iso')
    route = 'B'
    complete({ ok: true, download_id: 'A-job', status: method === 'resumePendingDownload' ? 'paused' : 'running' })
    await pending
    assert.ok(bases.every(base => base === '/bmc/A/api'))
    assert.ok(keys.length > 0 && keys.every(key => key === 'msd-download:/bmc/A/api'))
    Reflect.deleteProperty(globalThis, '__downloadScope')
  })
}

async function deferredResourceFixture(key: string) {
  const handlers: Record<string, (event: unknown) => void> = {}
  const completions: Array<(value: unknown) => void> = []
  const fixture = {
    device: 'lab',
    ws: { on(name: string, handler: (event: unknown) => void) { handlers[name] = handler }, off() {}, connect() {}, acquireFeature() { return () => {} } },
  }
  Object.assign(globalThis, { window: { setTimeout, clearTimeout }, [key]: fixture })
  const core = moduleUrl(readFileSync('src/feature-data/core.ts', 'utf8'))
  const source = readFileSync('src/feature-data/resource.ts', 'utf8')
    .replace("from 'vue'", `from '${vueUrl}'`)
    .replace("import { currentBmcIdFromLocation } from '@/api/request'", `const currentBmcIdFromLocation = () => globalThis.${key}.device`)
    .replace("import { useWebSocket } from '@/composables/useWebSocket'", `const useWebSocket = () => globalThis.${key}.ws`)
    .replace("from './core'", `from '${core}'`)
  const { createFeatureDataResource } = await import(moduleUrl(source))
  const resource = createFeatureDataResource({
    feature: 'power',
    load: () => new Promise(resolve => completions.push(resolve)),
    revision: (value: { revision: number }) => value.revision,
    epoch: (value: { epoch: string }) => value.epoch,
  })
  resource.start()
  return { resource, handlers, completions, fixture, close() { resource.stop(); Reflect.deleteProperty(globalThis, key) } }
}

test('same-revision power failure and recovery update observation metadata', async () => {
  const f = await deferredResourceFixture('__powerReadFailure')
  try {
    const first = f.resource.refresh()
    f.completions[0]!({ epoch: '1', revision: 3, power_status: 'on', stale: false, fault: null })
    await first
    const failed = f.resource.refresh()
    f.completions[1]!({ epoch: '1', revision: 3, power_status: 'on', stale: true, fault: { code: 'status_failed' } })
    await failed
    assert.equal(f.resource.revision.value, 3)
    assert.equal(f.resource.data.value.stale, true)
    assert.equal(f.resource.data.value.fault.code, 'status_failed')
    const recovered = f.resource.refresh()
    f.completions[2]!({ epoch: '1', revision: 3, power_status: 'on', stale: false, fault: null })
    await recovered
    assert.equal(f.resource.data.value.stale, false)
    assert.equal(f.resource.data.value.fault, null)
  } finally {
    f.close()
  }
})

test('resync replaces an older pending read and rejects its late response', async () => {
  const f = await deferredResourceFixture('__resyncRead')
  try {
    const old = f.resource.refresh()
    f.handlers['feature.data_resync_required']!({ epoch: '2' })
    assert.equal(f.completions.length, 2)
    f.completions[1]!({ epoch: '2', revision: 4, state: 'new' })
    await f.resource.refresh()
    f.completions[0]!({ epoch: '1', revision: 100, state: 'old' })
    await old
    assert.equal(f.resource.data.value.state, 'new')
    assert.equal(f.resource.phase.value, 'ready')
  } finally { f.close() }
})

test('mutation completion refreshes past an older pending read even after resync', async () => {
  const f = await deferredResourceFixture('__mutationRead')
  try {
    const old = f.resource.refresh()
    let complete!: () => void
    const mutation = f.resource.mutate(() => new Promise<void>(resolve => { complete = resolve }))
    f.handlers['feature.data_resync_required']!({})
    complete()
    await Promise.resolve()
    assert.equal(f.completions.length, 3)
    f.completions[2]!({ epoch: '1', revision: 3, state: 'committed' })
    await mutation
    f.completions[1]!({ epoch: '1', revision: 2, state: 'before-mutation' })
    f.completions[0]!({ epoch: '1', revision: 1, state: 'old' })
    await old
    assert.equal(f.resource.data.value.state, 'committed')
  } finally { f.close() }
})

test('mutation completion after navigation does not refresh another device', async () => {
  const f = await deferredResourceFixture('__mutationNavigation')
  try {
    const old = f.resource.refresh()
    let complete!: () => void
    const mutation = f.resource.mutate(() => new Promise<void>(resolve => { complete = resolve }))
    f.fixture.device = 'other'
    complete()
    await mutation
    assert.equal(f.completions.length, 1)
    f.resource.stop()
    f.completions[0]!({ epoch: '1', revision: 1 })
    await old
  } finally { f.close() }
})

test('cached feature consumers release on deactivation and reacquire once on activation', async () => {
  const hooks: { activated?: () => void; deactivated?: () => void } = {}
  Object.assign(globalThis, { __featureActivationHooks: hooks })
  const vue = moduleUrl(`export * from '${vueUrl}';
    export const getCurrentInstance = () => ({});
    export const onActivated = fn => globalThis.__featureActivationHooks.activated = fn;
    export const onDeactivated = fn => globalThis.__featureActivationHooks.deactivated = fn;`)
  const core = moduleUrl(readFileSync('src/feature-data/core.ts', 'utf8'))
  const source = readFileSync('src/feature-data/resource.ts', 'utf8')
    .replace("from 'vue'", `from '${vue}'`)
    .replace("import { currentBmcIdFromLocation } from '@/api/request'", "const currentBmcIdFromLocation = () => 'lab'")
    .replace("import { useWebSocket } from '@/composables/useWebSocket'", 'const useWebSocket = () => ({})')
    .replace("from './core'", `from '${core}'`)
  const { useFeatureDataSubscription } = await import(moduleUrl(source))
  const { nextTick } = await import(vueUrl)
  const owner = effectScope()
  const active = ref(true)
  let starts = 0, stops = 0
  try {
    owner.run(() => useFeatureDataSubscription({ start() { starts++ }, stop() { stops++ } }, active))
    assert.equal(starts, 1)
    hooks.activated!()
    assert.equal(starts, 1)
    hooks.deactivated!()
    hooks.deactivated!()
    assert.equal(stops, 1)
    active.value = false
    await nextTick()
    active.value = true
    await nextTick()
    assert.equal(starts, 1)
    hooks.activated!()
    hooks.activated!()
    assert.equal(starts, 2)
    owner.stop()
    assert.equal(stops, 2)
    hooks.activated!()
    assert.equal(starts, 2)
  } finally {
    owner.stop()
    Reflect.deleteProperty(globalThis, '__featureActivationHooks')
  }
})

for (const method of ['uploadImage', 'resumePendingDownload']) {
  test(`${method} preserves recovery records when status inspection fails`, async () => {
    const operation = msdOperationSource(method)
    for (const failure of [new Error('network unavailable'), new FixtureApiError(401), new FixtureApiError(500), new FixtureApiError(503)]) {
      let removed = 0, mutations = 0, requests = 0
      Object.assign(globalThis, { __transferRecovery: {
        ApiError: FixtureApiError,
        deviceApiBase: () => '/bmc/A/api',
        localStorage: { getItem: () => 'existing-transfer', removeItem() { removed++ }, setItem() {} },
        requestWithBase: async (_base: string, _endpoint: string, options: RequestInit = {}) => {
          requests++
          if (!options.method) throw failure
          mutations++
          return { id: 'new-transfer', offset: 4, length: 4, state: 'completed', image_id: 'test.iso', filename: 'test.iso', digests: [] }
        },
      } })
      const { run } = await import(moduleUrl(`const {deviceApiBase,localStorage,requestWithBase,ApiError}=globalThis.__transferRecovery; ${scopeFactory};export const run=${operation}; // ${method}-${failure.message}`))
      try {
        await assert.rejects(run('aggregator', { name: 'test.iso', size: 4, lastModified: 0 }), error => error === failure)
        assert.equal(removed, 0)
        assert.equal(mutations, 0)
        assert.equal(requests, 1)
      } finally { Reflect.deleteProperty(globalThis, '__transferRecovery') }
    }
  })
}


for (const method of ['restoreUrlDownload', 'cancelUrlDownload']) {
  test(`${method} handles inspection failure without clearing progress or restarting polling`, async () => {
    const source = readFileSync('src/features/msd/MsdDialog.vue', 'utf8').split('<script setup lang="ts">')[1]!.split('</script>')[0]!
    const ast = ts.createSourceFile('dialog.ts', source, ts.ScriptTarget.Latest, true)
    const operation = ast.statements.find(statement => ts.isFunctionDeclaration(statement) && statement.name?.text === method)!.getText(ast)
    let notifications = 0, polls = 0, resets = 0
    const progress = { download_id: 'existing-transfer', status: 'paused' }
    const fail = async () => { throw new Error('inspection unavailable') }
    const downloadProgress = { value: progress }
    Object.assign(globalThis, { __restoreFailure: {
      msdApi: { resumePendingDownload: fail, cancelDownload: fail },
      console: { error() {} },
      toast: { error(message: string) { assert.equal(message, 'inspection unavailable'); notifications++ } },
      downloadProgress, downloading: { value: false },
      startDownloadPolling() { polls++ }, resetDownloadState() { resets++ },
    } })
    const { run } = await import(moduleUrl(`const {msdApi,console,toast,downloadProgress,downloading,startDownloadPolling,resetDownloadState}=globalThis.__restoreFailure; ${operation}; export const run=${method}`))
    try {
      await run()
      assert.equal(notifications, 1)
      assert.equal(polls, 0)
      assert.equal(resets, 0)
      assert.equal(downloadProgress.value, progress)
    } finally { Reflect.deleteProperty(globalThis, '__restoreFailure') }
  })
}

test('unconfirmed download cancellation retains its recovery record', async () => {
  let removed = 0
  Object.assign(globalThis, { __cancelFailure: {
    deviceApiBase: () => '/bmc/A/api',
    localStorage: { removeItem() { removed++ } },
    requestWithBase: async () => ({ ok: false }),
  } })
  const { run } = await import(moduleUrl(`const {deviceApiBase,localStorage,requestWithBase}=globalThis.__cancelFailure; ${scopeFactory};export const run=${msdOperationSource('cancelDownload')}; // unconfirmed`))
  try {
    await assert.rejects(run('existing-download'), /not confirmed/)
    assert.equal(removed, 0)
  } finally { Reflect.deleteProperty(globalThis, '__cancelFailure') }
})


test('resource mutations resume polling after both successful and failed readback', async () => {
  const timers = new Map<number, () => void>()
  let nextTimer = 0
  const previousWindow = globalThis.window
  Object.assign(globalThis, { window: {
    setTimeout(callback: () => void) { const id = ++nextTimer; timers.set(id, callback); return id },
    clearTimeout(id: number) { timers.delete(id) },
  } })
  const core = moduleUrl(readFileSync('src/feature-data/core.ts', 'utf8'))
  const source = readFileSync('src/feature-data/resource.ts', 'utf8')
    .replace("from 'vue'", `from '${vueUrl}'`)
    .replace("import { currentBmcIdFromLocation } from '@/api/request'", "const currentBmcIdFromLocation = () => 'lab'")
    .replace("import { useWebSocket } from '@/composables/useWebSocket'", 'const useWebSocket = () => ({on(){},off(){},connect(){},acquireFeature(){return () => {}}})')
    .replace("from './core'", `from '${core}'`)
  const { createFeatureDataResource } = await import(moduleUrl(source))
  let reads = 0
  let failRead = false
  const resource = createFeatureDataResource({
    feature: 'power', pollInterval: 3000,
    load: async () => { ++reads; if (failRead) throw new Error('readback failed'); return { revision: reads } },
    revision: (value: { revision: number }) => value.revision,
  })
  try {
    resource.start()
    await resource.refresh()
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(timers.size, 1)
    await resource.mutate(async () => 'accepted')
    assert.equal(timers.size, 1, 'successful mutation readback must restart polling')
    failRead = true
    await resource.mutate(async () => 'accepted')
    assert.equal(timers.size, 1, 'failed mutation readback must preserve recovery polling')
    const uncertain = new Error('mutation result unknown')
    await assert.rejects(resource.mutate(async () => { throw uncertain }), error => error === uncertain)
    assert.equal(timers.size, 1, 'failed mutation and failed readback must preserve recovery polling')
    failRead = false
    const [id, callback] = [...timers.entries()][0]!
    timers.delete(id)
    const before = reads
    callback()
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(reads, before + 1)
    assert.equal(timers.size, 1)
    resource.stop()
    assert.equal(timers.size, 0)
    await resource.mutate(async () => 'accepted')
    assert.equal(timers.size, 0, 'inactive resources must not restart polling')
  } finally {
    resource.stop()
    Object.assign(globalThis, { window: previousWindow })
  }
})


test('failed mutations read back uncertain remote state and preserve the original error', async () => {
  const f = await deferredResourceFixture('__uncertainMutation')
  try {
    f.completions[0]!({ epoch: '1', revision: 1, state: 'before' })
    await f.resource.refresh()
    const failure = new Error('response lost after remote acceptance')
    let settled = false
    const mutation = f.resource.mutate(async () => { throw failure })
    const observed = mutation.then(() => { settled = true; return null }, (error: unknown) => { settled = true; return error })
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(f.completions.length, 2, 'an uncertain mutation must trigger authoritative readback')
    assert.equal(settled, false)
    f.completions[1]!({ epoch: '1', revision: 2, state: 'applied' })
    assert.equal(await observed, failure)
    assert.equal(f.resource.data.value.state, 'applied')
  } finally { f.close() }
})

test('failed mutation completion after navigation never reads another device', async () => {
  const f = await deferredResourceFixture('__failedMutationNavigation')
  try {
    const old = f.resource.refresh()
    const failure = new Error('response lost')
    let reject!: (error: Error) => void
    const mutation = f.resource.mutate(() => new Promise<void>((_, fail) => { reject = fail }))
    const observed = mutation.catch((error: unknown) => error)
    f.fixture.device = 'other'
    reject(failure)
    assert.equal(await observed, failure)
    assert.equal(f.completions.length, 1)
    f.resource.stop()
    f.completions[0]!({ epoch: '1', revision: 1 })
    await old
  } finally { f.close() }
})


let webTransportFixtureSerial = 0

async function webTransportFixture(readyPending = false, stage?: 'capabilities' | 'codec') {
  const key = `__wt_${++webTransportFixtureSerial}`
  const requests: Array<{ operation: string; device: string }> = []
  let complete!: () => void
  const gate = new Promise<void>(resolve => { complete = resolve })
  const fixture = {
    device: 'A',
    api: (owner: () => string) => ({
      async getWebTransportCapabilities() {
        const device = owner()
        requests.push({ operation: 'capabilities', device })
        if (stage === 'capabilities') await gate
        return { enabled: true, url: `https://${device}.invalid/transport` }
      },
      async connectWebTransport() {
        const device = owner()
        requests.push({ operation: 'ticket', device })
        return { success: true, url: `https://${device}.invalid/transport` }
      },
    }),
  }
  Object.assign(globalThis, { [key]: fixture })
  const previousWindow = globalThis.window
  const transports: Array<{
    fail: (error: Error) => void; finish: () => void; failReady: (error: Error) => void;
    closes: number; incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;
  }> = []
  const decoders: Array<{ state: string; closes: number }> = []
  class FixtureTransport {
    ready: Promise<void>
    closed: Promise<void>
    fail!: (error: Error) => void
    finish!: () => void
    failReady!: (error: Error) => void
    closes = 0
    controller!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>
    incomingUnidirectionalStreams = new ReadableStream<ReadableStream<Uint8Array>>({ start: controller => { this.controller = controller } })
    constructor() {
      this.ready = readyPending ? new Promise<void>((_, reject) => { this.failReady = reject }) : Promise.resolve()
      this.closed = new Promise<void>((resolve, reject) => { this.finish = resolve; this.fail = reject })
      transports.push(this)
    }
    close() { if (++this.closes === 1) this.controller.close() }
  }
  class FixtureDecoder {
    static async isConfigSupported() { if (stage === 'codec') await gate; return { supported: true } }
    state = 'configured'
    closes = 0
    constructor() { decoders.push(this) }
    configure() {}
    close() { ++this.closes; this.state = 'closed' }
  }
  Object.assign(globalThis, { window: { isSecureContext: true, WebTransport: FixtureTransport, VideoDecoder: FixtureDecoder } })
  const source = readFileSync('src/composables/useWebTransport.ts', 'utf8')
    .replace("from 'vue'", `from '${vueUrl}'`)
    .replace(/import \{[^\n]*\} from '@\/api'/, `
      const fixture = globalThis.${key};
      const streamApi = fixture.api(() => fixture.device);
      const createWebTransportApi = scope => fixture.api(() => scope.base);
    `)
    .replace("import { captureDeviceRequest, currentBmcIdFromLocation } from '@/api/request'", `
      const currentBmcIdFromLocation = () => globalThis.${key}.device;
      const captureDeviceRequest = () => ({ request: { base: globalThis.${key}.device } });
    `)
    .replace("import { generateUUID } from '@/lib/utils'", "const generateUUID = () => 'fixture-connection'")
  const { useWebTransport } = await import(moduleUrl(source))
  const client = useWebTransport()
  return { client, fixture, requests, complete, transports, decoders, async close() {
    complete(); await client.disconnect()
    Object.assign(globalThis, { window: previousWindow })
    Reflect.deleteProperty(globalThis, key)
  } }
}

test('WebTransport remote closure releases decoder and transport resources', async () => {
  const f = await webTransportFixture()
  try {
    await f.client.connect('h264')
    f.transports[0]!.finish()
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(f.client.state.value, 'closed')
    assert.equal(f.decoders[0]!.closes, 1, 'remote closure must release its decoder')
    assert.equal(f.transports[0]!.closes, 1)
  } finally { await f.close() }
})

test('WebTransport closure rejection becomes a handled connection failure', async () => {
  const f = await webTransportFixture()
  try {
    await f.client.connect('h264')
    f.transports[0]!.fail(new Error('transport connection lost'))
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(f.client.state.value, 'failed')
    assert.equal(f.client.error.value, 'transport connection lost')
    assert.equal(f.decoders[0]!.closes, 1)
  } finally { await f.close() }
})

test('WebTransport observes closure rejection even before readiness fails', async () => {
  const f = await webTransportFixture(true)
  try {
    const pending = f.client.connect('h264')
    const observed = pending.catch((error: unknown) => error)
    await new Promise(resolve => setImmediate(resolve))
    const failure = new Error('handshake lost')
    f.transports[0]!.fail(failure)
    f.transports[0]!.failReady(failure)
    assert.equal(await observed, failure)
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(f.client.state.value, 'failed')
    assert.equal(f.decoders[0]!.closes, 1)
  } finally { await f.close() }
})

test('WebTransport late closure from a replaced transport cannot close its successor', async () => {
  const f = await webTransportFixture()
  try {
    await f.client.connect('h264')
    const previous = f.transports[0]!
    await f.client.connect('h264')
    previous.fail(new Error('old transport lost'))
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(f.client.state.value, 'connected')
    assert.equal(f.decoders[1]!.closes, 0)
  } finally { await f.close() }
})


let webRtcFixtureSerial = 0

async function webRtcFixture(stage: 'ice' | 'offer' | 'close') {
  const previousWindow = globalThis.window
  const previousPeer = globalThis.RTCPeerConnection
  const key = `__rtc_${stage}_${++webRtcFixtureSerial}`
  const peers: Array<{ closes: number; descriptions: number; getStats: () => Promise<Map<string, Record<string, unknown>>> }> = []
  const statsTicks: Array<() => Promise<void>> = []
  const requests: Array<{ operation: string; device: string }> = []
  let complete!: () => void
  const gate = new Promise<void>(resolve => { complete = resolve })
  const fixture = {
    device: 'A',
    offerFailure: null as Error | null,
    api: (owner: () => string) => ({
      async getIceServers() { if (stage === 'ice') await gate; return { ice_servers: [{ urls: 'stun:example.invalid' }] } },
      async offer() { requests.push({ operation: 'offer', device: owner() }); if (stage === 'offer') await gate; if (fixture.offerFailure) throw fixture.offerFailure; return { connection_id: 'fixture-session', sdp: 'answer', ice_candidates: [] } },
      async addIceCandidate() {},
      async close() { requests.push({ operation: 'close', device: owner() }); if (stage === 'close') await gate },
    }),
  }
  class FixturePeer {
    closes = 0
    descriptions = 0
    connectionState = 'connected'
    iceConnectionState = 'connected'
    constructor() { peers.push(this) }
    addTransceiver() {}
    async createOffer() { return { sdp: 'offer' } }
    async setLocalDescription() {}
    async setRemoteDescription() { ++this.descriptions }
    async addIceCandidate() {}
    async getStats() { return new Map() }
    close() { ++this.closes; this.connectionState = 'closed' }
  }
  Object.assign(globalThis, { window: { location: { hostname: 'localhost' }, setInterval(callback: () => Promise<void>) { statsTicks.push(callback); return statsTicks.length }, clearInterval() {}, addEventListener() {} }, RTCPeerConnection: FixturePeer, [key]: fixture })
  const source = readFileSync('src/composables/useWebRTC.ts', 'utf8')
    .replace("from 'vue'", `from '${vueUrl}'`)
    .replace(/import \{[^\n]*\} from '@\/api'/, `
      const fixture = globalThis.${key};
      const webrtcApi = fixture.api(() => fixture.device);
      const createWebrtcApi = scope => fixture.api(() => scope.base);
    `)
    .replace("import { captureDeviceRequest, currentBmcIdFromLocation } from '@/api/request'", `
      const currentBmcIdFromLocation = () => globalThis.${key}.device;
      const captureDeviceRequest = () => ({ base: globalThis.${key}.device, request: { base: globalThis.${key}.device } });
    `)
    .replace("import { videoDebugLog } from '@/lib/debugLog'", 'const videoDebugLog = () => {}')
  const { useWebRTC } = await import(moduleUrl(source))
  const client = useWebRTC()
  return { client, fixture, peers, requests, statsTicks, complete, async close() {
    complete(); await client.disconnect()
    Object.assign(globalThis, { window: previousWindow, RTCPeerConnection: previousPeer })
    Reflect.deleteProperty(globalThis, key)
  } }
}

test('WebRTC disconnect during ICE discovery cannot resurrect its peer', async () => {
  const f = await webRtcFixture('ice')
  try {
    const pending = f.client.connect()
    await f.client.disconnect()
    f.complete()
    assert.equal(await pending, false)
    assert.equal(f.peers.length, 0)
    assert.equal(f.requests.length, 0)
    assert.equal(f.client.state.value, 'disconnected')
  } finally { await f.close() }
})

test('WebRTC delayed offers cannot publish into a disconnected peer', async () => {
  const f = await webRtcFixture('offer')
  try {
    const pending = f.client.connect()
    await new Promise(resolve => setImmediate(resolve))
    await f.client.disconnect()
    f.complete()
    assert.equal(await pending, false)
    assert.equal(f.client.connectionId.value, null)
    assert.equal(f.peers[0]!.descriptions, 0)
    assert.ok(f.requests.some(request => request.operation === 'close'), 'a late remote session must be released')
  } finally { await f.close() }
})

test('WebRTC close keeps its device scope and cannot clear a newer connection', async () => {
  const f = await webRtcFixture('close')
  try {
    assert.equal(await f.client.connect(), true)
    f.fixture.device = 'B'
    const closing = f.client.disconnect()
    assert.equal(await f.client.connect(), true)
    f.complete()
    await closing
    assert.equal(f.requests.find(request => request.operation === 'close')?.device, 'A')
    assert.equal(f.client.state.value, 'connected')
    assert.equal(f.client.connectionId.value, 'fixture-session')
    assert.equal(f.peers[1]!.closes, 0)
  } finally { await f.close() }
})


test('WebRTC stats completed after replacement cannot mark the successor active', async () => {
  const f = await webRtcFixture('close')
  try {
    assert.equal(await f.client.connect(), true)
    let completeStats!: (report: Map<string, Record<string, unknown>>) => void
    let reads = 0
    f.peers[0]!.getStats = () => { ++reads; return new Promise(resolve => { completeStats = resolve }) }
    const pending = f.statsTicks[0]!()
    await f.statsTicks[0]!()
    assert.equal(reads, 1, 'stats sampling must not overlap an unfinished read')
    f.complete()
    await f.client.disconnect()
    f.fixture.device = 'B'
    assert.equal(await f.client.connect(), true)
    completeStats(new Map([['video', { type: 'inbound-rtp', kind: 'video', framesDecoded: 999, bytesReceived: 9999, frameWidth: 1920, frameHeight: 1080 }]]))
    await pending
    assert.equal(f.client.stats.value.framesDecoded, 0)
    assert.equal(f.client.stats.value.bytesReceived, 0)
  } finally { await f.close() }
})

test('WebRTC APIs send every stage through the captured device request', async () => {
  const ast = ts.createSourceFile('index.ts', readFileSync('src/api/index.ts', 'utf8'), ts.ScriptTarget.Latest, true)
  const declarations = ast.statements.filter(statement => ts.isFunctionDeclaration(statement) && ['createWebrtcApi', 'assertNoWebRtcError'].includes(statement.name?.text ?? ''))
  assert.equal(declarations.length, 2)
  const module = await import(moduleUrl(`
    let device = 'A';
    export const bases = [];
    export const navigate = () => { device = 'B' };
    const deviceApiBase = () => device;
    const requestWithBase = async base => { bases.push(base); return { connection_id: 'session', sdp: 'answer', success: true } };
    ${scopeFactory}
    ${declarations.map(node => node.getText(ast)).join('\n')}
  `))
  const api = module.createWebrtcApi(module.captureDeviceRequest().request)
  module.navigate()
  await api.getIceServers()
  await api.offer('offer')
  await api.addIceCandidate({ candidate: 'candidate' }, 'session')
  await api.close('session')
  assert.deepEqual(module.bases, ['A', 'A', 'A', 'A'])
})


test('WebRTC never reuses a connected peer belonging to another device', async () => {
  const f = await webRtcFixture('close')
  try {
    assert.equal(await f.client.connect(), true)
    f.fixture.device = 'B'
    const next = f.client.connect()
    f.complete()
    assert.equal(await next, true)
    assert.equal(f.peers.length, 2)
    assert.equal(f.peers[0]!.closes, 1)
    assert.equal(f.requests.find(request => request.operation === 'close')?.device, 'A')
    assert.equal(f.requests.filter(request => request.operation === 'offer').at(-1)?.device, 'B')
  } finally { await f.close() }
})


test('WebRTC setup failure survives cleanup as an actionable failure', async () => {
  const f = await webRtcFixture('offer')
  try {
    f.fixture.offerFailure = new Error('offer rejected by backend')
    const pending = f.client.connect()
    f.complete()
    assert.equal(await pending, false)
    assert.equal(f.client.state.value, 'failed')
    assert.equal(f.client.error.value, 'offer rejected by backend')
    assert.equal(f.peers[0]!.closes, 1)
    assert.equal(f.client.connectionId.value, null)
  } finally { await f.close() }
})


test('WebTransport disconnect immediately after connect cancels the entire attempt', async () => {
  const f = await webTransportFixture()
  try {
    const pending = f.client.connect('h264').then(() => true, () => false)
    await f.client.disconnect()
    assert.equal(await pending, false)
    assert.equal(f.transports.length, 0)
    assert.equal(f.requests.length, 0)
    assert.equal(f.client.state.value, 'closed')
  } finally { await f.close() }
})

for (const stage of ['capabilities', 'codec'] as const) {
  test(`WebTransport navigation during ${stage} cannot issue a ticket for another device`, async () => {
    const f = await webTransportFixture(false, stage)
    try {
      const pending = f.client.connect('h264').then(() => true, () => false)
      await new Promise(resolve => setImmediate(resolve))
      f.fixture.device = 'B'
      f.complete()
      assert.equal(await pending, false)
      assert.equal(f.requests.filter(request => request.operation === 'ticket').length, 0)
      assert.equal(f.transports.length, 0)
    } finally { await f.close() }
  })
}

test('WebTransport capability completion after disconnect cannot restore retired cache state', async () => {
  const f = await webTransportFixture(false, 'capabilities')
  try {
    const pending = f.client.refreshCapabilities()
    await f.client.disconnect()
    f.complete()
    await pending
    assert.equal(f.client.capabilities.value, null)
  } finally { await f.close() }
})

test('WebTransport refreshes capabilities when reusing a client for another device', async () => {
  const f = await webTransportFixture()
  try {
    await f.client.connect('h264')
    await f.client.disconnect()
    f.fixture.device = 'B'
    await f.client.connect('h264')
    assert.deepEqual(f.requests.filter(request => request.operation === 'capabilities').map(request => request.device), ['A', 'B'])
  } finally { await f.close() }
})

test('WebTransport API stages retain the captured device request', async () => {
  const ast = ts.createSourceFile('index.ts', readFileSync('src/api/index.ts', 'utf8'), ts.ScriptTarget.Latest, true)
  const factory = ast.statements.find(statement => ts.isFunctionDeclaration(statement) && statement.name?.text === 'createWebTransportApi')
  assert.ok(factory)
  const module = await import(moduleUrl(`
    let device = 'A';
    export const bases = [];
    export const navigate = () => { device = 'B' };
    const deviceApiBase = () => device;
    const requestWithBase = async base => { bases.push(base); return {} };
    ${scopeFactory}
    ${factory.getText(ast)}
  `))
  const api = module.createWebTransportApi(module.captureDeviceRequest().request)
  module.navigate()
  await api.getWebTransportCapabilities()
  await api.connectWebTransport({ connection_id: 'session', codec: 'h264' })
  assert.deepEqual(module.bases, ['A', 'A'])
})
