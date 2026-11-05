import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import test from 'node:test'
import ts from 'typescript'
import { ref } from 'vue'

const vueUrl = pathToFileURL(resolve('node_modules/vue/dist/vue.runtime.esm-bundler.js')).href
const moduleUrl = (source: string) => 'data:text/javascript;base64,' + Buffer.from(ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
}).outputText).toString('base64')
const scopeUrl = moduleUrl(readFileSync('src/api/deviceScope.ts', 'utf8'))
let serial = 0

async function fixture(names: string[], boundary: 'preview' | 'mode' | 'gate' | 'config' | 'capability') {
  const key = `__videoWork${++serial}`
  const calls: string[] = []
  let complete!: () => void
  const pending = new Promise<void>(resolve => { complete = resolve })
  const f = { device: 'A', active: ref(true), http: ref(true), webTransportActive: ref(false), mjpegTimestamp: ref(0), calls, pending }
  Object.assign(globalThis, { [key]: f })
  const ast = ts.createSourceFile('useVideoStream.ts', readFileSync('src/features/video/useVideoStream.ts', 'utf8'), ts.ScriptTarget.Latest, true)
  const body = ast.statements.find(statement => ts.isFunctionDeclaration(statement) && statement.name?.text === 'useVideoStream')! as ts.FunctionDeclaration
  const statements = body.body!.statements
  const functions = names.map(name => {
    const node = statements.find(statement => ts.isFunctionDeclaration(statement) && statement.name?.text === name)
    assert.ok(node, `missing production function ${name}`)
    return node.getText(ast)
  })
  const ownedDeclarations = statements.filter(statement => ts.isVariableStatement(statement) && statement.declarationList.declarations.some(declaration =>
    ['videoDeviceId', 'videoWorkScope', 'videoDisposed', 'stopVideoActivationWatch'].includes(declaration.name.getText(ast))))
  const suspend = statements.find(statement => ts.isFunctionDeclaration(statement) && statement.name?.text === 'suspendVideoWork')
  const capture = statements.find(statement => ts.isFunctionDeclaration(statement) && statement.name?.text === 'captureVideoWork')
  const ownership = [...ownedDeclarations.map(node => node.getText(ast)), capture?.getText(ast) ?? 'function captureVideoWork() { return () => true }', suspend?.getText(ast) ?? ''].join('\n')
  const source = `
    import { ref, watch } from '${vueUrl}';
    import { DeviceScope } from '${scopeUrl}';
    const f = globalThis.${key};
    const calls = f.calls;
    const currentBmcIdFromLocation = () => f.device;
    const opts = {
      isConsoleActive: f.active,
      webrtc: { isConnected: ref(true), isConnecting: ref(false), disconnect: async () => { calls.push('rtc-disconnect') }, connect: async () => { calls.push('rtc-connect'); return true } },
      webtransport: { disconnect: async () => { calls.push('wt-disconnect') } },
      webrtcVideoRef: ref(null),
      unifiedAudio: { switchMode: () => calls.push('audio-mode') },
      videoSession: { registerTransition: () => calls.push('transition'), waitForModeReady: async () => 'mjpeg', waitForWebRTCReadyAny: async () => f.pending, reset() {} },
    };
    const videoRestarting = ref(false), videoLoading = ref(false), videoError = ref(false), videoErrorMessage = ref('');
    const mjpegTimestamp = f.mjpegTimestamp;
    const mjpegFrameReceived = ref(false), backendFps = ref(0), isHttpMode = f.http, isWebTransportMode = f.webTransportActive, videoMode = ref('mjpeg'), selectedEncoderBackend = ref(null);
    let webrtcReconnectTimeout = null, pendingWebRTCReadyGate = true, webrtcConnectTask = null;
    let retryTimeoutId = null, gracePeriodTimeoutId = null, webrtcRecoveryTimerId = null, mjpegRefreshTimeoutId = null, mjpegFirstFrameDeadlineId = null;
    let initialModeRestoreInProgress = false, isRefreshingVideo = false;
    const cancelWebRTCRecovery = () => {};
    const transitionLifecycle = () => {};
    const captureFrameOverlay = async () => ${boundary === 'preview' ? 'f.pending' : 'undefined'};
    const stopMjpegStream = () => {};
    const refreshVideo = () => calls.push('refresh');
    const setActiveTransport = () => calls.push('transport');
    const connectWebTransportOnly = async () => calls.push('wt-connect');
    const connectWebRTCOnly = async () => calls.push('rtc-connect');
    const switchToWebRTC = async () => calls.push('rtc-switch');
    const shouldUseWebTransport = async () => { ${boundary === 'capability' ? 'await f.pending;' : ''} return true };
    const videoConfigStore = { refreshStream: async () => ${boundary === 'config' ? 'f.pending' : 'undefined'} };
    const normalizeEncoderBackend = value => value;
    const streamApi = {
      reset: async () => calls.push('reset'),
      setMode: async () => { ${boundary === 'mode' ? 'await f.pending;' : ''} return { transition_id: 'transition' } },
      encoderSelfCheck: async () => { calls.push('encoder'); return { current_hardware_encoder: 'software' } },
    };
    const videoDebugLog = () => {};
    ${ownership}
    ${functions.join('\n')}
    export const run = { ${names.join(',')} };
    export const dispose = () => { if (typeof stopVideoActivationWatch === 'function') stopVideoActivationWatch() };
  `
  // Function names used as boundary substitutes are omitted when executing that production function.
  const filtered = source.replace(/const (\w+) = [^\n]+;\n/g, (line, name: string) => names.includes(name) ? '' : line)
  const module = await import(moduleUrl(filtered))
  return { f, run: module.run, complete, close() { module.dispose(); Reflect.deleteProperty(globalThis, key) } }
}

