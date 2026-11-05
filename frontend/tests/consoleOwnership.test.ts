import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import * as Vue from 'vue'
import { compileTemplate, parse } from 'vue/compiler-sfc'

interface HostNode { type: string; parent: HostNode | null; children: HostNode[]; text?: string }
const node = (type: string): HostNode => ({ type, parent: null, children: [] })

// Render the production App template with the real KeepAlive implementation.
// Host operations and page contents are replaced; no browser is required.
test('console ownership follows device identity while same-device caching is preserved', async () => {
  const appSource = parse(readFileSync('src/App.vue', 'utf8')).descriptor
  const template = appSource.template!.content
  const ts = await import('typescript')
  const appAst = ts.createSourceFile('App.ts', appSource.scriptSetup!.content, ts.ScriptTarget.Latest, true)
  const state = appAst.statements.find(statement => ts.isVariableStatement(statement)
    && statement.declarationList.declarations.some(declaration => declaration.name.getText(appAst) === 'consoleCacheKey'))!.getText(appAst)
  const observation = appAst.statements.find(statement => ts.isExpressionStatement(statement)
    && ts.isCallExpression(statement.expression) && statement.expression.expression.getText(appAst) === 'watch'
    && statement.getText(appAst).includes('consoleCacheKey'))!.getText(appAst)
  const compiled = compileTemplate({ source: template, filename: 'App.vue', id: 'ownership', compilerOptions: { mode: 'function' } })
  assert.deepEqual(compiled.errors, [])
  const render = new Function('Vue', compiled.code)(Vue)
  const route = Vue.ref<{ name: string; params: { bmcId?: string }; query: string }>({ name: 'Console', params: { bmcId: 'a' }, query: '' })
  const mounted: string[] = []
  const unmounted: string[] = []
  const Console = Vue.defineComponent({ setup() {
    const owner = route.value.params.bmcId!
    Vue.onMounted(() => mounted.push(owner))
    Vue.onUnmounted(() => unmounted.push(owner))
    return () => Vue.h('span', owner)
  } })
  const Settings = Vue.defineComponent({ render: () => Vue.h('div') })
  const RouterView = Vue.defineComponent({ setup(_props, { slots }) {
    return () => slots.default!({ Component: route.value.name === 'Console' ? Console : Settings, route: route.value })
  } })
  const renderer = Vue.createRenderer<HostNode, HostNode>({
    createElement: node, createText: text => ({ ...node('text'), text }), createComment: text => ({ ...node('comment'), text }),
    setText: (target, text) => { target.text = text }, setElementText: (target, text) => { target.text = text },
    parentNode: target => target.parent,
    nextSibling: target => target.parent?.children[target.parent.children.indexOf(target) + 1] ?? null,
    patchProp() {},
    insert(target, parent, anchor = null) {
      if (target.parent) target.parent.children.splice(target.parent.children.indexOf(target), 1)
      target.parent = parent
      const index = anchor ? parent.children.indexOf(anchor) : -1
      parent.children.splice(index < 0 ? parent.children.length : index, 0, target)
    },
    remove(target) {
      if (target.parent) target.parent.children.splice(target.parent.children.indexOf(target), 1)
      target.parent = null
    },
  })
  const app = renderer.createApp({
    components: { RouterView, Toaster: Vue.defineComponent({ render: () => null }) },
    setup: () => ({ authStore: { isAuthenticated: true }, toasterToastOptions: {}, t: (key: string) => key,
      consoleCacheKey: new Function('ref', 'watch', 'router', `${state}\n${observation}\nreturn consoleCacheKey;`)(Vue.ref, Vue.watch, { currentRoute: route }),
    }),
    render,
  })
  app.mount(node('root'))
  try {
    assert.deepEqual(mounted, ['a'])
    route.value = { name: 'Console', params: { bmcId: 'b' }, query: '' }
    await Vue.nextTick()
    assert.deepEqual(mounted, ['a', 'b'], 'the new device must construct its own device-bound transports')
    assert.deepEqual(unmounted, ['a'], 'the previous device must be evicted')
    route.value = { name: 'Settings', params: { bmcId: 'b' }, query: '' }
    await Vue.nextTick()
    route.value = { name: 'Console', params: { bmcId: 'b' }, query: 'same-device' }
    await Vue.nextTick()
    assert.deepEqual(mounted, ['a', 'b'], 'returning to the same console keeps its instance')
    route.value = { name: 'Devices', params: {}, query: '' }
    await Vue.nextTick()
    route.value = { name: 'Console', params: { bmcId: 'b' }, query: '' }
    await Vue.nextTick()
    assert.deepEqual(mounted, ['a', 'b'], 'device-list navigation preserves the most recent console')
    route.value = { name: 'Console', params: { bmcId: 'a' }, query: '' }
    await Vue.nextTick()
    assert.deepEqual(mounted, ['a', 'b', 'a'])
    assert.deepEqual(unmounted, ['a', 'b'])
  } finally { app.unmount() }
})

