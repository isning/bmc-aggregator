import assert from 'node:assert/strict'
import { createServer as createHttpServer, request } from 'node:http'
import type { Socket } from 'node:net'
import { once } from 'node:events'
import test from 'node:test'
import { createServer, type ProxyOptions, type UserConfig } from 'vite'
import config from '../vite.config.ts'

// Exercise the production proxy settings against an actual HTTP upgrade peer.
test('development proxy forwards root and device WebSocket upgrades', async () => {
  const sockets = new Set<Socket>()
  const paths: string[] = []
  const authorities: { host: string | undefined; origin: string | undefined }[] = []
  const backend = createHttpServer()
  backend.on('connection', socket => {
    sockets.add(socket)
    socket.on('close', () => sockets.delete(socket))
  })
  backend.on('upgrade', (req, socket) => {
    paths.push(req.url!)
    authorities.push({ host: req.headers.host, origin: req.headers.origin })
    socket.write('HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n')
  })
  backend.listen(0, '127.0.0.1')
  await once(backend, 'listening')
  const address = backend.address()
  assert.ok(address && typeof address !== 'string')
  const target = `http://127.0.0.1:${address.port}`
  const production = config as UserConfig
  const proxy = Object.fromEntries(Object.entries(production.server!.proxy!).map(([path, options]) => {
    assert.equal(typeof options, 'object')
    return [path, { ...(options as ProxyOptions), target }]
  }))
  let vite: Awaited<ReturnType<typeof createServer>> | undefined
  try {
    vite = await createServer({
      ...production,
      configFile: false,
      logLevel: 'silent',
      server: { ...production.server, host: '127.0.0.1', port: 0, hmr: false, proxy },
    })
    await vite.listen()
    const frontend = vite.httpServer!.address()
    assert.ok(frontend && typeof frontend !== 'string')
    const browserHost = `127.0.0.1:${frontend.port}`
    const browserOrigin = `http://${browserHost}`
    for (const path of ['/api/configuration/ws', '/bmc/test/api/ws', '/bmc/test/api/feature/hid/ws']) {
      await new Promise<void>((resolve, reject) => {
        const req = request({ host: '127.0.0.1', port: frontend.port, path,
          headers: { Connection: 'Upgrade', Upgrade: 'websocket', Origin: browserOrigin },
        })
        req.setTimeout(2000, () => req.destroy(new Error(`WebSocket upgrade was not forwarded: ${path}`)))
        req.on('error', reject)
        req.on('response', response => {
          response.resume()
          reject(new Error(`Expected upgrade, received ${response.statusCode}`))
        })
        req.on('upgrade', (response, socket) => {
          socket.destroy()
          try { assert.equal(response.statusCode, 101); resolve() } catch (error) { reject(error) }
        })
        req.end()
      })
    }
    assert.deepEqual(authorities, Array.from({ length: 3 }, () => ({ host: browserHost, origin: browserOrigin })))
    assert.deepEqual(paths, ['/api/configuration/ws', '/bmc/test/api/ws', '/bmc/test/api/feature/hid/ws'])
  } finally {
    for (const socket of sockets) socket.destroy()
    await vite?.close()
    await new Promise<void>(resolve => backend.close(() => resolve()))
  }
})
