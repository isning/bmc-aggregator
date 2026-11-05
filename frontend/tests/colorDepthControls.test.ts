import assert from 'node:assert/strict'
import test from 'node:test'

import {
  availableColorPreferences,
  colorDepthProfilesForPreference,
  profileForPreferenceChange,
  resolveColorPreference,
} from '../src/features/video/colorDepthControls.ts'

const idrac6Profiles = [
  { id: 'variant-1-15', depth_bpp: 15, variant: true },
]

const idrac8Profiles = [
  { id: 'base-4', depth_bpp: 4, variant: false },
  { id: 'base-7', depth_bpp: 7, variant: false },
  { id: 'variant-1-7', depth_bpp: 7, variant: true },
  { id: 'variant-1-23', depth_bpp: 23, variant: true },
]

const hardwareConfirmedMatrix = [
  ...[4, 5, 6, 7].map(depth => ({ id: `base-${depth}`, depth_bpp: depth, variant: false })),
  ...[4, 5, 6, 7, 9, 12, 15, 18, 21, 23].map(depth => ({
    id: `variant-1-${depth}`,
    depth_bpp: depth,
    variant: true,
  })),
]

test('a single backend profile exposes only its supported preference', () => {
  assert.deepEqual(availableColorPreferences(idrac6Profiles), ['video'])
  assert.equal(resolveColorPreference(idrac6Profiles, 'performance'), 'video')
  assert.deepEqual(
    colorDepthProfilesForPreference(idrac6Profiles, 'video'),
    idrac6Profiles,
  )
})

test('mixed backend profiles preserve both preference groups and their depths', () => {
  assert.deepEqual(availableColorPreferences(idrac8Profiles), ['performance', 'video'])
  assert.equal(resolveColorPreference(idrac8Profiles, 'video'), 'video')
  assert.deepEqual(
    colorDepthProfilesForPreference(idrac8Profiles, 'performance').map(profile => profile.depth_bpp),
    [4, 7],
  )
  assert.deepEqual(
    colorDepthProfilesForPreference(idrac8Profiles, 'video').map(profile => profile.depth_bpp),
    [7, 23],
  )
})

test('preference changes preserve depth when possible and otherwise choose the first profile', () => {
  assert.equal(
    profileForPreferenceChange(idrac8Profiles, 'base-7', 'video')?.id,
    'variant-1-7',
  )
  assert.equal(
    profileForPreferenceChange(idrac8Profiles, 'variant-1-23', 'performance')?.id,
    'base-4',
  )
})

test('an empty capability set has no invented preference', () => {
  assert.deepEqual(availableColorPreferences([]), [])
  assert.equal(resolveColorPreference([], 'video'), null)
  assert.deepEqual(colorDepthProfilesForPreference([], 'video'), [])
})

test('preference changes preserve every depth supported by both modes', () => {
  for (const depth of [4, 5, 6, 7]) {
    assert.equal(
      profileForPreferenceChange(hardwareConfirmedMatrix, `base-${depth}`, 'video')?.id,
      `variant-1-${depth}`,
    )
    assert.equal(
      profileForPreferenceChange(hardwareConfirmedMatrix, `variant-1-${depth}`, 'performance')?.id,
      `base-${depth}`,
    )
  }
  assert.equal(
    profileForPreferenceChange(hardwareConfirmedMatrix, 'variant-1-23', 'performance')?.id,
    'base-4',
  )
})
