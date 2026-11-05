export function isNewerRevision(current: number, incoming: number): boolean {
  return incoming > current
}

export class RevisionEpoch {
  private current: string | null = null
  private retired = new Set<string>()

  private sequence(epoch: string): bigint | null {
    return /^\d+$/.test(epoch) ? BigInt(epoch) : null
  }

  reset(): void {
    this.current = null
    this.retired.clear()
  }

  adopt(incoming: string | undefined): 'same' | 'changed' | 'retired' {
    if (!incoming || incoming === this.current) return 'same'
    if (this.retired.has(incoming)) return 'retired'
    if (this.current) {
      const currentSequence = this.sequence(this.current)
      const incomingSequence = this.sequence(incoming)
      if (currentSequence !== null && incomingSequence !== null && incomingSequence < currentSequence) {
        this.retired.add(incoming)
        return 'retired'
      }
    }
    if (this.current) this.retired.add(this.current)
    this.current = incoming
    return 'changed'
  }
}

export class FeatureLeaseRegistry {
  private legacy = new Set<string>()
  private refs = new Map<string, number>()

  setLegacy(features: Iterable<string>): void {
    this.legacy = new Set(features)
  }

  acquire(feature: string): () => void {
    this.refs.set(feature, (this.refs.get(feature) ?? 0) + 1)
    let released = false
    return () => {
      if (released) return
      released = true
      const refs = Math.max(0, (this.refs.get(feature) ?? 0) - 1)
      if (refs === 0) this.refs.delete(feature)
      else this.refs.set(feature, refs)
    }
  }

  values(): string[] {
    return Array.from(new Set([
      ...this.legacy,
      ...Array.from(this.refs.entries())
        .filter(([, refs]) => refs > 0)
        .map(([feature]) => feature),
    ])).sort()
  }
}
