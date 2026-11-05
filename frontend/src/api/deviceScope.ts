/** Owns the identity and lifetime of device-derived asynchronous work. */
export class DeviceScope {
  private generation = 0

  constructor(private readonly identity: () => string | null) {}

  reset(): void { ++this.generation }

  capture() {
    const id = this.identity()
    const generation = this.generation
    return {
      id,
      current: () => generation === this.generation && id === this.identity(),
    }
  }
}
