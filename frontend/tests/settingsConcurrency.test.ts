import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import test from 'node:test'
import ts from 'typescript'

const moduleUrl = (source: string) => 'data:text/javascript;base64,' + Buffer.from(
  ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022,
  } }).outputText,
).toString('base64')
function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(done => { resolve = done })
  return { promise, resolve }
}
let sequence = 0
async function loadView(name: string, dependencies: Record<string, unknown>, exports: string) {
  const key = `__settingsFixture${++sequence}`
  Object.assign(globalThis, { [key]: dependencies })
  const vueUrl = pathToFileURL(resolve('node_modules/vue/dist/vue.runtime.esm-bundler.js')).href
  const vue = moduleUrl(`export { ref, computed, watch } from '${vueUrl}'; export const onMounted = () => {}; export const onBeforeUnmount = fn => globalThis.${key}.onDispose?.(fn); export const onUnmounted = onBeforeUnmount; export const onActivated = fn => globalThis.${key}.onActivate?.(fn); export const onDeactivated = fn => globalThis.${key}.onDeactivate?.(fn)`)
  const source = readFileSync(`src/${name.includes('/') ? name : `views/settings/${name}`}.vue`, 'utf8').split('<script setup lang="ts">')[1]!.split('</script>')[0]!
    .replace(/^import[\s\S]*?from '([^']+)'\n/gm, (line, path) => path === 'vue' ? line.replace("'vue'", `'${vue}'`) : '')
  const order = moduleUrl(readFileSync('src/configuration/requestOrder.ts', 'utf8'))
  const prelude = `import { RequestOrder } from '${order}'; const { ${Object.keys(dependencies).join(',')} } = globalThis.${key};\n`
  try { return await import(moduleUrl(prelude + source + `\nexport { ${exports} }`)) }
  finally { Reflect.deleteProperty(globalThis, key) }
}
const web = { bind_address: '127.0.0.1', http_port: 80, frontend_dir: 'dist' }
async function globalFixture(request: (...args: unknown[]) => unknown) {
  return loadView('GlobalSettingsView', {
    rootRequestMeta: request, useConfigurationEvents() {}, systemApi: {},
    localStorage: { getItem() { return null } }, ApiError: class extends Error {},
  }, 'web, etag, dirty, loading, refreshWeb, saveWeb')
}

test('service settings discard pre-save reads and reject duplicate saves', async () => {
  const read = deferred<unknown>(), write = deferred<unknown>()
  let writes = 0
  const f = await globalFixture((_url, options) => {
    if (options) { writes++; return write.promise }
    return read.promise
  })
  f.web.value = { ...web }; f.etag.value = 'v1'
  const pendingRead = f.refreshWeb()
  f.web.value.http_port = 8080; f.dirty.value = true
  const pendingWrite = f.saveWeb()
  await f.saveWeb()
  assert.equal(writes, 1)
  write.resolve({ data: { ...web, http_port: 8080 }, etag: 'v2' })
  await pendingWrite
  read.resolve({ data: web, etag: 'v1' })
  await pendingRead
  assert.equal(f.web.value.http_port, 8080)
  assert.equal(f.etag.value, 'v2')
})

test('service settings retain edits made while a refresh is pending', async () => {
  const read = deferred<unknown>()
  const f = await globalFixture(() => read.promise)
  f.web.value = { ...web }; f.etag.value = 'v1'
  const pending = f.refreshWeb()
  f.web.value.http_port = 8080; f.dirty.value = true
  read.resolve({ data: { ...web, http_port: 9000 }, etag: 'v2' })
  await pending
  assert.equal(f.web.value.http_port, 8080)
  assert.equal(f.dirty.value, true)
})

