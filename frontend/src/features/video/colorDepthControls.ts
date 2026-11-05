export type ColorPerformancePreference = 'performance' | 'video'

interface ColorDepthProfileGroup {
  variant: boolean
}

export function availableColorPreferences(
  profiles: readonly ColorDepthProfileGroup[],
): ColorPerformancePreference[] {
  const preferences: ColorPerformancePreference[] = []
  if (profiles.some(profile => !profile.variant)) preferences.push('performance')
  if (profiles.some(profile => profile.variant)) preferences.push('video')
  return preferences
}

export function colorDepthProfilesForPreference<T extends ColorDepthProfileGroup>(
  profiles: readonly T[],
  preference: ColorPerformancePreference,
): T[] {
  const variant = preference === 'video'
  return profiles.filter(profile => profile.variant === variant)
}

export function resolveColorPreference(
  profiles: readonly ColorDepthProfileGroup[],
  requested: ColorPerformancePreference,
): ColorPerformancePreference | null {
  const available = availableColorPreferences(profiles)
  return available.includes(requested) ? requested : (available[0] ?? null)
}

export function profileForPreferenceChange<
  T extends ColorDepthProfileGroup & { id: string; depth_bpp: number },
>(
  profiles: readonly T[],
  currentProfileId: string,
  preference: ColorPerformancePreference,
): T | null {
  const candidates = colorDepthProfilesForPreference(profiles, preference)
  const current = profiles.find(profile => profile.id === currentProfileId)
  return (
    candidates.find(profile => profile.depth_bpp === current?.depth_bpp)
    ?? candidates[0]
    ?? null
  )
}