async function inputFixture() {
  const ts = await import('typescript')
  const js = (source: string) => ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }).outputText
  const scopeModule = await import('data:text/javascript;base64,' + Buffer.from(js(readFileSync('src/api/deviceScope.ts', 'utf8'))).toString('base64'))
  let scope = 'a'
  let sends = 0
  let consumers = 0
  let connects = 0
  let fail = false
  let keyboardFail = false
  const rejectedKeys = new Set<unknown>()
  const keyboard: Array<{ type: string; key: unknown }> = []
  const deliveredKeyboard: Array<{ type: string; key: unknown }> = []
  let timerId = 0
  const timers = new Map<number, () => void>()
  const notifications: string[] = []
  const bus = {
    connected: Vue.ref(true), lastSendError: Vue.ref<string | null>(null),
    connect: async () => { connects++; return true },
    sendMouse: async () => { sends++; if (fail) throw new Error('busy') },
    sendConsumer: async () => { consumers++; if (fail) throw new Error('busy') },
    sendKeyboard: async (event: { type: string; key: unknown }) => {
      keyboard.push(event)
      if (keyboardFail || rejectedKeys.has(event.key)) throw new Error('backpressure')
      deliveredKeyboard.push(event)
    },
  }
  const fixture = {
    ...Vue, DeviceScope: scopeModule.DeviceScope, currentBmcIdFromLocation: () => scope,
    useSystemStore: () => ({ hid: null }), useI18n: () => ({ t: (key: string) => key }),
    useHidStore: () => ({ mouseMode: Vue.ref('absolute'), cursorVisible: Vue.ref(true), syncMouseModeFromConfig() {} }),
    storeToRefs: (store: unknown) => store, useHidEventBus: () => bus,
    keyboardEventToCanonicalKey: (code: string) => code === 'KeyB' ? 5 : 4, updateModifierMaskForKey: () => 0,
    toast: { error: (message: string) => notifications.push(message) },
  }
  const original = { window: globalThis.window, document: globalThis.document, localStorage: globalThis.localStorage }
  Object.assign(globalThis, {
    window: { setTimeout: (run: () => void) => { timers.set(++timerId, run); return timerId } },
    document: { pointerLockElement: null }, localStorage: { getItem: () => null },
  })
  const source = readFileSync('src/features/hid/useHidInput.ts', 'utf8')
    .replace(/^import .*\n/gm, '').replace('export function useHidInput', 'function useHidInput')
  const names = Object.keys(fixture).join(',')
  const useInput = new Function('fixture', `const {${names}} = fixture; ${js(source)}; return useHidInput;`)(fixture)
  const owner = Vue.effectScope()
  const options = {
    videoContainerRef: Vue.ref<HTMLElement | null>(null), isHttpMode: Vue.ref(true), isFullscreen: Vue.ref(false),
    videoRef: Vue.ref(null), webrtcVideoRef: Vue.ref(null), webTransportCanvasRef: Vue.ref(null), webrtc: {},
  }
  const input = owner.run(() => useInput(options))!
  const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve() }
  return { input, timers, bus, flush, notifications, options, keyboard, deliveredKeyboard,
    failKeyboard: (value: boolean) => { keyboardFail = value },
    rejectKey: (key: number) => { rejectedKeys.add(key) },
    wheel: () => input.handleWheel({ preventDefault() {}, deltaY: 1 }),
    consumer: () => input.handleMouseDown({ preventDefault() {}, button: 3 }),
    fail: (value: boolean) => { fail = value }, device: (id: string) => { scope = id },
    counts: () => ({ sends, consumers, connects }),
    close() { input.dispose(); owner.stop(); Object.assign(globalThis, original) },
  }
}

