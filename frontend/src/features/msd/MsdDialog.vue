<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed, watch, toRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { toast } from 'vue-sonner'
import { useSystemStore } from '@/stores/system'
import {
  msdApi,
  type DriveFile,
  type MsdDownloadStatus,
  type MsdImage,
  type MsdMediaKind,
  type MsdStorageLocation,
  type MsdStorageOperation,
} from '@/api'
import MsdMediaTypeSelect from './MsdMediaTypeSelect.vue'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Progress } from '@/components/ui/progress'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Separator } from '@/components/ui/separator'
import {
  HardDrive,
  Upload,
  Trash2,
  Link,
  Unlink,
  Disc,
  File,
  Folder,
  FolderPlus,
  Download,
  RefreshCw,
  ChevronRight,
  ArrowLeft,
  Globe,
  X,
  AlertCircle,
  Plus,
  Copy,
  Server,
  Cloud,
  CircleDot,
  Save,
  MemoryStick,
  Usb,
} from 'lucide-vue-next'
import { useFeatureDataSubscription } from '@/feature-data/resource'
import { msdCatalogResources, msdStatusResource } from '@/feature-data/resources'

const props = defineProps<{
  open: boolean
}>()
useFeatureDataSubscription(msdStatusResource, toRef(props, 'open'))

const emit = defineEmits<{
  (e: 'update:open', value: boolean): void
}>()

const { t } = useI18n()
const systemStore = useSystemStore()
const activeTab = ref('images')

const images = ref<MsdImage[]>([])
const loadingImages = ref(false)
const uploadProgress = ref(0)
const uploading = ref(false)

type PendingConnect =
  | { kind: 'image'; slotId: string; location: MsdStorageLocation; imageId: string }
  | { kind: 'remote'; slotId: string }
  | { kind: 'drive'; slotId: string }

const pendingConnect = ref<PendingConnect>()
const connecting = computed(() => pendingConnect.value !== undefined)
const disconnecting = ref(false)
const deleting = ref(false)
const showTargetCreateDialog = ref(false)
const showTargetDeleteDialog = ref(false)
const targetMediaType = ref<MsdMediaKind>()
const targetLabel = ref('')
const targetMutationInProgress = ref(false)

const driveFiles = ref<DriveFile[]>([])
const currentPath = ref('/')
const loadingDrive = ref(false)
const driveInfo = ref<import('@/api').MsdDriveInfo | null>(null)
const driveInitialized = ref(false)
const uploadingFile = ref(false)
const fileUploadProgress = ref(0)

const showDeleteDialog = ref(false)
const deleteTarget = ref<{
  type: 'image' | 'file'
  id: string
  name: string
  location?: MsdStorageLocation
} | null>(null)
const showNewFolderDialog = ref(false)
const newFolderName = ref('')

const showDriveInitDialog = ref(false)
const showDeleteDriveDialog = ref(false)
const selectedDriveSize = ref(256) // Default 256MB
const customDriveSize = ref<number | undefined>(undefined)
const initializingDrive = ref(false)
const deletingDrive = ref(false)

const showUrlDialog = ref(false)
const remoteMediaUrl = ref('')
const downloadUrl = ref('')
const downloadFilename = ref('')
const downloadUserAgent = ref('')
const downloadHeaders = ref<Array<{ name: string; value: string }>>([])
const showDownloadAdvanced = ref(false)
const downloading = ref(false)
const downloadProgress = ref<MsdDownloadStatus | null>(null)
let downloadCompleteTimeout: number | null = null
let downloadPollTimer: number | null = null
let downloadPollGeneration = 0

const TWO_POINT_TWO_GB = 2.2 * 1024 * 1024 * 1024

function copyDigest(value: string) {
  void navigator.clipboard.writeText(value)
}

const msdCapabilities = computed(() => systemStore.msd?.capabilities ?? {
  virtual_drive: false,
  slots: [],
  storage_locations: [],
  unavailable_storage_locations: [],
  target_management: { media_types: [], deletable: false, limits: [], max_targets: null },
})
const selectedSlotId = ref('')
const selectedSlot = computed(() => msdCapabilities.value.slots.find(
  (slot) => slot.id === selectedSlotId.value,
))
const currentAttachment = computed(() => systemStore.msd?.attachments.find(
  (attachment) => attachment.slot_id === selectedSlotId.value,
))
const msdConnected = computed(() => currentAttachment.value !== undefined)
const externallyOccupied = computed(() => systemStore.msd?.occupancy.state === 'external')
const canCreateTarget = computed(() => msdCapabilities.value.target_management.media_types.length > 0)
const targetTypeAtCapacity = computed(() => {
  if (!targetMediaType.value) return false
  const management = msdCapabilities.value.target_management
  const limit = (management.limits ?? []).find(
    (candidate) => candidate.media_type === targetMediaType.value,
  )?.max_targets ?? management.max_targets
  if (limit === null || limit === undefined) return false
  return msdCapabilities.value.slots.filter(
    (slot) => slot.dynamic && slot.media_types.includes(targetMediaType.value!),
  ).length >= limit
})
const canDeleteTarget = computed(() => (
  msdCapabilities.value.target_management.deletable
  && selectedSlot.value?.dynamic === true
  && selectedSlotId.value !== ''
  && !msdConnected.value
))
const msdMode = computed(() => currentAttachment.value?.mode ?? 'none')
const remoteStorage = computed(() => msdCapabilities.value.storage_locations.find(
  (storage) => storage.location === 'remote',
))
const catalogStorages = computed(() => msdCapabilities.value.storage_locations.filter(
  (storage) => storage.location !== 'remote'
    && (storage.operations.includes('list') || storage.operations.includes('upload')),
))
const selectedCatalogLocation = ref<MsdStorageLocation>()
const selectedSourceLocation = ref<MsdStorageLocation>()
const selectedStorage = computed(() => catalogStorages.value.find(
  (storage) => storage.location === selectedCatalogLocation.value,
))
const aggregatorCatalogActive = computed(() => props.open && selectedCatalogLocation.value === 'aggregator')
const bmcCatalogActive = computed(() => props.open && selectedCatalogLocation.value === 'bmc')
useFeatureDataSubscription(msdCatalogResources.aggregator, aggregatorCatalogActive)
useFeatureDataSubscription(msdCatalogResources.bmc, bmcCatalogActive)
watch(
  [msdCatalogResources.aggregator.data, msdCatalogResources.bmc.data, selectedCatalogLocation],
  () => {
    const location = selectedCatalogLocation.value
    if (location === 'aggregator' || location === 'bmc') {
      images.value = msdCatalogResources[location].data.value?.images ?? []
    }
  },
  { immediate: true },
)
const selectedMediaType = ref<MsdMediaKind>()
const remoteMediaType = ref<MsdMediaKind>()
const mediaTypes = computed(() => (selectedStorage.value?.media_types ?? []).filter(
  (mediaType) => selectedSlot.value?.media_types.includes(mediaType),
))
const remoteMediaTypes = computed(() => (remoteStorage.value?.media_types ?? []).filter(
  (mediaType) => selectedSlot.value?.media_types.includes(mediaType),
))

function supportsOperation(
  storage: { operations: MsdStorageOperation[] } | undefined,
  operation: MsdStorageOperation,
): boolean {
  return storage?.operations.includes(operation) ?? false
}

function keepValidMediaType(types: MsdMediaKind[], selected: MsdMediaKind | undefined) {
  return selected && types.includes(selected) ? selected : types[0]
}

function slotLabel(slotId: string): string {
  const slot = msdCapabilities.value.slots.find((candidate) => candidate.id === slotId)
  return slot?.label || slotId.charAt(0).toUpperCase() + slotId.slice(1)
}

function mediaKindLabel(kind: MsdMediaKind): string {
  return { optical: 'Optical', floppy: 'Floppy', disk: 'Disk', flash: 'Flash' }[kind]
}

