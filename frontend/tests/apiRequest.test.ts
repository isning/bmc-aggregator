import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import ts from 'typescript'

const source = readFileSync('src/api/request.ts', 'utf8')
  .replace("import { toast } from 'vue-sonner'", 'const toast = { error() {} }')
  .replace("import i18n from '@/i18n'", 'const i18n = { global: { t: key => key } }')
const moduleUrl = 'data:text/javascript;base64,' + Buffer.from(ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
}).outputText).toString('base64')
const api = await import(moduleUrl)

test('API preserves conditional mutation headers for every supported HeadersInit shape', async () => {
  const original = globalThis.fetch
  const seen: Headers[] = []
  globalThis.fetch = async (_input, options) => {
    seen.push(new Headers(options?.headers))
    return new Response('{}', { headers: { 'content-type': 'application/json' } })
  }
  try {
    for (const headers of [
      { 'If-Match': 'v1', 'X-Mutation-Id': 'mutation' },
      new Headers({ 'If-Match': 'v1', 'X-Mutation-Id': 'mutation' }),
      [['If-Match', 'v1'], ['X-Mutation-Id', 'mutation']] as [string, string][],
    ]) {
      await api.rootRequestMeta('/server/web', { method: 'PATCH', headers, body: '{}' })
      assert.equal(seen.at(-1)!.get('if-match'), 'v1')
      assert.equal(seen.at(-1)!.get('x-mutation-id'), 'mutation')
      assert.equal(seen.at(-1)!.get('content-type'), 'application/json')
    }
  } finally { globalThis.fetch = original }
})

test('API respects explicit case-insensitive content types and browser multipart boundaries', async () => {
  const original = globalThis.fetch
  const seen: Headers[] = []
  globalThis.fetch = async (_input, options) => {
    seen.push(new Headers(options?.headers))
    return new Response(null, { status: 204 })
  }
  try {
    const headers = new Headers({ 'content-type': 'application/octet-stream' })
    await api.rootRequest('/upload', { method: 'POST', headers, body: 'data' })
    assert.equal(seen[0]!.get('content-type'), 'application/octet-stream')
    assert.equal(headers.get('content-type'), 'application/octet-stream')
    await api.rootRequest('/upload', { method: 'POST', body: new FormData() })
    assert.equal(seen[1]!.has('content-type'), false)
  } finally { globalThis.fetch = original }
})
