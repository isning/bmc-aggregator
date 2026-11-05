export type BootEmptyState = 'discovering' | 'empty' | 'unavailable'

interface BootPresentationStatus {
  entries?: readonly unknown[]
  providers?: readonly { phase: string }[]
}

export function bootEmptyState(status: BootPresentationStatus | null | undefined): BootEmptyState | null {
  if (status?.entries?.length) return null

  const providers = status?.providers
  if (!providers?.length) return 'unavailable'
  if (providers.some(provider => provider.phase === 'loading')) return 'discovering'
  if (providers.some(provider => provider.phase === 'faulted')) return 'unavailable'
  return 'empty'
}
