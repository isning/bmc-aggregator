import { defineStore } from 'pinia'

export const useMsdStore = defineStore('feature-msd', () => {
  function reset() {
    // Future: reset MSD UI state here
  }

  return {
    reset,
  }
})