test('a keyboard release rejected before dispatch is retried after backpressure clears', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.options.videoContainerRef.value = { contains: () => true } as unknown as HTMLElement
    const event = { code: 'KeyA', key: 'a', preventDefault() {}, stopPropagation() {} }
    f.input.handleKeyDown(event)
    await f.flush()
    f.failKeyboard(true)
    f.input.handleKeyUp(event)
    await f.flush()
    f.failKeyboard(false)
    for (const run of f.timers.values()) run()
    f.timers.clear()
    await f.flush()
    assert.deepEqual(f.deliveredKeyboard.map(event => event.type), ['keydown', 'keyup'])
  } finally { f.close() }
})

test('a delayed keyboard release stays before a new press of the same key', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.options.videoContainerRef.value = { contains: () => true } as unknown as HTMLElement
    const event = { code: 'KeyA', key: 'a', preventDefault() {}, stopPropagation() {} }
    f.input.handleKeyDown(event)
    await f.flush()
    f.failKeyboard(true)
    f.input.handleKeyUp(event)
    await f.flush()
    f.failKeyboard(false)
    f.input.handleKeyDown(event)
    for (const run of f.timers.values()) run()
    f.timers.clear()
    await f.flush()
    assert.deepEqual(f.deliveredKeyboard.map(event => event.type), ['keydown', 'keyup', 'keydown'])
    f.input.handleKeyUp(event)
    await f.flush()
    assert.equal(f.deliveredKeyboard.at(-1)?.type, 'keyup')
  } finally { f.close() }
})

test('a pending keyboard release cannot be overtaken by a different key press', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.options.videoContainerRef.value = { contains: () => true } as unknown as HTMLElement
    const a = { code: 'KeyA', key: 'a', preventDefault() {}, stopPropagation() {} }
    const b = { ...a, code: 'KeyB', key: 'b' }
    f.input.handleKeyDown(a)
    await f.flush()
    f.failKeyboard(true)
    f.input.handleKeyUp(a)
    await f.flush()
    f.failKeyboard(false)
    f.input.handleKeyDown(b)
    for (const run of f.timers.values()) run()
    f.timers.clear()
    await f.flush()
    assert.deepEqual(f.deliveredKeyboard, [
      { type: 'keydown', key: 4 }, { type: 'keyup', key: 4 }, { type: 'keydown', key: 5 },
    ])
  } finally { f.close() }
})

test('a retired keyboard release cannot resume after console reactivation', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.options.videoContainerRef.value = { contains: () => true } as unknown as HTMLElement
    const event = { code: 'KeyA', key: 'a', preventDefault() {}, stopPropagation() {} }
    f.input.handleKeyDown(event)
    await f.flush()
    f.failKeyboard(true)
    f.input.handleKeyUp(event)
    await f.flush()
    f.input.dispose()
    f.device('b'); f.device('a')
    f.failKeyboard(false)
    f.input.init()
    for (const run of f.timers.values()) run()
    f.timers.clear()
    await f.flush()
    assert.deepEqual(f.deliveredKeyboard.map(event => event.type), ['keydown'])
  } finally { f.close() }
})

