export type ConfigurationEventType = 'configuration.ready' | 'configuration.changed' | 'configuration.resync_required'

export interface ConfigurationEventCursorInput {
  type: ConfigurationEventType
  epoch: string
  revision: number
}

/** Drops delayed events while allowing a ready event to trigger HTTP resync. */
export class ConfigurationEventCursor {
  private currentEpoch: string | null = null
  private currentRevision = 0

  accept(event: ConfigurationEventCursorInput): boolean {
    // A lag notification is an instruction to resync the authoritative HTTP
    // resource. It is meaningful even when the server's current revision is
    // equal to the last event observed by this socket.
    if (event.type === 'configuration.resync_required') {
      if (this.currentEpoch !== event.epoch) {
        this.currentEpoch = event.epoch
        this.currentRevision = event.revision
      } else {
        this.currentRevision = Math.max(this.currentRevision, event.revision)
      }
      return true
    }
    if (event.type === 'configuration.ready') {
      if (this.currentEpoch !== event.epoch) {
        this.currentEpoch = event.epoch
        this.currentRevision = event.revision
        return true
      }
      this.currentEpoch = event.epoch
      this.currentRevision = Math.max(this.currentRevision, event.revision)
      return true
    }
    if (this.currentEpoch !== event.epoch) {
      this.currentEpoch = event.epoch
      this.currentRevision = event.revision
      return true
    }
    if (event.revision <= this.currentRevision) return false
    this.currentRevision = event.revision
    return true
  }

  get epoch() {
    return this.currentEpoch
  }

  get revision() {
    return this.currentRevision
  }
}
