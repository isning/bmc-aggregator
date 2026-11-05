import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import test from 'node:test'
import ts from 'typescript'
import { createPinia, setActivePinia } from 'pinia'

const moduleUrl = (source: string) => 'data:text/javascript;base64,' + Buffer.from(ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
}).outputText).toString('base64')
function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(complete => { resolve = complete })
  return { promise, resolve }
}
function device(id: string, version = 'v1') {
  return { id, version, name: id, endpoint: { host: 'localhost', port: 443 },
    credentials: { username: 'admin', password: { configured: true } }, assignments: {} }
}
const response = (id: string, version = 'v1') => ({ data: { device: device(id, version) }, etag: version })
let sequence = 0
async function fixture(api: object) {
  setActivePinia(createPinia())
  const key = `__deviceConfiguration${++sequence}`
  Object.assign(globalThis, { [key]: api })
  const vue = pathToFileURL(resolve('node_modules/vue/dist/vue.runtime.esm-bundler.js')).href
  const pinia = pathToFileURL(resolve('node_modules/pinia/dist/pinia.mjs')).href
  const order = moduleUrl(readFileSync('src/configuration/requestOrder.ts', 'utf8'))
  const source = readFileSync('src/stores/deviceConfiguration.ts', 'utf8')
    .replace("from 'vue'", `from '${vue}'`).replace("from 'pinia'", `from '${pinia}'`)
    .replace(/import \{\s*configurationApi,[\s\S]*?\} from '@\/configuration\/catalog'/, `const configurationApi = globalThis.${key}`)
    .replace("import { ApiError } from '@/api/request'", 'class ApiError extends Error { status = 0 }')
    .replace("from '@/configuration/requestOrder'", `from '${order}'`)
  const module = await import(moduleUrl(source))
  return { store: module.useDeviceConfigurationStore(), close() { Reflect.deleteProperty(globalThis, key) } }
}

test('device saves reject concurrent writes and invalidate old device/list reads', async () => {
  const oldDevice = deferred<ReturnType<typeof response>>()
  const oldList = deferred<object>()
  const write = deferred<ReturnType<typeof response>>()
  let reads = 0
  let writes = 0
  const f = await fixture({
    device: () => ++reads === 1 ? Promise.resolve(response('a')) : oldDevice.promise,
    devices: () => oldList.promise,
    update: () => { ++writes; return write.promise },
  })
  try {
    await f.store.loadDevice('a')
    const read = f.store.loadDevice('a')
    const list = f.store.loadDevices()
    const save = f.store.update('a', {})
    await assert.rejects(f.store.update('a', {}), /already in progress/)
    write.resolve(response('a', 'v2'))
    await save
    oldDevice.resolve(response('a'))
    oldList.resolve({ data: { devices: [device('a')] } })
    await Promise.all([read, list])
    assert.equal(writes, 1)
    assert.equal(f.store.selected.version, 'v2')
    assert.equal(f.store.etag, 'v2')
    assert.equal(f.store.devices[0].version, 'v2')
  } finally { f.close() }
})

test('a late save cannot replace another selected device or its version', async () => {
  const write = deferred<ReturnType<typeof response>>()
  const f = await fixture({ device: async (id: string) => response(id),
    update: () => write.promise })
  try {
    await f.store.loadDevice('a')
    const save = f.store.update('a', {})
    await f.store.loadDevice('b')
    write.resolve(response('a', 'v2'))
    await save
    assert.equal(f.store.selected.id, 'b')
    assert.equal(f.store.etag, 'v1')
    assert.equal(f.store.devices[0].id, 'a')
  } finally { f.close() }
})

test('delete invalidates old reads without clearing an unrelated device version', async () => {
  const late = deferred<ReturnType<typeof response>>()
  let reads = 0
  const f = await fixture({ device: (id: string) => ++reads === 2 ? late.promise : Promise.resolve(response(id)),
    remove: async () => undefined })
  try {
    await f.store.loadDevice('a')
    const read = f.store.loadDevice('a')
    await f.store.remove('a')
    late.resolve(response('a'))
    await read
    assert.equal(f.store.selected, null)
    await f.store.loadDevice('b')
    f.store.devices.push(device('c'))
    await f.store.remove('c')
    assert.equal(f.store.selected.id, 'b')
    assert.equal(f.store.etag, 'v1')
  } finally { f.close() }
})

test('a completed create cannot switch selection after navigation', async () => {
  const create = deferred<ReturnType<typeof response>>()
  const f = await fixture({ device: async (id: string) => response(id), create: () => create.promise })
  try {
    const pending = f.store.create({})
    await f.store.loadDevice('b')
    create.resolve(response('a'))
    await pending
    assert.equal(f.store.selected.id, 'b')
    assert.equal(f.store.etag, 'v1')
    assert.equal(f.store.devices[0].id, 'a')
  } finally { f.close() }
})

test('loading remains true until all concurrent reads settle', async () => {
  const read = deferred<ReturnType<typeof response>>()
  const list = deferred<object>()
  const f = await fixture({ device: () => read.promise, devices: () => list.promise })
  try {
    const a = f.store.loadDevice('a')
    const b = f.store.loadDevices()
    list.resolve({ data: { devices: [] } })
    await b
    assert.equal(f.store.loading, true)
    read.resolve(response('a'))
    await a
    assert.equal(f.store.loading, false)
  } finally { f.close() }
})