function storageLabel(location: MsdStorageLocation): string {
  return { aggregator: 'Aggregator', bmc: 'BMC storage', remote: 'Remote URL' }[location]
}

function attachmentForSlot(slotId: string) {
  return systemStore.msd?.attachments.find((attachment) => attachment.slot_id === slotId)
}

function attachmentLabel(slotId: string): string {
  const attachment = attachmentForSlot(slotId)
  if (!attachment) return 'Empty'
  if (attachment.mode === 'drive') return t('msd.drive')
  if (attachment.media?.location === 'remote') return 'Remote URL'
  return attachment.media?.image_id ?? 'External media'
}

async function selectSlot(slotId: string) {
  selectedSlotId.value = slotId
  const slot = msdCapabilities.value.slots.find((candidate) => candidate.id === slotId)
  const availableMediaTypes = (selectedStorage.value?.media_types ?? []).filter(
    (mediaType) => slot?.media_types.includes(mediaType),
  )
  selectedMediaType.value = keepValidMediaType(availableMediaTypes, selectedMediaType.value)

  const attachment = attachmentForSlot(slotId)
  if (!attachment) return
  if (attachment.mode === 'drive') {
    activeTab.value = 'drive'
    return
  }
  activeTab.value = 'images'
  const location = attachment.media?.location
  if (!location) return
  selectedSourceLocation.value = location
  if (location !== 'remote') await selectCatalog(location)
}

function isImageConnecting(imageId: string): boolean {
  const pending = pendingConnect.value
  return pending?.kind === 'image'
    && pending.slotId === selectedSlotId.value
    && pending.location === selectedCatalogLocation.value
    && pending.imageId === imageId
}

function isDriveConnecting(): boolean {
  const pending = pendingConnect.value
  return pending?.kind === 'drive' && pending.slotId === selectedSlotId.value
}

watch(catalogStorages, (storages) => {
  if (!storages.some((storage) => storage.location === selectedCatalogLocation.value)) {
    selectedCatalogLocation.value = storages.find(
      (storage) => storage.location === 'aggregator',
    )?.location ?? storages[0]?.location
  }
}, { immediate: true })
watch(() => msdCapabilities.value.storage_locations, (storages) => {
  if (!storages.some((storage) => storage.location === selectedSourceLocation.value)) {
    selectedSourceLocation.value = storages.find(
      (storage) => storage.location === 'aggregator',
    )?.location ?? storages[0]?.location
  }
}, { immediate: true })
watch(() => msdCapabilities.value.slots, (slots) => {
  if (!slots.some((slot) => slot.id === selectedSlotId.value)) {
    const firstSlotId = slots[0]?.id ?? ''
    if (firstSlotId) void selectSlot(firstSlotId)
    else selectedSlotId.value = ''
  }
}, { immediate: true })
watch(mediaTypes, (types) => {
  selectedMediaType.value = keepValidMediaType(types, selectedMediaType.value)
}, { immediate: true })

watch(() => msdCapabilities.value.target_management.media_types, (types) => {
  if (!targetMediaType.value || !types.includes(targetMediaType.value)) {
    targetMediaType.value = types[0]
  }
}, { immediate: true })

async function createTarget() {
  if (!targetMediaType.value || targetTypeAtCapacity.value || targetMutationInProgress.value) return
  targetMutationInProgress.value = true
  try {
    const result = await msdStatusResource.mutate(mutationId =>
      msdApi.createTarget(targetMediaType.value!, targetLabel.value.trim() || undefined, mutationId),
    )
    selectedSlotId.value = result.target.id
    targetLabel.value = ''
    showTargetCreateDialog.value = false
  } finally {
    targetMutationInProgress.value = false
  }
}

async function removeSelectedTarget() {
  if (!canDeleteTarget.value || targetMutationInProgress.value) return
  targetMutationInProgress.value = true
  try {
    await msdStatusResource.mutate(mutationId => msdApi.deleteTarget(selectedSlotId.value, mutationId))
    showTargetDeleteDialog.value = false
  } finally {
    targetMutationInProgress.value = false
  }
}
watch(remoteMediaTypes, (types) => {
  remoteMediaType.value = keepValidMediaType(types, remoteMediaType.value)
}, { immediate: true })

const connectedImageName = computed(() => {
  if (!msdConnected.value) return null
  if (msdMode.value === 'drive') return t('msd.drive')
  const media = currentAttachment.value?.media
  if (!media) return 'External media'
  if (media.location === 'remote') return media.url
  const image = media.location === selectedCatalogLocation.value
    ? images.value.find(i => i.id === media.image_id)
    : undefined
  return image?.name ?? media.image_id
})

function isCurrentImage(imageId: string): boolean {
  const media = currentAttachment.value?.media
  if (!msdConnected.value || !media || media.location === 'remote') return false
  return media.location === selectedCatalogLocation.value && media.image_id === imageId
}

const operationInProgress = computed(() => {
  return connecting.value ||
         disconnecting.value ||
         deleting.value ||
         uploading.value ||
         uploadingFile.value ||
         initializingDrive.value ||
         deletingDrive.value
})

function isLargeFile(image: MsdImage): boolean {
  return (image.size ?? 0) > TWO_POINT_TWO_GB
}

const breadcrumbs = computed(() => {
  const parts = currentPath.value.split('/').filter(Boolean)
  const crumbs = [{ name: '/', path: '/' }]
  let path = ''
  for (const part of parts) {
    path += '/' + part
    crumbs.push({ name: part, path })
  }
  return crumbs
})

watch(() => props.open, async (isOpen) => {
  if (isOpen) {
    await loadData()
  }
})

watch(() => msdCapabilities.value.virtual_drive, (supported) => {
  if (!supported && activeTab.value === 'drive') activeTab.value = 'images'
})

async function loadData() {
  await systemStore.fetchMsdState(true)
  if (!catalogStorages.value.some(
    (storage) => storage.location === selectedCatalogLocation.value,
  )) {
    selectedCatalogLocation.value = catalogStorages.value.find(
      (storage) => storage.location === 'aggregator',
    )?.location ?? catalogStorages.value[0]?.location
  }
  if (!msdCapabilities.value.storage_locations.some(
    (storage) => storage.location === selectedSourceLocation.value,
  )) {
    selectedSourceLocation.value = selectedCatalogLocation.value
      ?? msdCapabilities.value.storage_locations[0]?.location
  }
  await loadImages()
  await loadDriveInfo()
  if (driveInitialized.value) {
    await loadDriveFiles()
  }
  await restoreUrlDownload()
}

async function restoreUrlDownload() {
  if (downloadProgress.value?.status === 'downloading') return
  const restored = await msdApi.resumePendingDownload().catch(error => {
    toast.error(error instanceof Error ? error.message : 'Failed to restore download')
    return null
  })
  if (!restored || restored.status === 'completed' || restored.status === 'cancelled') return
  downloading.value = true
  downloadProgress.value = {
    ok: restored.ok,
    download_id: restored.download_id,
    filename: restored.filename,
    bytes_downloaded: restored.bytes_downloaded,
    total_bytes: restored.total_bytes,
    download_speed_bps: restored.download_speed_bps,
    status: restored.status,
  }
  startDownloadPolling()
}

async function selectCatalog(location: MsdStorageLocation) {
  if (selectedCatalogLocation.value === location) return
  selectedCatalogLocation.value = location
  await loadImages()
}

async function selectSourceLocation(location: MsdStorageLocation) {
  if (selectedSourceLocation.value === location) return
  selectedSourceLocation.value = location
  if (location !== 'remote') await selectCatalog(location)
}

async function loadImages() {
  loadingImages.value = true
  try {
    const storage = selectedStorage.value
    if (!storage || !supportsOperation(storage, 'list')) {
      images.value = []
      return
    }
    if (storage.location === 'aggregator' || storage.location === 'bmc') {
      images.value = (await msdCatalogResources[storage.location].refresh()).images
    }
  } catch (e) {
    console.error('Failed to load images:', e)
  } finally {
    loadingImages.value = false
  }
}

