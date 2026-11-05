import type {
  BackendAssignment,
  BackendAssignmentMutation,
  FeatureAssignment,
  FeatureAssignmentMutation,
} from './catalog'

export function backendAssignmentToMutation(backend: BackendAssignment): BackendAssignmentMutation {
  return {
    backend_id: backend.backend_id,
    params: { ...backend.params },
    secrets: Object.fromEntries(
      Object.keys(backend.secrets).map(key => [key, { operation: 'keep' as const }]),
    ),
  }
}

export function featureAssignmentToMutation(assignment: FeatureAssignment): FeatureAssignmentMutation {
  if (assignment.kind === 'disabled') return { kind: 'disabled' }
  if (assignment.kind === 'single') {
    return { kind: 'single', backend: backendAssignmentToMutation(assignment.backend) }
  }
  return {
    kind: 'provider_set',
    providers: assignment.providers.map(provider => ({
      id: provider.id,
      ...backendAssignmentToMutation(provider),
    })),
  }
}

export function patchBackendAssignment(
  current: BackendAssignmentMutation,
  patch: Partial<BackendAssignmentMutation>,
): BackendAssignmentMutation {
  return {
    backend_id: patch.backend_id ?? current.backend_id,
    params: { ...current.params, ...(patch.params ?? {}) },
    secrets: { ...current.secrets, ...(patch.secrets ?? {}) },
  }
}

export function configuredSecrets(
  assignment: FeatureAssignment | undefined,
  backendId: string,
  providerId?: string,
): BackendAssignment['secrets'] {
  if (assignment?.kind === 'single' && providerId == null && assignment.backend.backend_id === backendId) {
    return assignment.backend.secrets
  }
  if (assignment?.kind === 'provider_set' && providerId != null) {
    const provider = assignment.providers.find(candidate => candidate.id === providerId)
    if (provider?.backend_id === backendId) return provider.secrets
  }
  return {}
}

export function nextProviderId(backendId: string, existingIds: Iterable<string>): string {
  const occupied = new Set(existingIds)
  let suffix = 1
  while (occupied.has(`${backendId}-${suffix}`)) suffix += 1
  return `${backendId}-${suffix}`
}

export function moveProvider<T>(providers: readonly T[], index: number, offset: -1 | 1): T[] {
  const destination = index + offset
  if (index < 0 || index >= providers.length || destination < 0 || destination >= providers.length) {
    return [...providers]
  }
  const reordered = [...providers]
  const [provider] = reordered.splice(index, 1)
  if (provider !== undefined) reordered.splice(destination, 0, provider)
  return reordered
}
