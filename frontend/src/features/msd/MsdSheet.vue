<script setup lang="ts">
import { ref, onMounted, computed, watch, toRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { useSystemStore } from '@/stores/system'
import {
  msdApi,
  type MsdImage,
  type DriveFile,
  type MsdMediaKind,
  type MsdStorageLocation,
  type MsdStorageOperation,
} from '@/api'
import MsdMediaTypeSelect from './MsdMediaTypeSelect.vue'
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetDescription,
} from '@/components/ui/sheet'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Progress } from '@/components/ui/progress'
import { Input } from '@/components/ui/input'
import { Separator } from '@/components/ui/separator'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
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
  AlertCircle,
  Copy,
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

function copyDigest(value: string) {
  void navigator.clipboard.writeText(value)
}

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
const msdMode = computed(() => currentAttachment.value?.mode ?? 'none')
const selectedMediaType = ref<MsdMediaKind>()
const catalogStorages = computed(() => msdCapabilities.value.storage_locations.filter(
  (storage) => storage.location !== 'remote'
    && (storage.operations.includes('list') || storage.operations.includes('upload')),
))
const selectedCatalogLocation = ref<MsdStorageLocation>()
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
const mediaTypes = computed<MsdMediaKind[]>(() => (selectedStorage.value?.media_types ?? []).filter(
  (mediaType) => selectedSlot.value?.media_types.includes(mediaType),
))

function supportsOperation(operation: MsdStorageOperation): boolean {
  return selectedStorage.value?.operations.includes(operation) ?? false
}

watch(catalogStorages, (storages) => {
  if (!storages.some((storage) => storage.location === selectedCatalogLocation.value)) {
    selectedCatalogLocation.value = storages.find(
      (storage) => storage.location === 'aggregator',
    )?.location ?? storages[0]?.location
  }
}, { immediate: true })
watch(() => msdCapabilities.value.slots, (slots) => {
  if (!slots.some((slot) => slot.id === selectedSlotId.value)) {
    selectedSlotId.value = slots[0]?.id ?? ''
  }
}, { immediate: true })

watch(mediaTypes, (types) => {
  if (!selectedMediaType.value || !types.includes(selectedMediaType.value)) {
    selectedMediaType.value = types[0]
  }
}, { immediate: true })

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

async function loadData() {
  await systemStore.fetchMsdState(true)
  if (!catalogStorages.value.some(
    (storage) => storage.location === selectedCatalogLocation.value,
  )) {
    selectedCatalogLocation.value = catalogStorages.value.find(
      (storage) => storage.location === 'aggregator',
    )?.location ?? catalogStorages.value[0]?.location
  }
  await loadImages()
  await loadDriveInfo()
  if (driveInitialized.value) {
    await loadDriveFiles()
  }
}

