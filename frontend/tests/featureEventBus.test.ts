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
let sequence = 0

async function fixture() {
  const sockets: FakeSocket[] = []
  class FakeSocket {
    static OPEN = 1
    static CONNECTING = 0
    readyState = 0
    bufferedAmount = 0
    binaryType = ''
    url: string
    sent: unknown[] = []
    onopen: (() => void) | null = null
    onclose: (() => void) | null = null
    onerror: (() => void) | null = null
    onmessage: ((event: { data: ArrayBuffer }) => void) | null = null
    constructor(url: string) { this.url = url; sockets.push(this) }
    close() { this.readyState = 3 }
    send(data: unknown) { this.sent.push(data) }
    acknowledge() {
      this.readyState = 1
      this.onopen?.()
      this.onmessage?.({ data: new Uint8Array([0]).buffer })
    }
  }
  let nextTimer = 0
  const timers = new Map<number, { run: () => void; delay: number }>()
  const location = { pathname: '/devices/device-a', protocol: 'http:', host: 'localhost' }
  const originalWindow = globalThis.window
  const originalSocket = globalThis.WebSocket
  Object.assign(globalThis, {
    WebSocket: FakeSocket,
    window: {
      location, addEventListener() {},
      setTimeout(run: () => void, delay: number) { timers.set(++nextTimer, { run, delay }); return nextTimer },
      clearTimeout(id: number) { timers.delete(id) },
    },
  })
  const vueUrl = pathToFileURL(resolve('node_modules/vue/dist/vue.runtime.esm-bundler.js')).href
  const typesUrl = moduleUrl(readFileSync('src/types/websocket.ts', 'utf8'))
  const source = readFileSync('src/composables/useFeatureEventBus.ts', 'utf8')
    .replace("from 'vue'", `from '${vueUrl}'`)
    .replace("from '@/types/websocket'", `from '${typesUrl}'`)
  const module = await import(moduleUrl(source + `\nexport const fixtureId = ${++sequence}`))
  const bus = module.useFeatureEventBus('hid', '/api/feature/hid/ws', {
    responseOk: 0, responseUnavailable: 1, responseInvalidMessage: 2,
    encode: () => new Uint8Array([4]).buffer,
  })
  return { bus, sockets, timers, location, close() {
    module.disconnectAllFeatureEventBuses()
    Object.assign(globalThis, { window: originalWindow, WebSocket: originalSocket })
  } }
}

test('feature input cannot reconnect or send after its owning device changes', async () => {
  const f = await fixture()
  try {
    const pending = f.bus.connect()
    f.sockets[0]!.acknowledge()
    assert.equal(await pending, true)
    assert.match(f.sockets[0]!.url, /\/bmc\/device-a\//)
    f.location.pathname = '/devices/device-b'
    await assert.rejects(f.bus.send({}), /another device/)
    assert.equal(await f.bus.connect(), false)
    assert.equal(f.sockets.length, 1)
    assert.equal(f.sockets[0]!.sent.length, 0)
    assert.equal(f.bus.connected.value, false)
  } finally { f.close() }
})

test('disconnect settles a pending handshake and old callbacks cannot alter a replacement', async () => {
  const f = await fixture()
  try {
    const first = f.bus.connect()
    const old = f.sockets[0]!
    f.bus.disconnect()
    assert.equal(await first, false)
    const second = f.bus.connect()
    old.acknowledge()
    old.onclose?.()
    old.onerror?.()
    assert.equal(f.bus.connected.value, false)
    f.sockets[1]!.acknowledge()
    assert.equal(await second, true)
    old.onclose?.()
    assert.equal(f.bus.connected.value, true)
    assert.equal(f.timers.size, 0)
  } finally { f.close() }
})

test('handshake deadline settles the waiter and rejects late acknowledgements', async () => {
  const f = await fixture()
  try {
    const pending = f.bus.connect()
    const deadline = [...f.timers.values()].find(timer => timer.delay === 30_000)!
    deadline.run()
    assert.equal(await pending, false)
    f.sockets[0]!.onmessage?.({ data: new Uint8Array([0]).buffer })
    assert.equal(f.bus.connected.value, false)
    assert.equal(f.bus.networkError.value, true)
  } finally { f.close() }
})

test('manual reconnect cancels the scheduled retry and stays single-flight', async () => {
  const f = await fixture()
  try {
    const first = f.bus.connect()
    f.sockets[0]!.acknowledge()
    await first
    f.sockets[0]!.close()
    f.sockets[0]!.onclose?.()
    assert.equal([...f.timers.values()].filter(timer => timer.delay === 3000).length, 1)
    const second = f.bus.connect()
    assert.equal([...f.timers.values()].filter(timer => timer.delay === 3000).length, 0)
    const same = f.bus.connect()
    assert.equal(f.sockets.length, 2)
    f.sockets[1]!.acknowledge()
    assert.equal(await second, true)
    assert.equal(await same, true)
  } finally { f.close() }
})