async function deviceFixture(update: () => Promise<unknown>, remove = async () => {}) {
  const vue = await import(pathToFileURL(resolve('node_modules/vue/dist/vue.runtime.esm-bundler.js')).href)
  const route = vue.reactive({ params: { bmcId: 'a' }, query: {} })
  const store = { catalog: { features: [], option_resources: [] }, selected: {
    id: 'a', name: 'device-a', endpoint: { host: 'host-a', port: 443 }, credentials: { username: 'a' }, assignments: {},
  }, update, remove, loadDevice: async () => {}, loadOptions: async () => {} }
  const navigations: unknown[] = []
  const f = await loadView('DeviceSettingsView', {
    useRoute: () => route, useRouter: () => ({ push: async (value: unknown) => { navigations.push(value) } }),
    useDeviceConfigurationStore: () => store, useConfigurationEvents() {}, onBeforeRouteLeave() {},
    featureAssignmentToMutation: (value: unknown) => value, ApiError: class extends Error {}, window: { confirm: () => true },
  }, 'name, dirty, saving, save, remove, load, markDirty')
  return { f, route, store, navigations }
}

test('late device saves cannot overwrite a different device form', async () => {
  const write = deferred<unknown>()
  const { f, route } = await deviceFixture(() => write.promise)
  f.name.value = 'draft-a'
  const pending = f.save()
  route.params.bmcId = 'b'
  await Promise.resolve()
  f.name.value = 'draft-b'; f.dirty.value = true
  write.resolve(undefined)
  await pending
  assert.equal(f.name.value, 'draft-b')
  assert.equal(f.dirty.value, true)
})

test('late device deletion cannot navigate away from another device', async () => {
  const deletion = deferred<void>()
  const { f, route, navigations } = await deviceFixture(async () => {}, () => deletion.promise)
  const pending = f.remove()
  route.params.bmcId = 'b'
  deletion.resolve()
  await pending
  assert.equal(navigations.length, 0)
})

test('device form refresh preserves edits started during option loading', async () => {
  const options = deferred<void>()
  const { f, store } = await deviceFixture(async () => {})
  Object.assign(store.catalog, { option_resources: [{ resource_id: 'input' }] })
  store.loadOptions = () => options.promise
  const pending = f.load()
  await Promise.resolve()
  f.name.value = 'new draft'; f.dirty.value = true
  options.resolve()
  await pending
  assert.equal(f.name.value, 'new draft')
  assert.equal(f.dirty.value, true)
})


test('successful settings saves preserve edits made while the write is pending', async () => {
  const write = deferred<unknown>()
  const f = await globalFixture(() => write.promise)
  f.web.value = { ...web }; f.etag.value = 'v1'; f.dirty.value = true
  const pending = f.saveWeb()
  f.web.value.http_port = 8081
  write.resolve({ data: web, etag: 'v2' })
  await pending
  assert.equal(f.web.value.http_port, 8081)
  assert.equal(f.dirty.value, true)
  assert.equal(f.etag.value, 'v2')

  const deviceWrite = deferred<unknown>()
  const { f: device } = await deviceFixture(() => deviceWrite.promise)
  const save = device.save()
  device.name.value = 'edited during save'; device.markDirty()
  deviceWrite.resolve(undefined)
  await save
  assert.equal(device.name.value, 'edited during save')
  assert.equal(device.dirty.value, true)
})


test('a device form cannot save fields belonging to the previously selected device', async () => {
  let writes = 0
  const { f, route } = await deviceFixture(async () => { writes++ })
  route.params.bmcId = 'b'
  f.name.value = 'still device-a fields'
  await f.save()
  assert.equal(writes, 0)
})


async function creationFixture(preview: () => Promise<unknown>, create = async () => ({ id: 'a' })) {
  let dispose = () => {}
  const navigations: unknown[] = []
  const f = await loadView('DeviceCreateView', {
    useRouter: () => ({ push: async (value: unknown) => { navigations.push(value) } }),
    useDeviceConfigurationStore: () => ({ catalog: { creation_profiles: [] }, create }),
    configurationApi: { creationDraft: preview }, onDispose(fn: () => void) { dispose = fn },
  }, 'id, name, host, profileId, draft, active, preview, create')
  f.profileId.value = 'profile'; f.id.value = 'a'; f.name.value = 'A'; f.host.value = 'bmc-a'
  return { f, navigations, dispose: () => dispose() }
}

test('device creation rejects stale drafts after identity changes and invalidates reviewed drafts', async () => {
  const response = deferred<unknown>()
  let writes = 0
  const { f } = await creationFixture(() => response.promise, async () => { writes++; return { id: 'a' } })
  const pending = f.preview()
  f.id.value = 'b'
  response.resolve({ draft: { id: 'a', assignments: {} } })
  await pending
  assert.equal(f.draft.value, null)
  assert.equal(f.active.value, 'identity')
  await f.preview()
  assert.equal(f.draft.value.id, 'a')
  f.host.value = 'bmc-b'
  await f.create()
  assert.equal(writes, 0)
})

