import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import {
  configurationApi,
  type ConfigurationCatalog,
  type DeviceConfiguration,
  type DeviceConfigurationMutation,
  type OptionSnapshot,
} from '@/configuration/catalog'
import { ApiError } from '@/api/request'
import { RequestOrder } from '@/configuration/requestOrder'

export const useDeviceConfigurationStore = defineStore('device-configuration', () => {
  const catalog = ref<ConfigurationCatalog | null>(null)
  const devices = ref<DeviceConfiguration[]>([])
  const selected = ref<DeviceConfiguration | null>(null)
  const options = ref<Record<string, OptionSnapshot>>({})
  const pendingReads = ref(0)
  const writing = ref(false)
  const loading = computed(() => pendingReads.value > 0 || writing.value)
  const error = ref<string | null>(null)
  const etag = ref<string | null>(null)
  const saved = ref(false)
  let selectedTarget: string | null = null
  let devicesRequestSerial = 0
  let deviceRequestSerial = 0
  const optionRequestSerial = new Map<string, number>()
  const listOrder = new RequestOrder()
  const deviceOrders = new Map<string, RequestOrder>()

  function deviceOrder(id: string) {
    let order = deviceOrders.get(id)
    if (!order) { order = new RequestOrder(); deviceOrders.set(id, order) }
    return order
  }

  function publishSelected(device: DeviceConfiguration) {
    ++deviceRequestSerial
    selectedTarget = device.id
    selected.value = device
    etag.value = device.version
  }

  function upsert(device: DeviceConfiguration, previousId = device.id) {
    const index = devices.value.findIndex(candidate => candidate.id === previousId)
    if (index >= 0) devices.value[index] = device
    else devices.value.push(device)
  }

  async function ownWrite<R>(id: string | null, operation: () => Promise<R>): Promise<R> {
    if (writing.value) throw new Error('A configuration save is already in progress')
    const order = id === null ? null : deviceOrder(id)
    const ticket = order?.write()
    const listTicket = listOrder.write()
    writing.value = true
    saved.value = false
    try { return await operation() }
    finally {
      if (order && ticket !== undefined) order.finish(ticket)
      listOrder.finish(listTicket)
      writing.value = false
    }
  }

  async function loadCatalog() {
    const response = await configurationApi.catalog()
    catalog.value = response.data
    return response.data
  }

  async function loadDevices() {
    const requestSerial = ++devicesRequestSerial
    const ticket = listOrder.read()
    ++pendingReads.value
    error.value = null
    try {
      const response = await configurationApi.devices()
      if (requestSerial === devicesRequestSerial && listOrder.accepts(ticket)) {
        devices.value = response.data.devices
      }
      return response.data.devices
    } catch (reason) {
      if (requestSerial === devicesRequestSerial && listOrder.accepts(ticket)) {
        error.value = reason instanceof Error ? reason.message : String(reason)
      }
      throw reason
    } finally { --pendingReads.value }
  }

  async function loadDevice(id: string) {
    const requestSerial = ++deviceRequestSerial
    const order = deviceOrder(id)
    const ticket = order.read()
    if (selectedTarget !== id) {
      selected.value = null
      etag.value = null
      saved.value = false
    }
    selectedTarget = id
    ++pendingReads.value
    error.value = null
    const current = () => requestSerial === deviceRequestSerial && order.accepts(ticket)
    try {
      const response = await configurationApi.device(id)
      if (current()) {
        selected.value = response.data.device
        etag.value = response.etag ?? response.data.device.version
      }
      return response.data.device
    } catch (reason) {
      if (current()) {
        if (reason instanceof ApiError && reason.status === 404) {
          selected.value = null
          etag.value = null
        }
        error.value = reason instanceof Error ? reason.message : String(reason)
      }
      throw reason
    } finally { --pendingReads.value }
  }

  async function loadOptions(resourceId: string) {
    const requestSerial = (optionRequestSerial.get(resourceId) ?? 0) + 1
    optionRequestSerial.set(resourceId, requestSerial)
    const response = await configurationApi.options(resourceId)
    if (optionRequestSerial.get(resourceId) === requestSerial) options.value[resourceId] = response.data
    return response.data
  }

  async function create(mutation: DeviceConfigurationMutation) {
    const owner = deviceRequestSerial
    return ownWrite(null, async () => {
      const response = await configurationApi.create(mutation)
      upsert(response.data.device)
      if (owner === deviceRequestSerial) {
        publishSelected(response.data.device)
        etag.value = response.etag ?? response.data.device.version
        saved.value = true
      }
      return response.data.device
    })
  }

  async function update(id: string, mutation: DeviceConfigurationMutation) {
    const version = selected.value?.id === id
      ? (etag.value ?? selected.value.version)
      : devices.value.find(device => device.id === id)?.version
    if (!version) throw new Error('Configuration version is unavailable; reload before saving')
    return ownWrite(id, async () => {
      const response = await configurationApi.update(id, version, mutation)
      upsert(response.data.device, id)
      if (selectedTarget === id) {
        publishSelected(response.data.device)
        etag.value = response.etag ?? response.data.device.version
        saved.value = true
      }
      return response.data.device
    })
  }

  async function remove(id: string) {
    const version = selected.value?.id === id
      ? (etag.value ?? selected.value.version)
      : devices.value.find(device => device.id === id)?.version
    if (!version) throw new Error('Configuration version is unavailable; reload before deleting')
    return ownWrite(id, async () => {
      await configurationApi.remove(id, version)
      devices.value = devices.value.filter(device => device.id !== id)
      if (selectedTarget === id) {
        ++deviceRequestSerial
        selectedTarget = null
        selected.value = null
        etag.value = null
      }
      saved.value = true
    })
  }

  function applyEvent(deviceId: string) {
    const refresh = selectedTarget === deviceId ? loadDevice(deviceId) : loadDevices()
    void refresh.catch(() => undefined)
  }

  const backendCount = computed(() => catalog.value?.backends.length ?? 0)
  return {
    catalog, devices, selected, options, loading, error, etag, saved, backendCount,
    loadCatalog, loadDevices, loadDevice, loadOptions, create, update, remove, applyEvent,
  }
})