async function handleImageUpload(e: Event) {
  const input = e.target as HTMLInputElement
  const file = input.files?.[0]
  if (!file) return

  uploading.value = true
  uploadProgress.value = 0

  try {
    const location = selectedStorage.value?.location
    if (!location) return
    await msdApi.uploadImage(location, file, selectedMediaType.value, (progress) => {
      uploadProgress.value = progress
    })
    if (location === 'aggregator' || location === 'bmc') {
      await msdCatalogResources[location].refresh()
    }
  } catch (e) {
    console.error('Failed to upload image:', e)
  } finally {
    uploading.value = false
    uploadProgress.value = 0
    input.value = ''
  }
}

async function connectImage(image: MsdImage) {
  if (msdConnected.value || operationInProgress.value) {
    toast.warning(t('msd.operationInProgress'))
    return
  }

  const slotId = selectedSlotId.value
  const location = selectedStorage.value?.location
  const mediaType = selectedMediaType.value
  if (location !== 'aggregator' && location !== 'bmc') return
  pendingConnect.value = { kind: 'image', slotId, location, imageId: image.id }
  try {
    await msdStatusResource.mutate(mutationId => msdApi.connect(
        slotId,
        'image',
        { location, image_id: image.id },
        mediaType,
        mutationId,
      ))
  } catch (e) {
    console.error('Failed to connect image:', e)
  } finally {
    pendingConnect.value = undefined
  }
}

async function connectRemoteMedia() {
  const url = remoteMediaUrl.value.trim()
  if (!url.startsWith('https://')) {
    toast.error('Redfish virtual media requires an HTTPS image URL')
    return
  }
  if (msdConnected.value || operationInProgress.value) {
    toast.warning(t('msd.operationInProgress'))
    return
  }
  const slotId = selectedSlotId.value
  const mediaType = remoteMediaType.value
  pendingConnect.value = { kind: 'remote', slotId }
  try {
    await msdStatusResource.mutate(mutationId => msdApi.connect(
        slotId,
        'image',
        { location: 'remote', url },
        mediaType,
        mutationId,
      ))
  } catch (e) {
    console.error('Failed to connect remote virtual media:', e)
  } finally {
    pendingConnect.value = undefined
  }
}

async function connectDrive() {
  if (msdConnected.value || operationInProgress.value) {
    toast.warning(t('msd.operationInProgress'))
    return
  }

  const slotId = selectedSlotId.value
  pendingConnect.value = { kind: 'drive', slotId }
  try {
    await msdStatusResource.mutate(mutationId => msdApi.connect(slotId, 'drive', undefined, undefined, mutationId))
  } catch (e) {
    console.error('Failed to connect drive:', e)
  } finally {
    pendingConnect.value = undefined
  }
}

async function disconnect() {
  if (operationInProgress.value) {
    return
  }

  disconnecting.value = true
  try {
    await msdStatusResource.mutate(mutationId => msdApi.disconnect(selectedSlotId.value, mutationId))
  } catch (e) {
    console.error('Failed to disconnect:', e)
  } finally {
    disconnecting.value = false
  }
}

function confirmDelete(type: 'image' | 'file', id: string, name: string) {
  deleteTarget.value = {
    type,
    id,
    name,
    location: type === 'image' ? selectedCatalogLocation.value : undefined,
  }
  showDeleteDialog.value = true
}

async function executeDelete() {
  if (!deleteTarget.value || deleting.value) return

  deleting.value = true
  try {
    if (deleteTarget.value.type === 'image') {
      if (!deleteTarget.value.location) return
      await msdApi.deleteImage(deleteTarget.value.location, deleteTarget.value.id)
      images.value = images.value.filter(i => i.id !== deleteTarget.value!.id)
    } else {
      await msdApi.deleteDriveFile(deleteTarget.value.id)
      await loadDriveFiles()
    }
  } catch (e) {
    console.error('Failed to delete:', e)
  } finally {
    showDeleteDialog.value = false
    deleteTarget.value = null
    deleting.value = false
  }
}

async function loadDriveInfo() {
  try {
    driveInfo.value = await msdApi.driveInfo()
    driveInitialized.value = true
  } catch {
    driveInitialized.value = false
  }
}

const driveSizeOptions = computed(() => [
  { value: 64, label: '64 MB' },
  { value: 128, label: '128 MB' },
  { value: 256, label: `256 MB (${t('common.recommended')})`, recommended: true },
  { value: 512, label: '512 MB' },
  { value: 1024, label: '1 GB' },
  { value: 2048, label: '2 GB' },
  { value: 4096, label: '4 GB' },
  { value: 8192, label: '8 GB' },
])

const finalDriveSize = computed(() => {
  return customDriveSize.value || selectedDriveSize.value
})

function initializeDrive() {
  showDriveInitDialog.value = true
}

async function createDrive() {
  initializingDrive.value = true
  try {
    await msdApi.initDrive(finalDriveSize.value)
    await loadDriveInfo()
    await loadDriveFiles()
    showDriveInitDialog.value = false
  } catch (e) {
    console.error('Failed to initialize drive:', e)
  } finally {
    initializingDrive.value = false
  }
}

async function deleteDrive() {
  deletingDrive.value = true
  try {
    await msdApi.deleteDrive()
    driveInitialized.value = false
    driveInfo.value = null
    driveFiles.value = []
    currentPath.value = '/'
    showDeleteDriveDialog.value = false
  } catch (e) {
    console.error('Failed to delete drive:', e)
  } finally {
    deletingDrive.value = false
  }
}

async function loadDriveFiles() {
  loadingDrive.value = true
  try {
    driveFiles.value = await msdApi.listDriveFiles(currentPath.value)
  } catch (e) {
    console.error('Failed to load drive files:', e)
  } finally {
    loadingDrive.value = false
  }
}

function navigateTo(path: string) {
  currentPath.value = path
  loadDriveFiles()
}

function navigateUp() {
  const parts = currentPath.value.split('/').filter(Boolean)
  parts.pop()
  currentPath.value = '/' + parts.join('/')
  loadDriveFiles()
}

async function handleFileUpload(e: Event) {
  const input = e.target as HTMLInputElement
  const file = input.files?.[0]
  if (!file) return

  uploadingFile.value = true
  fileUploadProgress.value = 0

  try {
    await msdApi.uploadDriveFile(file, currentPath.value, (progress) => {
      fileUploadProgress.value = progress
    })
    await loadDriveFiles()
  } catch (e) {
    console.error('Failed to upload file:', e)
  } finally {
    uploadingFile.value = false
    fileUploadProgress.value = 0
    input.value = ''
  }
}

async function createFolder() {
  if (!newFolderName.value.trim()) return

  try {
    const path = currentPath.value === '/'
      ? '/' + newFolderName.value
      : currentPath.value + '/' + newFolderName.value
    await msdApi.createDirectory(path)
    await loadDriveFiles()
  } catch (e) {
    console.error('Failed to create folder:', e)
  } finally {
    showNewFolderDialog.value = false
    newFolderName.value = ''
  }
}

async function startUrlDownload() {
  if (!downloadUrl.value.trim()) return

  downloading.value = true
  try {
    const headers = Object.fromEntries(
      downloadHeaders.value
        .map(({ name, value }) => [name.trim(), value] as const)
        .filter(([name]) => name.length > 0),
    )
    if (downloadUserAgent.value.trim()) headers['User-Agent'] = downloadUserAgent.value.trim()
    const result = await msdApi.downloadFromUrl(
      downloadUrl.value.trim(),
      downloadFilename.value.trim() || undefined,
      headers,
    )
    downloadProgress.value = {
      ok: result.ok,
      download_id: result.download_id,
      filename: result.filename,
      bytes_downloaded: 0,
      total_bytes: null,
      download_speed_bps: 0,
      status: result.status,
    }
    await refreshDownloadStatus()
    if (
      downloadProgress.value?.status === 'downloading'
      || downloadProgress.value?.status === 'cancelling'
    ) {
      startDownloadPolling()
    }
  } catch (e) {
    console.error('Failed to start download:', e)
    downloading.value = false
  }
}