test('disposing HID immediately releases an admitted keyboard press', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.options.videoContainerRef.value = { contains: () => true } as unknown as HTMLElement
    f.input.handleKeyDown({ code: 'KeyA', key: 'a', preventDefault() {}, stopPropagation() {} })
    await f.flush()
    f.input.dispose()
    assert.deepEqual(f.deliveredKeyboard.map(event => event.type), ['keydown', 'keyup'])
  } finally { f.close() }
})

test('virtual and physical keyboard events share release ordering', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.options.videoContainerRef.value = { contains: () => true } as unknown as HTMLElement
    const keyboard = f.input.captureKeyboard()
    await keyboard.send(4, true)
    f.failKeyboard(true)
    const release = keyboard.send(4, false)
    await f.flush()
    f.failKeyboard(false)
    f.input.handleKeyDown({ code: 'KeyB', key: 'b', preventDefault() {}, stopPropagation() {} })
    for (const run of f.timers.values()) run()
    f.timers.clear()
    await release
    await f.flush()
    assert.deepEqual(f.deliveredKeyboard, [
      { type: 'keydown', key: 4 }, { type: 'keyup', key: 4 }, { type: 'keydown', key: 5 },
    ])
  } finally { f.close() }
})

test('a captured virtual keyboard operation cannot release a new activation key', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    const oldKeyboard = f.input.captureKeyboard()
    await oldKeyboard.send(4, true)
    f.input.dispose()
    f.device('b'); f.device('a')
    f.input.init()
    await f.input.captureKeyboard().send(4, true)
    assert.equal(oldKeyboard.current(), false)
    await oldKeyboard.send(4, false)
    assert.deepEqual(f.deliveredKeyboard.map(event => event.type), ['keydown', 'keyup', 'keydown'])
  } finally { f.close() }
})

async function macroFixture(pressedKeys: number[] = []) {
  const ts = await import('typescript')
  const source = parse(readFileSync('src/components/VirtualKeyboard.vue', 'utf8')).descriptor.scriptSetup!.content
  const ast = ts.createSourceFile('VirtualKeyboard.ts', source, ts.ScriptTarget.Latest, true)
  const operation = ast.statements.find(statement => ts.isFunctionDeclaration(statement)
    && statement.name?.text === 'executeMacro')!.getText(ast)
  const js = ts.transpileModule(operation, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
  return new Function('props', 'keys', 'setTimeout', `${js}; return executeMacro;`)(
    { pressedKeys }, { ControlLeft: 4, Delete: 5 }, (run: () => void) => { run(); return 0 },
  )
}

test('a virtual macro stops when its first press is never admitted', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.failKeyboard(true)
    const macro = await macroFixture()
    const pending = macro([{ modifiers: ['ControlLeft'], keys: ['Delete'] }], f.input.captureKeyboard())
    for (let attempt = 0; attempt < 4; attempt++) {
      await f.flush()
      for (const run of f.timers.values()) run()
      f.timers.clear()
    }
    await pending
    assert.equal(f.notifications.length, 1)
    assert.ok(f.keyboard.length > 0)
    assert.ok(f.keyboard.every(event => event.key === 4))
    assert.deepEqual(f.deliveredKeyboard, [])
  } finally { f.close() }
})

test('a partially admitted virtual macro releases its own modifier on failure', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.rejectKey(5)
    const macro = await macroFixture()
    const pending = macro([{ modifiers: ['ControlLeft'], keys: ['Delete'] }], f.input.captureKeyboard())
    for (let attempt = 0; attempt < 4; attempt++) {
      await f.flush()
      for (const run of f.timers.values()) run()
      f.timers.clear()
    }
    await pending
    assert.deepEqual(f.deliveredKeyboard, [{ type: 'keydown', key: 4 }, { type: 'keyup', key: 4 }])
  } finally { f.close() }
})

