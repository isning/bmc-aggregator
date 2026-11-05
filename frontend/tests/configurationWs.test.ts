import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import ts from 'typescript'

const moduleUrl = (source: string) => 'data:text/javascript;base64,' + Buffer.from(
  ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022,
  } }).outputText,
).toString('base64')
let sequence = 0

async function fixture(onEvent?: () => Promise<void>) {
  const hooks: { mount?: () => void; unmount?: () => void } = {}
  const sockets: FakeSocket[] = []
  let failConstruction = false
  class FakeSocket {
    onmessage: ((event: { data: string }) => void) | null = null
    onclose: (() => void) | null = null
    close() {}
    constructor() {
      if (failConstruction) throw new Error('temporarily unavailable')
      sockets.push(this)
    }
    emit(value: unknown) { this.onmessage?.({ data: JSON.stringify(value) }) }
  }
  let timerId = 0
  const timers = new Map<number, () => void>()
  const originals = { window: globalThis.window, WebSocket: globalThis.WebSocket, location: globalThis.location }
  const key = `__configurationWs${++sequence}`
  Object.assign(globalThis, { [key]: hooks, WebSocket: FakeSocket, location: { origin: 'http://localhost' }, window: {
    setTimeout(run: () => void) { timers.set(++timerId, run); return timerId },
    clearTimeout(id: number) { timers.delete(id) },
  } })
  const vue = moduleUrl(`export const onMounted = fn => globalThis.${key}.mount = fn;
    export const onBeforeUnmount = fn => globalThis.${key}.unmount = fn`)
  const cursor = moduleUrl(readFileSync('src/configuration/eventCursor.ts', 'utf8'))
  const source = readFileSync('src/configuration/ws.ts', 'utf8')
    .replace("from 'vue'", `from '${vue}'`)
    .replace("from './eventCursor'", `from '${cursor}'`)
  const module = await import(moduleUrl(source))
  const received: unknown[] = []
  const subscription = module.useConfigurationEvents((event: unknown) => { received.push(event); return onEvent?.() })
  return { hooks, sockets, timers, received, subscription,
    failConstruction(value: boolean) { failConstruction = value },
    runRetry() {
      const [id, run] = [...timers][0]!
      timers.delete(id)
      run()
    },
    close() { hooks.unmount?.(); Object.assign(globalThis, originals); Reflect.deleteProperty(globalThis, key) },
  }
}
const ready = { type: 'configuration.ready', epoch: 'one', revision: 1 }

test('configuration subscription rejects callbacks after close and cancels queued retries', async () => {
  const f = await fixture()
  try {
    f.hooks.mount!()
    f.sockets[0]!.emit(ready)
    assert.equal(f.received.length, 1)
    f.sockets[0]!.onclose!()
    assert.equal(f.timers.size, 1)
    f.subscription.close()
    assert.equal(f.timers.size, 0)
    f.sockets[0]!.emit({ ...ready, revision: 2 })
    assert.equal(f.received.length, 1)
  } finally { f.close() }
})

test('obsolete socket callbacks cannot replace the current connection or rewind its cursor', async () => {
  const f = await fixture()
  try {
    f.hooks.mount!()
    const old = f.sockets[0]!
    old.onclose!()
    f.runRetry()
    f.sockets[1]!.emit({ ...ready, epoch: 'two', revision: 10 })
    old.onclose!()
    old.emit(ready)
    assert.equal(f.timers.size, 0)
    f.sockets[1]!.emit({ type: 'configuration.changed', epoch: 'two', revision: 9 })
    assert.equal(f.received.length, 1)
  } finally { f.close() }
})

test('invalid configuration events do not advance the cursor and rejected refreshes are observed', async () => {
  const f = await fixture(async () => { throw new Error('HTTP unavailable') })
  try {
    f.hooks.mount!()
    for (const event of [null, { ...ready, type: 'unknown' }, { ...ready, revision: -1 },
      { ...ready, revision: 1.5 }, { ...ready, revision: Number.MAX_SAFE_INTEGER + 1 },
      { ...ready, scope: null }, { ...ready, changed_paths: [3] }]) f.sockets[0]!.emit(event)
    assert.equal(f.received.length, 0)
    f.sockets[0]!.emit(ready)
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(f.received.length, 1)
  } finally { f.close() }
})

test('constructor failure retries without throwing from mount and unmount stops delivery', async () => {
  const f = await fixture()
  try {
    f.failConstruction(true)
    assert.doesNotThrow(() => f.hooks.mount!())
    assert.equal(f.timers.size, 1)
    f.failConstruction(false)
    f.runRetry()
    assert.equal(f.sockets.length, 1)
    f.hooks.unmount!()
    f.sockets[0]!.emit(ready)
    f.sockets[0]!.onclose!()
    assert.equal(f.received.length, 0)
    assert.equal(f.timers.size, 0)
  } finally { f.close() }
})
