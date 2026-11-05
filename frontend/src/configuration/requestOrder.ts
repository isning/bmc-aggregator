/** Orders reads against writes within one configuration resource. */
export class RequestOrder {
  private revision = 0
  private writing = false

  read(): number { return this.revision }

  write(): number {
    if (this.writing) throw new Error('A configuration save is already in progress')
    this.writing = true
    return ++this.revision
  }

  finish(ticket: number): void {
    if (!this.accepts(ticket)) return
    this.writing = false
    ++this.revision
  }

  reset(): void {
    this.writing = false
    ++this.revision
  }

  accepts(ticket: number): boolean { return ticket === this.revision }
}