async function cancelUrlDownload() {
  if (!downloadProgress.value) return

  try {
    await msdApi.cancelDownload(downloadProgress.value.download_id)
    resetDownloadState()
  } catch (e) {
    console.error('Failed to cancel download:', e)
    toast.error(e instanceof Error ? e.message : 'Failed to cancel download')
  }
}

function resetDownloadState() {
  if (downloadCompleteTimeout !== null) {
    window.clearTimeout(downloadCompleteTimeout)
    downloadCompleteTimeout = null
  }
  stopDownloadPolling()
  downloading.value = false
  downloadProgress.value = null
  downloadUrl.value = ''
  downloadFilename.value = ''
}

async function refreshDownloadStatus() {
  const current = downloadProgress.value
  if (!current) return
  try {
    const status = await msdApi.downloadStatus(current.download_id)
    if (downloadProgress.value?.download_id !== status.download_id) return
    downloadProgress.value = status

    if (status.status === 'completed') {
      msdApi.clearPendingDownload()
      await loadImages()
      stopDownloadPolling()
      if (downloadCompleteTimeout !== null) window.clearTimeout(downloadCompleteTimeout)
      downloadCompleteTimeout = window.setTimeout(() => {
        downloadCompleteTimeout = null
        showUrlDialog.value = false
        resetDownloadState()
      }, 1000)
    } else if (status.status === 'cancelled' || status.status === 'failed') {
      downloading.value = false
      stopDownloadPolling()
    }
  } catch (error) {
    console.error('Failed to query download status:', error)
    downloading.value = false
    stopDownloadPolling()
  }
}

function stopDownloadPolling() {
  downloadPollGeneration += 1
  if (downloadPollTimer !== null) window.clearTimeout(downloadPollTimer)
  downloadPollTimer = null
}

function startDownloadPolling() {
  stopDownloadPolling()
  const generation = downloadPollGeneration
  const schedule = () => {
    downloadPollTimer = window.setTimeout(() => {
      downloadPollTimer = null
      void refreshDownloadStatus().finally(() => {
        const status = downloadProgress.value?.status
        if (
          generation === downloadPollGeneration
          && (status === 'downloading' || status === 'cancelling')
        ) {
          schedule()
        }
      })
    }, 750)
  }
  schedule()
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return bytes + ' B'
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB'
  if (bytes < 1024 * 1024 * 1024) return (bytes / 1024 / 1024).toFixed(1) + ' MB'
  return (bytes / 1024 / 1024 / 1024).toFixed(1) + ' GB'
}

function formatSpeed(bps: number): string {
  if (bps < 1024) return bps.toFixed(0) + ' B/s'
  if (bps < 1024 * 1024) return (bps / 1024).toFixed(1) + ' KB/s'
  return (bps / 1024 / 1024).toFixed(1) + ' MB/s'
}

onMounted(async () => {
  if (props.open) {
    await loadData()
  }
})

onUnmounted(() => {
  if (downloadCompleteTimeout !== null) {
    window.clearTimeout(downloadCompleteTimeout)
    downloadCompleteTimeout = null
  }
  stopDownloadPolling()
})
</script>