for (const [name, boundary, args] of [
  ['resetVideoStream', 'preview', []],
  ['switchToHttp', 'mode', ['mjpeg']],
  ['refreshVideoBackendStats', 'config', []],
  ['switchToStreaming', 'capability', ['h264']],
] as const) {
  test(`${name} cannot continue after navigating to another device`, async () => {
    const f = await fixture([name], boundary)
    try {
      const pending = f.run[name](...args)
      await Promise.resolve()
      f.f.device = 'B'
      f.complete()
      await pending
      assert.deepEqual(f.f.calls, [])
    } finally { f.close() }
  })
}

test('WebRTC ready-gate completion cannot reactivate work after deactivation and return', async () => {
  const f = await fixture(['connectWebRTCSerial', 'waitForWebRTCReadyGate'], 'gate')
  try {
    const pending = f.run.connectWebRTCSerial('fixture', 'h264')
    f.f.active.value = false
    f.f.active.value = true
    f.complete()
    assert.equal(await pending, false)
    assert.deepEqual(f.f.calls, [])
  } finally { f.close() }
})

test('video sessions belong to individual console owners', async () => {
  const source = readFileSync('src/composables/useVideoSession.ts', 'utf8').replace("from 'vue'", `from '${vueUrl}'`)
  const { useVideoSession } = await import(moduleUrl(source))
  const previous = useVideoSession()
  previous.startLocalSwitch()
  const next = useVideoSession()
  assert.equal(next.localSwitching.value, false)
  next.startLocalSwitch()
  previous.endLocalSwitch()
  assert.equal(next.localSwitching.value, true)
  previous.clearWaiters()
  next.clearWaiters()
})


for (const [name, boundary, args, expected] of [
  ['resetVideoStream', 'preview', [], ['reset', 'rtc-disconnect', 'wt-disconnect', 'refresh']],
  ['switchToHttp', 'mode', ['mjpeg'], ['transition', 'rtc-disconnect', 'wt-disconnect', 'transport', 'audio-mode', 'refresh']],
  ['refreshVideoBackendStats', 'config', [], ['encoder']],
  ['switchToStreaming', 'capability', ['h264'], ['wt-connect']],
] as const) {
  test(`${name} completes its normal active-console path`, async () => {
    const f = await fixture([name], boundary)
    try {
      const pending = f.run[name](...args)
      f.complete()
      await pending
      assert.deepEqual(f.f.calls, [...expected])
    } finally { f.close() }
  })
}

test('an active console still connects after its backend ready gate', async () => {
  const f = await fixture(['connectWebRTCSerial', 'waitForWebRTCReadyGate'], 'gate')
  try {
    const pending = f.run.connectWebRTCSerial('fixture', 'h264')
    f.complete()
    assert.equal(await pending, true)
    assert.deepEqual(f.f.calls, ['rtc-connect'])
  } finally { f.close() }
})

test('resetting a video session settles its own pending work and preserves other owners', async () => {
  const source = readFileSync('src/composables/useVideoSession.ts', 'utf8').replace("from 'vue'", `from '${vueUrl}'`)
  const { useVideoSession } = await import(moduleUrl(source))
  const previous = useVideoSession()
  const next = useVideoSession()
  previous.startLocalSwitch()
  previous.registerTransition('previous')
  const pending = previous.waitForModeReady('previous')
  next.startLocalSwitch()
  previous.reset()
  assert.equal(await pending, null)
  assert.equal(previous.localSwitching.value, false)
  assert.equal(previous.backendSwitching.value, false)
  assert.equal(previous.activeTransitionId.value, null)
  assert.equal(next.localSwitching.value, true)
  next.reset()
})


test('a cached HTTP console starts a fresh stream on return', async () => {
  const f = await fixture(['connectWebRTCSerial', 'waitForWebRTCReadyGate'], 'gate')
  try {
    f.f.mjpegTimestamp.value = 123
    f.f.active.value = false
    f.f.active.value = true
    assert.deepEqual(f.f.calls, ['refresh'])
  } finally { f.close() }
})


for (const transport of ['webrtc', 'webtransport']) {
  test(`video reset preserves an active ${transport} session`, async () => {
    const f = await fixture(['resetVideoStream'], 'preview')
    try {
      f.f.http.value = false
      f.f.webTransportActive.value = transport === 'webtransport'
      const pending = f.run.resetVideoStream()
      f.complete()
      await pending
      assert.deepEqual(f.f.calls, ['reset', 'rtc-disconnect', 'wt-disconnect', transport === 'webtransport' ? 'wt-connect' : 'rtc-connect'])
    } finally { f.close() }
  })
}