async function loadImages() {
  loadingImages.value = true
  try {
    const location = selectedStorage.value?.location
    if (!location || !supportsOperation('list')) {
      images.value = []
      return
    }
    if (location === 'aggregator' || location === 'bmc') {
      images.value = (await msdCatalogResources[location].refresh()).images
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
  const location = selectedStorage.value?.location
  if (location !== 'aggregator' && location !== 'bmc') return
  try {
    await msdStatusResource.mutate(mutationId => msdApi.connect(
        selectedSlotId.value,
        'image',
        { location, image_id: image.id },
        selectedMediaType.value,
        mutationId,
      ))
  } catch (e) {
    console.error('Failed to connect image:', e)
  }
}

async function disconnect() {
  try {
    await msdStatusResource.mutate(mutationId => msdApi.disconnect(selectedSlotId.value, mutationId))
  } catch (e) {
    console.error('Failed to disconnect:', e)
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
  if (!deleteTarget.value) return

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
  }
}

async function selectCatalog(location: MsdStorageLocation) {
  if (selectedCatalogLocation.value === location) return
  selectedCatalogLocation.value = location
  await loadImages()
}

function isCurrentImage(imageId: string): boolean {
  const media = currentAttachment.value?.media
  if (!msdConnected.value || !media || media.location === 'remote') return false
  return media.location === selectedCatalogLocation.value && media.image_id === imageId
}

async function loadDriveInfo() {
  try {
    driveInfo.value = await msdApi.driveInfo()
    driveInitialized.value = true
  } catch {
    driveInitialized.value = false
  }
}

async function initializeDrive() {
  try {
    await msdApi.initDrive(256)
    await loadDriveInfo()
    await loadDriveFiles()
  } catch (e) {
    console.error('Failed to initialize drive:', e)
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

async function connectDrive() {
  try {
    await msdStatusResource.mutate(mutationId => msdApi.connect(selectedSlotId.value, 'drive', undefined, undefined, mutationId))
  } catch (e) {
    console.error('Failed to connect drive:', e)
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return bytes + ' B'
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB'
  if (bytes < 1024 * 1024 * 1024) return (bytes / 1024 / 1024).toFixed(1) + ' MB'
  return (bytes / 1024 / 1024 / 1024).toFixed(1) + ' GB'
}

onMounted(async () => {
  if (props.open) {
    await loadData()
  }
})
</script>

<template>
  <Sheet :open="open" @update:open="emit('update:open', $event)">
    <SheetContent side="right" class="w-full sm:max-w-lg overflow-hidden flex flex-col h-[dvh]">
      <SheetHeader class="shrink-0">
        <div class="flex items-center justify-between pr-8">
          <div>
            <SheetTitle class="flex items-center gap-2">
              <HardDrive class="h-5 w-5" />
              {{ t('msd.title') }}
            </SheetTitle>
            <SheetDescription class="flex items-center gap-2 mt-1">
              {{ msdConnected ? t('common.connected') : t('common.disconnected') }}
              <Badge v-if="msdConnected" variant="secondary" class="text-xs">{{ msdMode }}</Badge>
            </SheetDescription>
          </div>
          <Button v-if="msdConnected" variant="destructive" size="sm" @click="disconnect">
            <Unlink class="h-4 w-4 mr-1" />
            {{ t('msd.disconnect') }}
          </Button>
        </div>
      </SheetHeader>

      <Separator class="my-4 shrink-0" />

      <Tabs v-model="activeTab" class="flex-1 flex flex-col min-h-0 overflow-hidden">
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

        <div class="flex-1 min-h-0 mt-4 flex flex-col">
          <!-- Images Tab -->
          <TabsContent value="images" class="flex-1 min-h-0 m-0 flex flex-col space-y-4">
            <div
              v-for="storage in msdCapabilities.unavailable_storage_locations ?? []"
              :key="storage.location"
              class="flex items-start gap-2 rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs"
            >
              <AlertCircle class="mt-0.5 h-4 w-4 shrink-0 text-amber-600" />
              <div class="min-w-0">
                <p class="font-medium">{{ t('msd.bmcStorageUnavailable') }}</p>
                <p class="mt-0.5 text-muted-foreground">{{ t(storage.hint_key) }}</p>
              </div>
            </div>
            <div v-if="catalogStorages.length > 1" class="grid shrink-0 gap-1" :style="{ gridTemplateColumns: `repeat(${catalogStorages.length}, minmax(0, 1fr))` }">
              <Button
                v-for="storage in catalogStorages"
                :key="storage.location"
                size="sm"
                :variant="selectedCatalogLocation === storage.location ? 'secondary' : 'ghost'"
                @click="selectCatalog(storage.location)"
              >
                {{ storage.location === 'aggregator' ? 'Aggregator storage' : 'BMC storage' }}
              </Button>
            </div>
            <MsdMediaTypeSelect
              v-model="selectedMediaType"
              :types="mediaTypes"
              :disabled="msdConnected"
            />
            <!-- Upload Area -->
            <div v-if="supportsOperation('upload')" class="shrink-0 space-y-3">
              <label class="block">
                <input
                  type="file"
                  accept=".iso,.img,.raw,.bin,.ima,.flp,.vfd,.dsk,.fat,.fat12,.fat16,.fat32,.efi,application/octet-stream"
                  class="hidden"
                  :disabled="uploading"
                  @change="handleImageUpload"
                />
                <div class="flex items-center justify-center h-16 border-2 border-dashed rounded-lg cursor-pointer hover:border-primary transition-colors">
                  <div class="text-center">
                    <Upload class="h-5 w-5 mx-auto text-muted-foreground" />
                    <p class="text-xs text-muted-foreground mt-1">{{ t('msd.uploadImage') }} (ISO/IMG)</p>
                  </div>
                </div>
              </label>
              <Progress v-if="uploading" :model-value="uploadProgress" class="h-1" />
            </div>

            <!-- Image List -->
            <div v-if="supportsOperation('list')" class="flex-1 min-h-0 flex flex-col space-y-2">
              <div class="shrink-0 flex items-center justify-between">
                <h4 class="text-sm font-medium">{{ t('msd.imageList') }}</h4>
                <Button variant="ghost" size="icon" class="h-7 w-7" @click="loadImages">
                  <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loadingImages }" />
                </Button>
              </div>

              <div v-if="images.length === 0" class="shrink-0 text-center py-6 text-muted-foreground text-sm">
                {{ t('msd.noImages') }}
              </div>

              <ScrollArea v-else class="flex-1 min-h-0 pr-4">
                <div class="space-y-1.5">
                  <div
                    v-for="image in images"
                    :key="image.id"
                    class="flex items-center justify-between p-2.5 rounded-lg border hover:bg-accent/50 transition-colors"
                  >
                    <div class="flex items-center gap-2.5 min-w-0 flex-1">
                      <Disc class="h-4 w-4 text-muted-foreground shrink-0" />
                      <div class="min-w-0">
                        <p class="text-sm font-medium truncate">{{ image.name }}</p>
                        <p class="text-xs text-muted-foreground">
                          {{ image.size === null ? '-' : formatBytes(image.size) }}
                        </p>
                        <div v-if="image.digests?.length" class="mt-1 space-y-0.5">
                          <div v-for="digest in image.digests" :key="digest.algorithm" class="flex items-center gap-1 text-[10px] text-muted-foreground">
                            <span class="w-12 uppercase">{{ digest.algorithm }}</span>
                            <code class="min-w-0 max-w-52 truncate" :title="digest.value">{{ digest.value }}</code>
                            <Button variant="ghost" size="icon" class="h-5 w-5 shrink-0" :title="t('msd.copyChecksum')" @click="copyDigest(digest.value)">
                              <Copy class="h-3 w-3" />
                            </Button>
                          </div>
                        </div>
                      </div>
                    </div>
                    <div class="flex items-center gap-1 shrink-0">
                      <Button
                        v-if="!isCurrentImage(image.id)"
                        variant="outline"
                        size="sm"
                        class="h-7 text-xs"
                        @click="connectImage(image)"
                      >
                        <Link class="h-3.5 w-3.5 mr-1" />
                        {{ t('msd.connect') }}
                      </Button>
                      <Badge v-else variant="default" class="text-xs">{{ t('common.connected') }}</Badge>
                      <Button
                        v-if="supportsOperation('delete') && image.deletable !== false"
                        variant="ghost"
                        size="icon"
                        class="h-7 w-7 text-destructive"
                        :disabled="isCurrentImage(image.id)"
                        @click="confirmDelete('image', image.id, image.name)"
                      >
                        <Trash2 class="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  </div>
                </div>
              </ScrollArea>
            </div>
          </TabsContent>

          <!-- Drive Tab -->
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
              <!-- Drive Info -->
              <div class="shrink-0 p-3 rounded-lg bg-muted/50 space-y-2">
                <div class="flex items-center justify-between">
                  <div class="space-y-0.5">
                    <p class="text-xs text-muted-foreground">{{ t('msd.driveSize') }}: {{ (driveInfo?.capacity || 0) / 1024 / 1024 }}MB</p>
                    <p class="text-xs text-muted-foreground">
                      {{ formatBytes(driveInfo?.used || 0) }} / {{ formatBytes(driveInfo?.capacity || 0) }}
                    </p>
                  </div>
                  <Button
                    v-if="!msdConnected || msdMode !== 'drive'"
                    variant="outline"
                    size="sm"
                    class="h-7 text-xs"
                    @click="connectDrive"
                  >
                    <Link class="h-3.5 w-3.5 mr-1" />
                    {{ t('msd.connect') }}
                  </Button>
                  <Badge v-else class="text-xs">{{ t('common.connected') }}</Badge>
                </div>
                <Progress
                  v-if="driveInfo"
                  :model-value="driveInfo.capacity > 0 ? (driveInfo.used / driveInfo.capacity) * 100 : 0"
                  class="h-1"
                />
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

                <ScrollArea v-else class="flex-1 min-h-0 pr-4">
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
                          <p class="text-sm font-medium truncate">{{ file.name }}</p>
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
                </ScrollArea>
              </div>
            </template>
          </TabsContent>
        </div>
      </Tabs>
    </SheetContent>
  </Sheet>

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
        <Button variant="outline" @click="showDeleteDialog = false">{{ t('common.cancel') }}</Button>
        <Button variant="destructive" @click="executeDelete">{{ t('common.delete') }}</Button>
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