<template>
  <TooltipProvider>
    <Dialog :open="open" @update:open="emit('update:open', $event)">
      <DialogContent class="w-[calc(100vw-2rem)] sm:max-w-[980px] h-[min(760px,90vh)] overflow-hidden flex flex-col p-0">
      <DialogHeader class="px-6 pt-6 shrink-0">
        <DialogTitle class="flex items-center gap-2">
          <HardDrive class="h-5 w-5" />
          {{ t('msd.title') }}
        </DialogTitle>
        <DialogDescription class="mt-1">
          {{ msdCapabilities.slots.length }} targets · {{ systemStore.msd?.attachments.length ?? 0 }} mounted
        </DialogDescription>
      </DialogHeader>

      <Separator class="shrink-0" />

      <div v-if="externallyOccupied" class="flex-1 min-h-0 flex items-center justify-center px-6 py-10">
        <div class="max-w-md text-center">
          <div class="mx-auto h-10 w-10 rounded-md border bg-muted flex items-center justify-center">
            <MemoryStick class="h-5 w-5 text-muted-foreground" />
          </div>
          <h3 class="mt-4 text-sm font-semibold">{{ t('msd.externalSessionTitle') }}</h3>
          <p class="mt-1.5 text-xs leading-5 text-muted-foreground">
            {{ t('msd.externalSessionDescription') }}
          </p>
        </div>
      </div>

      <div v-else class="flex-1 min-h-0 grid grid-cols-1 grid-rows-[auto_minmax(0,1fr)] sm:grid-cols-[220px_minmax(0,1fr)] sm:grid-rows-1">
        <aside class="min-h-0 border-b sm:border-b-0 sm:border-r bg-muted/20 flex flex-col">
          <div class="hidden sm:flex px-4 py-3 border-b items-center justify-between">
            <div>
              <p class="text-xs font-semibold uppercase text-muted-foreground">Targets</p>
              <p class="text-[11px] text-muted-foreground mt-0.5">Select where media is mounted</p>
            </div>
            <Button v-if="canCreateTarget" size="icon" variant="ghost" class="h-8 w-8" :title="t('msd.createTarget')" @click="showTargetCreateDialog = true">
              <Plus class="h-4 w-4" />
            </Button>
          </div>
          <div class="flex sm:block gap-1 sm:space-y-1 overflow-x-auto sm:overflow-x-hidden sm:overflow-y-auto p-2">
            <Button
              v-for="slot in msdCapabilities.slots"
              :key="slot.id"
              variant="ghost"
              class="h-auto min-w-[150px] sm:min-w-0 sm:w-full justify-start px-3 py-2.5 text-left border border-transparent"
              :class="selectedSlotId === slot.id
                ? 'border-border bg-background text-foreground shadow-sm hover:bg-background'
                : 'text-muted-foreground hover:bg-muted/70 hover:text-foreground'"
              @click="selectSlot(slot.id)"
            >
              <Disc v-if="slot.media_types.length === 1 && slot.media_types[0] === 'optical'" class="h-4 w-4 shrink-0 self-start mt-0.5" />
              <Save v-else-if="slot.media_types.length === 1 && slot.media_types[0] === 'floppy'" class="h-4 w-4 shrink-0 self-start mt-0.5" />
              <HardDrive v-else-if="slot.media_types.length === 1 && slot.media_types[0] === 'disk'" class="h-4 w-4 shrink-0 self-start mt-0.5" />
              <MemoryStick v-else-if="slot.media_types.length === 1 && slot.media_types[0] === 'flash'" class="h-4 w-4 shrink-0 self-start mt-0.5" />
              <Usb v-else class="h-4 w-4 shrink-0 self-start mt-0.5" />
              <span class="min-w-0 flex-1">
                <span class="flex items-center gap-2">
                  <span class="font-medium truncate">{{ slotLabel(slot.id) }}</span>
                  <span v-if="attachmentForSlot(slot.id)" class="ml-auto h-2 w-2 rounded-full bg-green-500 shrink-0" />
                </span>
                <span class="mt-1 flex flex-wrap gap-1">
                  <span v-for="kind in slot.media_types" :key="kind" class="inline-flex items-center gap-0.5 text-[10px] font-normal text-muted-foreground">
                    <Disc v-if="kind === 'optical'" class="h-2.5 w-2.5" />
                    <Save v-else-if="kind === 'floppy'" class="h-2.5 w-2.5" />
                    <HardDrive v-else-if="kind === 'disk'" class="h-2.5 w-2.5" />
                    <MemoryStick v-else class="h-2.5 w-2.5" />
                    {{ mediaKindLabel(kind) }}
                  </span>
                </span>
                <span class="block mt-1 text-[11px] font-normal truncate" :class="attachmentForSlot(slot.id) ? 'text-green-600 dark:text-green-400' : 'text-muted-foreground'">
                  {{ attachmentLabel(slot.id) }}
                </span>
              </span>
            </Button>
            <Button v-if="canCreateTarget" variant="outline" class="sm:hidden h-auto min-w-[96px] border-dashed" @click="showTargetCreateDialog = true">
              <Plus class="h-4 w-4 mr-1.5" />
              {{ t('msd.createTarget') }}
            </Button>
          </div>
        </aside>

        <div class="min-h-0 flex flex-col px-5 pb-5 pt-4">
          <div class="shrink-0 flex items-start justify-between gap-4 pb-3">
            <div class="min-w-0">
              <div class="flex items-center gap-2">
                <h3 class="font-semibold truncate">{{ selectedSlot ? slotLabel(selectedSlot.id) : 'No target selected' }}</h3>
                <Badge v-if="msdConnected" variant="outline" class="h-5 border-green-500/40 text-green-600 dark:text-green-400">Mounted</Badge>
              </div>
              <p class="text-xs text-muted-foreground mt-1 truncate">
                {{ msdConnected ? connectedImageName : 'Choose media from a storage location' }}
              </p>
            </div>
            <div class="flex items-center gap-1.5 shrink-0">
              <Button v-if="msdConnected" variant="outline" size="sm" class="h-8 text-destructive" :disabled="operationInProgress" @click="disconnect">
                <Unlink class="h-3.5 w-3.5 mr-1.5" />
                {{ disconnecting ? `${t('common.disconnecting')}...` : t('msd.disconnect') }}
              </Button>
              <Button v-if="canDeleteTarget" size="icon" variant="ghost" class="h-8 w-8 text-destructive" :title="t('msd.deleteTarget')" @click="showTargetDeleteDialog = true">
                <Trash2 class="h-4 w-4" />
              </Button>
            </div>
          </div>
        <Tabs v-model="activeTab" class="flex-1 flex flex-col min-h-0">
          <TabsList class="w-full grid shrink-0" :class="msdCapabilities.virtual_drive ? 'grid-cols-2' : 'grid-cols-1'">
          <TabsTrigger value="images">
            <Disc class="h-4 w-4 mr-1.5" />
            {{ t('msd.images') }}
          </TabsTrigger>
          <TabsTrigger v-if="msdCapabilities.virtual_drive" value="drive">
            <HardDrive class="h-4 w-4 mr-1.5" />
            {{ t('msd.drive') }}
          </TabsTrigger>
        </TabsList>

        <!-- Tab Description -->
        <p class="text-xs text-muted-foreground mt-2 mb-1 shrink-0" v-if="msdCapabilities.virtual_drive">
          {{ activeTab === 'images' ? t('msd.imagesDesc') : t('msd.driveDesc') }}
        </p>

          <TabsContent value="images" class="flex-1 min-h-0 m-0 flex flex-col space-y-3">
            <div class="shrink-0 space-y-1.5">
              <div class="flex items-center justify-between">
                <p class="text-xs font-semibold uppercase text-muted-foreground">Storage location</p>
                <p class="text-[11px] text-muted-foreground">Select a source for {{ selectedSlot ? slotLabel(selectedSlot.id) : 'this target' }}</p>
              </div>
              <div class="flex flex-wrap gap-2">
              <Button
                v-for="storage in msdCapabilities.storage_locations"
                :key="storage.location"
                variant="outline"
                class="h-12 min-w-[150px] flex-1 justify-start px-3"
                :class="selectedSourceLocation === storage.location ? 'border-primary bg-primary/5 ring-1 ring-primary/20' : ''"
                @click="selectSourceLocation(storage.location)"
              >
                <Server v-if="storage.location === 'aggregator'" class="h-4 w-4 shrink-0" />
                <HardDrive v-else-if="storage.location === 'bmc'" class="h-4 w-4 shrink-0" />
                <Cloud v-else class="h-4 w-4 shrink-0" />
                <span class="ml-2 min-w-0 text-left">
                  <span class="block text-xs font-medium truncate">{{ storageLabel(storage.location) }}</span>
                  <span class="block text-[10px] font-normal text-muted-foreground truncate">
                    {{ storage.location === 'remote' ? 'Mount by HTTPS URL' : `${storage.operations.includes('upload') ? 'Managed library' : 'Read-only library'}` }}
                  </span>
                </span>
              </Button>
              <Tooltip v-for="storage in msdCapabilities.unavailable_storage_locations" :key="`unavailable-${storage.location}`">
                <TooltipTrigger as-child>
                  <Button variant="outline" disabled class="h-12 min-w-[150px] flex-1 justify-start px-3 border-dashed opacity-60">
                    <HardDrive class="h-4 w-4 shrink-0" />
                    <span class="ml-2 min-w-0 text-left">
                      <span class="block text-xs font-medium truncate">{{ storageLabel(storage.location) }}</span>
                      <span class="block text-[10px] font-normal text-amber-600">Unavailable</span>
                    </span>
                    <AlertCircle class="ml-auto h-3.5 w-3.5 text-amber-600" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent><p class="max-w-xs">{{ t(storage.hint_key) }}</p></TooltipContent>
              </Tooltip>
              </div>
            </div>

            <template v-if="selectedSourceLocation !== 'remote'">
            <MsdMediaTypeSelect
              v-if="supportsOperation(selectedStorage, 'select')"
              v-model="selectedMediaType"
              :types="mediaTypes"
              :disabled="operationInProgress || msdConnected"
            />
            <!-- Compact Upload Toolbar -->
            <div v-if="supportsOperation(selectedStorage, 'upload')" class="shrink-0 flex items-center gap-2 min-w-0">
              <label class="flex-1">
                <input
                  type="file"
                  class="hidden"
                  accept=".iso,.img,.raw,.bin,.ima,.flp,.vfd,.dsk,.fat,.fat12,.fat16,.fat32,.efi,application/octet-stream"
                  :disabled="uploading"
                  @change="handleImageUpload"
                />
                <Button variant="outline" size="sm" as="span" class="w-full cursor-pointer">
                  <Upload class="h-4 w-4 mr-1.5" />
                  {{ t('msd.uploadImage') }}
                </Button>
              </label>
              <Button
                v-if="selectedCatalogLocation === 'aggregator'"
                variant="outline"
                size="sm"
                class="flex-1"
                @click="showUrlDialog = true"
              >
                <Globe class="h-4 w-4 mr-1.5" />
                {{ t('msd.downloadFromUrl') }}
              </Button>
            </div>
            <Progress v-if="uploading" :model-value="uploadProgress" class="h-1 shrink-0" />

            <!-- Image List -->
            <div v-if="supportsOperation(selectedStorage, 'list')" class="flex-1 min-h-0 flex flex-col space-y-2 min-w-0">
              <div class="shrink-0 flex items-center justify-between">
                <div>
                  <h4 class="text-sm font-medium">{{ t('msd.imageList') }}</h4>
                  <p class="text-[11px] text-muted-foreground">{{ images.length }} images in {{ storageLabel(selectedCatalogLocation!) }}</p>
                </div>
                <Button variant="ghost" size="icon" class="h-7 w-7" @click="loadImages">
                  <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loadingImages }" />
                </Button>
              </div>

              <div v-if="images.length === 0" class="shrink-0 text-center py-6 text-muted-foreground text-sm">
                {{ t('msd.noImages') }}
              </div>

              <div v-else class="flex-1 min-h-0 overflow-y-auto pr-2 custom-scrollbar">
                <div class="space-y-2">
                  <div
                    v-for="image in images"
                    :key="image.id"
                    class="p-3 rounded-lg border transition-colors"
                    :class="[
                      isCurrentImage(image.id)
                        ? 'border-primary bg-primary/5'
                        : 'hover:bg-accent/50'
                    ]"
                  >
                    <div class="flex items-start justify-between gap-2">
                      <div class="flex items-start gap-2 w-0 flex-1">
                        <Disc v-if="selectedMediaType === 'optical'" class="h-4 w-4 text-muted-foreground shrink-0 mt-0.5" />
                        <Save v-else-if="selectedMediaType === 'floppy'" class="h-4 w-4 text-muted-foreground shrink-0 mt-0.5" />
                        <HardDrive v-else-if="selectedMediaType === 'disk'" class="h-4 w-4 text-muted-foreground shrink-0 mt-0.5" />
                        <MemoryStick v-else class="h-4 w-4 text-muted-foreground shrink-0 mt-0.5" />
                        <div class="w-0 flex-1">
                          <Tooltip>
                            <TooltipTrigger as-child>
                              <p class="text-sm font-medium cursor-help overflow-hidden text-ellipsis whitespace-nowrap">{{ image.name }}</p>
                            </TooltipTrigger>
                            <TooltipContent>
                              <p class="max-w-sm break-all">{{ image.name }}</p>
                            </TooltipContent>
                          </Tooltip>
                          <div class="flex items-center gap-2 mt-0.5 flex-wrap">
                            <span v-if="image.size !== null" class="text-xs text-muted-foreground">{{ formatBytes(image.size) }}</span>
                            <Tooltip v-if="isLargeFile(image)">
                              <TooltipTrigger as-child>
                                <Badge variant="outline" class="text-[10px] h-4 px-1.5 border-amber-500/50 text-amber-600 dark:text-amber-400 cursor-help">
                                  <AlertCircle class="h-2.5 w-2.5 mr-0.5" />
                                  {{ t('msd.largeFileWarning') }}
                                </Badge>
                              </TooltipTrigger>
                              <TooltipContent><p>{{ t('msd.largeFileTooltip') }}</p></TooltipContent>
                            </Tooltip>
                          </div>
                          <div v-if="image.digests?.length" class="mt-1 space-y-0.5">
                            <div v-for="digest in image.digests" :key="digest.algorithm" class="flex items-center gap-1 text-[10px] text-muted-foreground">
                              <span class="w-12 uppercase">{{ digest.algorithm }}</span>
                              <code class="min-w-0 flex-1 truncate" :title="digest.value">{{ digest.value }}</code>
                              <Button variant="ghost" size="icon" class="h-5 w-5 shrink-0" :title="t('msd.copyChecksum')" @click="copyDigest(digest.value)">
                                <Copy class="h-3 w-3" />
                              </Button>
                            </div>
                          </div>
                        </div>
                      </div>
                      <div class="flex items-center gap-1.5 shrink-0">
                        <Badge v-if="isCurrentImage(image.id)" variant="default" class="text-xs h-7 px-2">
                          <CircleDot class="h-3.5 w-3.5 mr-1" />
                          {{ t('common.connected') }}
                        </Badge>
                        <Button v-else-if="!msdConnected && supportsOperation(selectedStorage, 'select')" variant="default" size="sm" class="h-7 text-xs" :disabled="operationInProgress" @click="connectImage(image)">
                          <Link v-if="!isImageConnecting(image.id)" class="h-3.5 w-3.5 mr-1" />
                          <span v-if="isImageConnecting(image.id)">{{ t('common.connecting') }}...</span>
                          <span v-else>Mount</span>
                        </Button>
                        <Button v-if="supportsOperation(selectedStorage, 'delete') && image.deletable !== false" variant="ghost" size="icon" class="h-7 w-7 text-destructive hover:text-destructive" :disabled="operationInProgress || isCurrentImage(image.id)" @click="confirmDelete('image', image.id, image.name)">
                          <Trash2 class="h-3.5 w-3.5" />
                        </Button>
                      </div>
                    </div>
                  </div>
                </div>
              </div>

              <div v-if="systemStore.diskSpace && selectedCatalogLocation === 'aggregator'" class="shrink-0 pt-2 border-t mt-2">
                <p class="text-[11px] text-muted-foreground text-right">{{ t('msd.systemAvailable') }}: {{ formatBytes(systemStore.diskSpace.available) }}</p>
              </div>
            </div>
            </template>

            <div v-else-if="supportsOperation(remoteStorage, 'select')" class="flex-1 min-h-0 rounded-md border p-5 flex flex-col justify-center">
              <div class="mx-auto w-full max-w-lg space-y-4">
                <div>
                  <h4 class="text-sm font-medium">Mount remote media</h4>
                  <p class="text-xs text-muted-foreground mt-1">The BMC reads the image directly from an HTTPS endpoint.</p>
                </div>
              <MsdMediaTypeSelect
                v-model="remoteMediaType"
                :types="remoteMediaTypes"
                :disabled="operationInProgress || msdConnected"
              />
              <div class="space-y-1.5">
                <Label for="remote-media-url" class="text-xs">Image URL</Label>
                <div class="flex gap-2">
                <Input id="remote-media-url" v-model="remoteMediaUrl" type="url" placeholder="https://images.example/installer.iso" :disabled="operationInProgress" />
                <Button v-if="!msdConnected" size="sm" :disabled="operationInProgress || !remoteMediaUrl.trim()" @click="connectRemoteMedia">
                  <Link class="h-4 w-4 mr-1" />
                  Mount
                </Button>
                </div>
              </div>
            </div>
            </div>

          </TabsContent>

          <TabsContent value="drive" class="flex-1 min-h-0 m-0 flex flex-col space-y-4">
            <template v-if="!driveInitialized">
              <div class="shrink-0 text-center py-8 space-y-4">
                <HardDrive class="h-10 w-10 mx-auto text-muted-foreground" />
                <p class="text-sm text-muted-foreground">{{ t('msd.driveNotInitialized') }}</p>
                <Button size="sm" @click="initializeDrive">
                  {{ t('msd.initializeDrive') }}
                </Button>
              </div>
            </template>

            <template v-else>
              <!-- Drive Info Card -->
              <div class="shrink-0 p-3 rounded-lg border space-y-3" :class="msdConnected && msdMode === 'drive' ? 'border-primary bg-primary/5' : 'bg-muted/50'">
                <div class="flex items-center justify-between">
                  <div class="flex items-center gap-2">
                    <HardDrive class="h-4 w-4 text-muted-foreground" />
                    <span class="text-sm font-medium">{{ t('msd.drive') }}</span>
                    <Badge variant="outline" class="text-xs">
                      {{ Math.round((driveInfo?.capacity || 0) / 1024 / 1024) }} MB
                    </Badge>
                  </div>
                  <div class="flex items-center gap-1.5">
                    <template v-if="msdConnected && msdMode === 'drive'">
                      <Badge variant="default" class="text-xs h-7 px-2">
                        <span class="relative flex h-1.5 w-1.5 mr-1.5">
                          <span class="animate-ping absolute inline-flex h-full w-full rounded-full bg-white opacity-75"></span>
                          <span class="relative inline-flex rounded-full h-1.5 w-1.5 bg-white"></span>
                        </span>
                        {{ t('common.connected') }}
                      </Badge>
                    </template>
                    <template v-else-if="!msdConnected">
                      <Button
                        variant="default"
                        size="sm"
                        class="h-7 text-xs"
                        :disabled="operationInProgress"
                        @click="connectDrive"
                      >
                        <Link v-if="!isDriveConnecting()" class="h-3.5 w-3.5 mr-1" />
                        <span v-if="isDriveConnecting()">{{ t('common.connecting') }}...</span>
                        <span v-else>{{ t('msd.connect') }}</span>
                      </Button>
                    </template>
                    <Button
                      variant="ghost"
                      size="icon"
                      class="h-7 w-7 text-destructive hover:text-destructive"
                      :disabled="operationInProgress || msdConnected"
                      @click="showDeleteDriveDialog = true"
                    >
                      <Trash2 class="h-3.5 w-3.5" />
                    </Button>
                  </div>
                </div>
                <!-- Storage usage bar -->
                <div class="space-y-1.5">
                  <Progress
                    v-if="driveInfo"
                    :model-value="driveInfo.capacity > 0 ? (driveInfo.used / driveInfo.capacity) * 100 : 0"
                    class="h-2"
                  />
                  <div class="flex items-center justify-between text-xs text-muted-foreground">
                    <span>{{ formatBytes(driveInfo?.used || 0) }} {{ t('msd.usedSpace') }}</span>
                    <span>{{ formatBytes(driveInfo?.free || 0) }} {{ t('msd.freeSpace') }}</span>
                  </div>
                </div>
              </div>

              <!-- File Browser -->
              <div class="flex-1 min-h-0 flex flex-col space-y-2">
                <!-- Toolbar -->
                <div class="shrink-0 flex items-center justify-between gap-2">
                  <div class="flex items-center gap-1 min-w-0 flex-1">
                    <Button
                      v-if="currentPath !== '/'"
                      variant="ghost"
                      size="icon"
                      class="h-7 w-7 shrink-0"
                      @click="navigateUp"
                    >
                      <ArrowLeft class="h-3.5 w-3.5" />
                    </Button>
                    <nav class="flex items-center text-xs min-w-0 overflow-hidden">
                      <template v-for="(crumb, index) in breadcrumbs" :key="crumb.path">
                        <ChevronRight v-if="index > 0" class="h-3 w-3 text-muted-foreground mx-0.5 shrink-0" />
                        <button
                          class="hover:text-primary transition-colors truncate"
                          :class="index === breadcrumbs.length - 1 ? 'font-medium' : 'text-muted-foreground'"
                          @click="navigateTo(crumb.path)"
                        >
                          {{ crumb.name }}
                        </button>
                      </template>
                    </nav>
                  </div>
                  <div class="shrink-0 flex items-center gap-1 shrink-0">
                    <label>
                      <input type="file" class="hidden" :disabled="uploadingFile" @change="handleFileUpload" />
                      <Button variant="ghost" size="icon" as="span" class="h-7 w-7 cursor-pointer">
                        <Upload class="h-3.5 w-3.5" />
                      </Button>
                    </label>
                    <Button variant="ghost" size="icon" class="h-7 w-7" @click="showNewFolderDialog = true">
                      <FolderPlus class="h-3.5 w-3.5" />
                    </Button>
                    <Button variant="ghost" size="icon" class="h-7 w-7" @click="loadDriveFiles">
                      <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loadingDrive }" />
                    </Button>
                  </div>
                </div>

                <Progress v-if="uploadingFile" :model-value="fileUploadProgress" class="h-1 shrink-0" />

                <!-- File List -->
                <div v-if="driveFiles.length === 0" class="shrink-0 text-center py-6 text-muted-foreground text-sm">
                  {{ t('msd.emptyFolder') }}
                </div>

                <div v-else class="flex-1 min-h-0 overflow-y-auto pr-2 custom-scrollbar">
                  <div class="space-y-1">
                    <div
                      v-for="file in driveFiles"
                      :key="file.path"
                      class="flex items-center justify-between p-2 rounded-lg hover:bg-accent/50 transition-colors"
                    >
                      <div
                        class="flex items-center gap-2 cursor-pointer flex-1 min-w-0"
                        @click="file.is_dir && navigateTo(file.path)"
                      >
                        <Folder v-if="file.is_dir" class="h-4 w-4 text-blue-500 shrink-0" />
                        <File v-else class="h-4 w-4 text-muted-foreground shrink-0" />
                        <div class="min-w-0">
                          <Tooltip>
                            <TooltipTrigger as-child>
                              <p class="text-sm font-medium truncate cursor-help">{{ file.name }}</p>
                            </TooltipTrigger>
                            <TooltipContent>
                              <p class="max-w-sm break-all">{{ file.name }}</p>
                            </TooltipContent>
                          </Tooltip>
                          <p v-if="!file.is_dir" class="text-xs text-muted-foreground">
                            {{ formatBytes(file.size) }}
                          </p>
                        </div>
                      </div>
                      <div class="flex items-center gap-0.5 shrink-0">
                        <Button
                          v-if="!file.is_dir"
                          variant="ghost"
                          size="icon"
                          class="h-7 w-7"
                          as="a"
                          :href="msdApi.downloadDriveFile(file.path)"
                          download
                        >
                          <Download class="h-3.5 w-3.5" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon"
                          class="h-7 w-7 text-destructive"
                          @click="confirmDelete('file', file.path, file.name)"
                        >
                          <Trash2 class="h-3.5 w-3.5" />
                        </Button>
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            </template>
          </TabsContent>
        </Tabs>
      </div>
      </div>
    </DialogContent>
  </Dialog>
  </TooltipProvider>

  <!-- Delete Dialog -->
  <Dialog v-model:open="showDeleteDialog">
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{{ t('common.confirm') }}</DialogTitle>
        <DialogDescription>
          {{ t('msd.confirmDelete', { name: deleteTarget?.name }) }}
        </DialogDescription>
      </DialogHeader>
      <DialogFooter>
        <Button variant="outline" @click="showDeleteDialog = false" :disabled="deleting">
          {{ t('common.cancel') }}
        </Button>
        <Button variant="destructive" @click="executeDelete" :disabled="deleting">
          <span v-if="deleting">{{ t('common.deleting') }}...</span>
          <span v-else>{{ t('common.delete') }}</span>
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>

  <!-- Delete Drive Dialog -->
  <Dialog v-model:open="showDeleteDriveDialog">
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{{ t('msd.deleteDrive') }}</DialogTitle>
        <DialogDescription>
          {{ t('msd.confirmDeleteDrive') }}
        </DialogDescription>
      </DialogHeader>
      <DialogFooter>
        <Button variant="outline" @click="showDeleteDriveDialog = false" :disabled="deletingDrive">
          {{ t('common.cancel') }}
        </Button>
        <Button variant="destructive" @click="deleteDrive" :disabled="deletingDrive">
          <span v-if="deletingDrive">{{ t('common.deleting') }}...</span>
          <span v-else>{{ t('common.delete') }}</span>
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>

  <!-- Drive Init Dialog -->
  <Dialog v-model:open="showDriveInitDialog">
    <DialogContent class="max-w-md">
      <DialogHeader>
        <DialogTitle>{{ t('msd.initializeDrive') }}</DialogTitle>
        <DialogDescription>{{ t('msd.selectDriveSize') }}</DialogDescription>
      </DialogHeader>

      <div class="space-y-4">
        <!-- Preset size selection -->
        <div class="space-y-2">
          <Label>{{ t('msd.driveSize') }}</Label>
          <RadioGroup v-model="selectedDriveSize">
            <div v-for="size in driveSizeOptions" :key="size.value" class="flex items-center space-x-2">
              <RadioGroupItem :id="`size-${size.value}`" :value="size.value" />
              <Label :for="`size-${size.value}`" class="font-normal cursor-pointer flex-1">
                {{ size.label }}
              </Label>
            </div>
          </RadioGroup>
        </div>

        <!-- Custom size -->
        <div class="space-y-2">
          <Label for="custom-size">{{ t('msd.customSize') }}</Label>
          <div class="flex items-center gap-2">
            <Input
              id="custom-size"
              v-model.number="customDriveSize"
              type="number"
              :min="64"
              :max="32768"
              placeholder="256"
              class="flex-1"
            />
            <span class="text-sm text-muted-foreground">MB</span>
          </div>
          <p class="text-xs text-muted-foreground">
            {{ t('msd.driveSizeHint') }}
          </p>
        </div>

        <!-- Final size display -->
        <div class="p-3 rounded-lg bg-muted/50">
          <div class="flex items-center justify-between text-sm">
            <span class="text-muted-foreground">{{ t('msd.selectedSize') }}:</span>
            <span class="font-medium">{{ finalDriveSize }} MB</span>
          </div>
        </div>
      </div>

      <DialogFooter>
        <Button variant="outline" @click="showDriveInitDialog = false" :disabled="initializingDrive">
          {{ t('common.cancel') }}
        </Button>
        <Button @click="createDrive" :disabled="initializingDrive">
          <span v-if="initializingDrive">{{ t('common.creating') }}...</span>
          <span v-else>{{ t('common.create') }}</span>
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>

  <Dialog v-model:open="showTargetCreateDialog">
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{{ t('msd.createTarget') }}</DialogTitle>
        <DialogDescription>{{ t('msd.createTargetDesc') }}</DialogDescription>
      </DialogHeader>
      <div class="space-y-4">
        <MsdMediaTypeSelect v-model="targetMediaType" :types="msdCapabilities.target_management.media_types" :disabled="targetMutationInProgress" />
        <div class="space-y-2">
          <Label for="target-label">{{ t('msd.targetName') }} ({{ t('common.optional') }})</Label>
          <Input id="target-label" v-model="targetLabel" :disabled="targetMutationInProgress" @keyup.enter="createTarget" />
        </div>
      </div>
      <DialogFooter>
        <Button variant="outline" @click="showTargetCreateDialog = false">{{ t('common.cancel') }}</Button>
        <Button :disabled="!targetMediaType || targetTypeAtCapacity || targetMutationInProgress" @click="createTarget">{{ t('common.confirm') }}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>

  <Dialog v-model:open="showTargetDeleteDialog">
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{{ t('msd.deleteTarget') }}</DialogTitle>
        <DialogDescription>{{ t('msd.confirmDeleteTarget', { name: slotLabel(selectedSlotId) }) }}</DialogDescription>
      </DialogHeader>
      <DialogFooter>
        <Button variant="outline" @click="showTargetDeleteDialog = false">{{ t('common.cancel') }}</Button>
        <Button variant="destructive" :disabled="targetMutationInProgress" @click="removeSelectedTarget">{{ t('common.confirm') }}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>

  <!-- New Folder Dialog -->
  <Dialog v-model:open="showNewFolderDialog">
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{{ t('msd.createFolder') }}</DialogTitle>
      </DialogHeader>
      <Input v-model="newFolderName" :placeholder="t('msd.folderName')" @keyup.enter="createFolder" />
      <DialogFooter>
        <Button variant="outline" @click="showNewFolderDialog = false">{{ t('common.cancel') }}</Button>
        <Button @click="createFolder">{{ t('common.confirm') }}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>

  <!-- URL Download Dialog -->
  <Dialog v-model:open="showUrlDialog">
    <DialogContent>
      <DialogHeader>
        <DialogTitle class="flex items-center gap-2">
          <Globe class="h-5 w-5" />
          {{ t('msd.downloadFromUrl') }}
        </DialogTitle>
        <DialogDescription>{{ t('msd.downloadFromUrlDesc') }}</DialogDescription>
      </DialogHeader>

      <div class="space-y-4">
        <div class="space-y-2">
          <Label for="download-url">{{ t('msd.url') }}</Label>
          <Input
            id="download-url"
            v-model="downloadUrl"
            placeholder="https://example.com/image.iso"
            :disabled="downloading"
          />
        </div>
        <div class="space-y-2">
          <Label for="download-filename">{{ t('msd.filename') }} ({{ t('common.optional') }})</Label>
          <Input
            id="download-filename"
            v-model="downloadFilename"
            :placeholder="t('msd.filenameAutoDetect')"
            :disabled="downloading"
          />
        </div>
        <Button
          type="button"
          variant="ghost"
          class="px-0 text-muted-foreground"
          :disabled="downloading"
          @click="showDownloadAdvanced = !showDownloadAdvanced"
        >
          <ChevronRight class="mr-1 h-4 w-4 transition-transform" :class="showDownloadAdvanced && 'rotate-90'" />
          {{ t('msd.advancedOptions') }}
        </Button>
        <div v-if="showDownloadAdvanced" class="space-y-3 rounded-md border p-3">
          <div class="space-y-2">
            <Label for="download-user-agent">User-Agent</Label>
            <Input
              id="download-user-agent"
              v-model="downloadUserAgent"
              placeholder="Mozilla/5.0"
              :disabled="downloading"
            />
          </div>
          <div
            v-for="(requestHeader, index) in downloadHeaders"
            :key="index"
            class="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] gap-2"
          >
            <Input v-model="requestHeader.name" :placeholder="t('msd.headerName')" :disabled="downloading" />
            <Input v-model="requestHeader.value" :placeholder="t('msd.headerValue')" :disabled="downloading" />
            <Button
              type="button"
              variant="ghost"
              size="icon"
              :aria-label="t('msd.removeHeader')"
              :disabled="downloading"
              @click="downloadHeaders.splice(index, 1)"
            >
              <Trash2 class="h-4 w-4" />
            </Button>
          </div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            :disabled="downloading || downloadHeaders.length >= 32"
            @click="downloadHeaders.push({ name: '', value: '' })"
          >
            <Plus class="mr-1 h-4 w-4" />
            {{ t('msd.addHeader') }}
          </Button>
          <p class="text-xs text-muted-foreground">{{ t('msd.downloadHeadersHint') }}</p>
        </div>

        <!-- Download Progress -->
        <div v-if="downloadProgress" class="space-y-2 p-3 rounded-lg bg-muted/50">
          <div class="flex items-center justify-between text-sm">
            <span class="truncate">{{ downloadProgress.filename }}</span>
            <span class="text-muted-foreground shrink-0 ml-2">
              {{ downloadProgress.total_bytes ? Math.min(100, (downloadProgress.bytes_downloaded / downloadProgress.total_bytes) * 100).toFixed(0) + '%' : '…' }}
            </span>
          </div>
          <Progress :model-value="downloadProgress.total_bytes ? Math.min(100, (downloadProgress.bytes_downloaded / downloadProgress.total_bytes) * 100) : 0" class="h-1.5" />
          <div class="flex items-center justify-between text-xs text-muted-foreground">
            <span>{{ formatBytes(downloadProgress.bytes_downloaded) }}</span>
            <span v-if="downloadProgress.total_bytes">
              / {{ formatBytes(downloadProgress.total_bytes) }}
            </span>
          </div>
          <div v-if="downloadProgress.download_speed_bps > 0" class="text-xs text-muted-foreground text-right">
            {{ formatSpeed(downloadProgress.download_speed_bps) }}
          </div>
          <div v-if="downloadProgress.status === 'completed'" class="text-xs text-green-600">
            {{ t('msd.downloadComplete') }}
          </div>
          <div v-else-if="downloadProgress.status === 'failed'" class="text-xs text-destructive">
            {{ downloadProgress.status }}
          </div>
        </div>
      </div>

      <DialogFooter>
        <Button variant="outline" @click="showUrlDialog = false; resetDownloadState()">
          {{ t('common.cancel') }}
        </Button>
        <Button
          v-if="!downloading"
          :disabled="!downloadUrl.trim()"
          @click="startUrlDownload"
        >
          <Download class="h-4 w-4 mr-1" />
          {{ t('msd.download') }}
        </Button>
        <Button v-else variant="destructive" @click="cancelUrlDownload">
          <X class="h-4 w-4 mr-1" />
          {{ t('common.cancel') }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

<style scoped>
.custom-scrollbar::-webkit-scrollbar {
  width: 6px;
}

.custom-scrollbar::-webkit-scrollbar-track {
  background: transparent;
}

.custom-scrollbar::-webkit-scrollbar-thumb {
  background: hsl(var(--muted-foreground) / 0.3);
  border-radius: 10px;
}

.custom-scrollbar::-webkit-scrollbar-thumb:hover {
  background: hsl(var(--muted-foreground) / 0.5);
}

/* For Firefox */
.custom-scrollbar {
  scrollbar-width: thin;
  scrollbar-color: hsl(var(--muted-foreground) / 0.3) transparent;
}
</style>
