import assert from 'node:assert/strict'
import test from 'node:test'
import { backendAssignmentToMutation, featureAssignmentToMutation, moveProvider, nextProviderId, patchBackendAssignment } from '../src/configuration/assignment.ts'
import { ConfigurationEventCursor } from '../src/configuration/eventCursor.ts'

test('canonical secrets become keep mutations without disclosing values', () => {
  const mutation = backendAssignmentToMutation({
    backend_id: 'example',
    params: { port: 443 },
    secrets: { password: { configured: true } },
  })
  assert.deepEqual(mutation.secrets, { password: { operation: 'keep' } })
  assert.equal(JSON.stringify(mutation).includes('configured'), false)
})

test('provider-set conversion preserves order and secret mutations survive parameter edits', () => {
  const mutation = featureAssignmentToMutation({
    kind: 'provider_set',
    providers: [
      { id: 'primary', backend_id: 'one', params: { port: 1 }, secrets: {} },
      { id: 'fallback', backend_id: 'two', params: { port: 2 }, secrets: {} },
    ],
  })
  assert.deepEqual(mutation.kind === 'provider_set' ? mutation.providers.map(provider => provider.id) : [], ['primary', 'fallback'])
  const edited = patchBackendAssignment(mutation.kind === 'provider_set' ? mutation.providers[0] : { backend_id: '', params: {}, secrets: {} }, {
    params: { port: 3 },
    secrets: { token: { operation: 'replace', value: 'new' } },
  })
  const editedAgain = patchBackendAssignment(edited, { params: { host: 'bmc.local' } })
  assert.deepEqual(editedAgain.secrets.token, { operation: 'replace', value: 'new' })
  assert.equal(editedAgain.params.port, 3)
})

test('configuration cursor accepts reconnect ready, rejects stale changes, and adopts new epochs', () => {
  const cursor = new ConfigurationEventCursor()
  assert.equal(cursor.accept({ type: 'configuration.ready', epoch: 'a', revision: 4 }), true)
  assert.equal(cursor.accept({ type: 'configuration.changed', epoch: 'a', revision: 3 }), false)
  assert.equal(cursor.accept({ type: 'configuration.changed', epoch: 'a', revision: 5 }), true)
  assert.equal(cursor.accept({ type: 'configuration.ready', epoch: 'a', revision: 5 }), true)
  assert.equal(cursor.accept({ type: 'configuration.ready', epoch: 'b', revision: 1 }), true)
  assert.equal(cursor.accept({ type: 'configuration.changed', epoch: 'b', revision: 2 }), true)
  assert.equal(cursor.epoch, 'b')
  assert.equal(cursor.revision, 2)
})

test('configuration cursor always delivers lag notifications for authoritative resync', () => {
  const cursor = new ConfigurationEventCursor()
  assert.equal(cursor.accept({ type: 'configuration.ready', epoch: 'a', revision: 8 }), true)
  assert.equal(cursor.accept({ type: 'configuration.resync_required', epoch: 'a', revision: 8 }), true)
  assert.equal(cursor.accept({ type: 'configuration.resync_required', epoch: 'a', revision: 7 }), true)
  assert.equal(cursor.revision, 8)
})

test('provider IDs remain unique after removal and provider order is explicit', () => {
  assert.equal(nextProviderId('ipmi', ['ipmi-1', 'ipmi-3']), 'ipmi-2')
  assert.deepEqual(moveProvider(['one', 'two', 'three'], 2, -1), ['one', 'three', 'two'])
  assert.deepEqual(moveProvider(['one', 'two'], 0, -1), ['one', 'two'])
})
