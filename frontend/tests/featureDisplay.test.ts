import assert from 'node:assert/strict'
import test from 'node:test'

import { formatFeatureBackend } from '../src/lib/featureDisplay.ts'

test('feature backend ids are presented as stable transport names', () => {
  assert.equal(formatFeatureBackend('idrac-avct-console'), 'iDRAC AVCT console')
  assert.equal(formatFeatureBackend('CH9329'), 'CH9329 serial HID')
  assert.equal(formatFeatureBackend('custom-backend'), 'custom-backend')
  assert.equal(formatFeatureBackend(null), '')
})