test('device creation cannot submit twice or redirect after its page has been disposed', async () => {
  const write = deferred<{ id: string }>()
  let writes = 0
  const { f, navigations, dispose } = await creationFixture(async () => ({ draft: { id: 'a', assignments: {} } }),
    () => { writes++; return write.promise })
  await f.preview()
  const pending = f.create()
  await f.create()
  assert.equal(writes, 1)
  dispose()
  write.resolve({ id: 'a' })
  await pending
  assert.equal(navigations.length, 0)
})


test('requirements cannot restart polling after unmount or remediate a different device', async () => {
  let device = 'a', dispose = () => {}, timers = 0, mutations = 0
  const read = deferred<unknown>()
  const f = await loadView('components/RequirementsSheet', {
    useI18n: () => ({ t: (key: string) => key }),
    currentBmcIdFromLocation: () => device,
    systemApi: { requirements: () => read.promise, remediateRequirement: async () => { mutations++ } },
    toast: { success() {} },
    window: { clearTimeout() {}, setTimeout() { timers++; return timers } },
    onDispose(fn: () => void) { dispose = fn },
  }, 'refresh, requirements, selected, remediate')
  const pending = f.refresh()
  f.selected.value = { feature: 'boot', id: 'inventory', remediation: { action: 'enable' } }
  device = 'b'
  await f.remediate()
  assert.equal(mutations, 0)
  dispose()
  read.resolve({ requirements: [{ state: 'unknown' }] })
  await pending
  assert.equal(timers, 0)
  assert.deepEqual(f.requirements.value, [])
})


test('cached requirements panels stop polling and refresh when activated again', async () => {
  let activate = () => {}, deactivate = () => {}, reads = 0
  const timers = new Map<number, () => void>()
  let timerId = 0
  const pending = deferred<unknown>()
  const f = await loadView('components/RequirementsSheet', {
    useI18n: () => ({ t: (key: string) => key }),
    currentBmcIdFromLocation: () => 'a',
    systemApi: { requirements: async () => {
      reads++
      if (reads === 2) return pending.promise
      return { requirements: [{ state: 'unknown' }] }
    } }, toast: { success() {} },
    window: { clearTimeout(id: number) { timers.delete(id) }, setTimeout(run: () => void) { timers.set(++timerId, run); return timerId } },
    onActivate(fn: () => void) { activate = fn }, onDeactivate(fn: () => void) { deactivate = fn },
  }, 'refresh')
  await f.refresh()
  assert.equal(timers.size, 1)
  activate()
  assert.equal(reads, 1)
  const read = f.refresh()
  deactivate()
  assert.equal(timers.size, 0)
  pending.resolve({ requirements: [{ state: 'unknown' }] })
  await read
  assert.equal(timers.size, 0)
  activate()
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(reads, 3)
  assert.equal(timers.size, 1)
  deactivate()
  assert.equal(timers.size, 0)
})

test('power confirmation created for one BMC cannot submit against another BMC', async () => {
  let device = 'a', writes = 0
  const f = await loadView('components/PowerPopover', {
    defineProps: () => ({ active: true }), defineEmits: () => () => {},
    useI18n: () => ({ t: (key: string) => key }),
    currentBmcIdFromLocation: () => device,
    useFeatureDataSubscription() {},
    powerStatusResource: { data: { value: {} }, mutate: async (operation: (id: string) => unknown) => operation('mutation') },
    powerApi: { power: async () => { writes++ } }, powerConfigApi: {}, toast: { success() {} },
  }, 'requestAction, handleAction, pendingAction, confirmDialogOpen')
  f.requestAction('off')
  device = 'b'
  await f.handleAction()
  assert.equal(writes, 0)
  assert.equal(f.pendingAction.value, null)
  assert.equal(f.confirmDialogOpen.value, false)
  device = 'a'
  f.requestAction('reset')
  await f.handleAction()
  assert.equal(writes, 1)
})