test('a virtual macro preserves a modifier already held by another input', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    await f.input.captureKeyboard().send(4, true)
    const macro = await macroFixture([4])
    await macro([{ modifiers: ['ControlLeft'], keys: ['Delete'] }], f.input.captureKeyboard())
    assert.deepEqual(f.deliveredKeyboard, [
      { type: 'keydown', key: 4 }, { type: 'keydown', key: 5 }, { type: 'keyup', key: 5 },
    ])
  } finally { f.close() }
})

test('disposing HID drops queued mouse and consumer input before dispatch', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.wheel(); f.consumer()
    f.input.dispose()
    await f.flush()
    assert.deepEqual(f.counts(), { sends: 0, consumers: 0, connects: 1 })
    assert.deepEqual(f.notifications, [])
  } finally { f.close() }
})

test('HID retries cannot revive after disposal, device change and same-device reactivation', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.fail(true)
    f.wheel()
    await f.flush()
    assert.equal(f.counts().sends, 1)
    assert.equal(f.timers.size, 1)
    f.input.dispose()
    f.device('b'); f.device('a')
    f.input.init()
    f.fail(false)
    for (const run of f.timers.values()) run()
    f.timers.clear()
    await f.flush()
    assert.equal(f.counts().sends, 1, 'the retired retry must not deliver another scroll')
    f.consumer()
    await f.flush()
    assert.equal(f.counts().consumers, 1, 'new work must not wait behind the retired queue')
    assert.deepEqual(f.notifications, [])
  } finally { f.close() }
})

test('late console initialization stops after deactivation without starting another request', async () => {
  const ts = await import('typescript')
  const script = parse(readFileSync('src/views/ConsoleView.vue', 'utf8')).descriptor.scriptSetup!.content
  const ast = ts.createSourceFile('Console.ts', script, ts.ScriptTarget.Latest, true)
  const statement = ast.statements.find(statement => ts.isExpressionStatement(statement)
    && ts.isCallExpression(statement.expression) && statement.expression.expression.getText(ast) === 'onMounted')!
  assert.ok(ts.isExpressionStatement(statement) && ts.isCallExpression(statement.expression))
  const callback = statement.expression.arguments[0]!.getText(ast)
  let active = true
  let complete!: () => void
  let extraRequests = 0
  const fixture = {
    captureConsoleWork: () => () => active, setupVideoWatchers() {}, stopHandles: [], watch() { return () => {} },
    wsConnected: Vue.ref(false), wsNetworkError: Vue.ref(false), hidBus: { connected: Vue.ref(false), networkError: Vue.ref(false) },
    systemStore: { resetDeviceState() {}, fetchSystemInfo: () => new Promise<void>(resolve => { complete = resolve }), fetchAllStates: async () => { extraRequests++ } },
    hidConfigStore: { reset() {}, refreshHid: async () => { extraRequests++ } },
    videoStream: { refreshVideoBackendStats: async () => { extraRequests++ } },
  }
  const initialize = new Function('fixture', `const {${Object.keys(fixture).join(',')}} = fixture; return (${callback});`)(fixture)
  const pending = initialize()
  active = false
  complete()
  await pending
  assert.equal(extraRequests, 0)
})


test('a pending keyboard reconnect cannot repeat a key after console reactivation', async () => {
  const f = await inputFixture()
  try {
    f.input.init()
    f.options.videoContainerRef.value = { contains: () => true } as unknown as HTMLElement
    f.bus.connected.value = false
    let ready!: (value: boolean) => void
    f.bus.connect = () => new Promise<boolean>(resolve => { ready = resolve })
    f.failKeyboard(true)
    f.input.handleKeyDown({ code: 'KeyA', key: 'a', preventDefault() {}, stopPropagation() {} })
    await f.flush()
    const finishOldAttempt = ready
    f.input.dispose()
    f.failKeyboard(false)
    // A fresh press of the same key must not validate the retired retry.
    f.bus.connect = async () => true
    f.input.init()
    await f.input.captureKeyboard().send(4, true)
    finishOldAttempt(true)
    await f.flush()
    assert.equal(f.deliveredKeyboard.filter(event => event.type === 'keydown').length, 1)
  } finally { f.close() }
})
