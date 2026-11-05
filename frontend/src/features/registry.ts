import type { Component } from 'vue'

export interface FeatureComponents {
  StatusCard?: Component
  ActionBar?: Component[]
  Overlay?: Component[]
}

export interface FeatureDefinition {
  id: string
  label?: string
  store?: () => void
  components?: FeatureComponents
  onActivate?: () => void | Promise<void>
  onDeactivate?: () => void
}

class FeatureRegistry {
  private features = new Map<string, FeatureDefinition>()

  register(def: FeatureDefinition) {
    if (this.features.has(def.id)) {
      console.warn(`[FeatureRegistry] Overwriting feature: ${def.id}`)
    }
    this.features.set(def.id, def)
  }

  get(id: string): FeatureDefinition | undefined {
    return this.features.get(id)
  }

  getAll(): FeatureDefinition[] {
    return Array.from(this.features.values())
  }

  getStatusCards(): Component[] {
    const cards: Component[] = []
    for (const f of this.features.values()) {
      if (f.components?.StatusCard) {
        cards.push(f.components.StatusCard)
      }
    }
    return cards
  }

  getStatusCard(id: string): Component | undefined {
    return this.features.get(id)?.components?.StatusCard
  }

  getActionBarComponents(): Component[] {
    const list: Component[] = []
    for (const f of this.features.values()) {
      if (f.components?.ActionBar) {
        list.push(...f.components.ActionBar)
      }
    }
    return list
  }

  getActionBarComponent(id: string): Component | undefined {
    return this.features.get(id)?.components?.ActionBar?.[0]
  }

  getOverlayComponents(): Component[] {
    const list: Component[] = []
    for (const f of this.features.values()) {
      if (f.components?.Overlay) {
        list.push(...f.components.Overlay)
      }
    }
    return list
  }

  getOverlayComponent(id: string): Component | undefined {
    return this.features.get(id)?.components?.Overlay?.[0]
  }

  initStores() {
    for (const f of this.features.values()) {
      f.store?.()
    }
  }

  async activateAll() {
    await Promise.all(
      Array.from(this.features.values()).map(f => f.onActivate?.())
    )
  }

  deactivateAll() {
    for (const f of this.features.values()) {
      f.onDeactivate?.()
    }
  }
}

export const featureRegistry = new FeatureRegistry()

export function registerFeature(def: FeatureDefinition) {
  featureRegistry.register(def)
}
