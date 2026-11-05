#[cfg(test)]
use std::collections::HashMap;
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

const BYTES_PER_MIB: u64 = 1024 * 1024;
const MAX_DRIVE_FILE_DOWNLOAD_BYTES: u64 = 128 * BYTES_PER_MIB;

/// MsdStorageProvider — the seam for MSD filesystem operations.
/// One adapter exists (FsMsdStorage); tests provide an in-memory adapter.
#[async_trait::async_trait]
pub trait MsdStorageProvider: Send + Sync {
    async fn ensure_dirs(&self, device_id: &str) -> std::io::Result<()>;
    async fn list_images(&self, device_id: &str) -> Vec<serde_json::Value>;
    async fn upload_image(
        &self,
        device_id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<u64, String>;
    async fn upload_image_file(
        &self,
        device_id: &str,
        filename: &str,
        source: &Path,
        size: u64,
    ) -> Result<u64, String>;
    async fn delete_image(&self, device_id: &str, image_id: &str) -> std::io::Result<()>;
    async fn write_downloaded_image(
        &self,
        device_id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<String, String>;
    async fn drive_info(&self, device_id: &str) -> (u64, u64, u64, u64);
    async fn drive_files_list(&self, device_id: &str, rel_path: &str) -> Vec<serde_json::Value>;
    async fn drive_upload_file(
        &self,
        device_id: &str,
        rel_path: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<u64, String>;
    async fn drive_upload_file_from_path(
        &self,
        device_id: &str,
        rel_path: &str,
        filename: &str,
        source: &Path,
        size: u64,
    ) -> Result<u64, String>;
    async fn drive_read_file(
        &self,
        device_id: &str,
        path: &str,
    ) -> Result<Option<(String, Vec<u8>)>, String>;
    async fn drive_delete_file(&self, device_id: &str, path: &str) -> std::io::Result<()>;
    async fn drive_mkdir(&self, device_id: &str, path: &str) -> std::io::Result<()>;
    async fn drive_delete_all(&self, device_id: &str) -> std::io::Result<()>;
    async fn drive_init_size(&self, device_id: &str, size_mb: u64) -> u64;
    async fn storage_base(&self, device_id: &str) -> String;
}

#[derive(Debug, thiserror::Error)]
pub enum ImageDigestError {
    #[error("published image changed before checksum commit")]
    Changed,
    #[error("{0}")]
    Storage(String),
}

type ImageLocks =
    std::collections::BTreeMap<(PathBuf, String), std::sync::Weak<tokio::sync::Mutex<()>>>;

/// Filesystem-backed MSD storage.
#[derive(Clone)]
pub struct FsMsdStorage {
    pub storage_root: String,
    pub config: Arc<RwLock<crate::config::Config>>,
    drive_write: Arc<tokio::sync::Mutex<()>>,
    image_locks: Arc<std::sync::Mutex<ImageLocks>>,
}

impl FsMsdStorage {
    async fn image_guard(&self, device_id: &str, name: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let key = (self.images_dir(device_id).await, name.to_owned());
        let lock = {
            let mut locks = self
                .image_locks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(&key).and_then(std::sync::Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(tokio::sync::Mutex::new(()));
                locks.insert(key, Arc::downgrade(&lock));
                lock
            }
        };
        lock.lock_owned().await
    }

    async fn upload_image_file_locked(
        &self,
        device_id: &str,
        filename: &str,
        source: &Path,
        size: u64,
    ) -> Result<u64, String> {
        let images_dir = self.images_dir(device_id).await;
        let target = Self::image_target(&images_dir, filename)?;
        let metadata = tokio::fs::metadata(source)
            .await
            .map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() != size {
            return Err("staged upload size changed".to_string());
        }
        let temporary = images_dir.join(format!(".{filename}.{}.tmp", uuid::Uuid::new_v4()));
        let result = async {
            if tokio::fs::hard_link(source, &temporary).await.is_ok() {
                tokio::fs::rename(&temporary, &target).await?;
                return Ok(size);
            }
            let mut input = tokio::fs::File::open(source).await?;
            let mut output = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .await?;
            let copied = tokio::io::copy(&mut input, &mut output).await?;
            if copied != size {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "staged upload size changed",
                ));
            }
            output.sync_all().await?;
            tokio::fs::rename(&temporary, &target).await?;
            Ok(copied)
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        result.map_err(|error: std::io::Error| error.to_string())
    }

    /// Publish only verified content; readers reject metadata from a different inode/version.
    pub async fn publish_image_file(
        &self,
        device_id: &str,
        filename: &str,
        source: &Path,
        size: u64,
        digests: &[crate::bmc::feature::msd::MsdMediaDigest],
    ) -> Result<u64, ImageDigestError> {
        let _guard = self.image_guard(device_id, filename).await;
        self.publish_image_file_locked(device_id, filename, source, size, digests)
            .await
    }

    /// Resume under the same image lock: an existing different version is never replaced.
    pub async fn recover_image_file(
        &self,
        device_id: &str,
        filename: &str,
        source: &Path,
        size: u64,
        digests: &[crate::bmc::feature::msd::MsdMediaDigest],
    ) -> Result<u64, ImageDigestError> {
        let _guard = self.image_guard(device_id, filename).await;
        let target = Self::image_target(&self.images_dir(device_id).await, filename)
            .map_err(ImageDigestError::Storage)?;
        match tokio::fs::metadata(target).await {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.len() != size {
                    return Err(ImageDigestError::Changed);
                }
                self.store_image_digests_locked(device_id, filename, digests)
                    .await?;
                Ok(size)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.publish_image_file_locked(device_id, filename, source, size, digests)
                    .await
            }
            Err(error) => Err(ImageDigestError::Storage(error.to_string())),
        }
    }

    async fn publish_image_file_locked(
        &self,
        device_id: &str,
        filename: &str,
        source: &Path,
        size: u64,
        digests: &[crate::bmc::feature::msd::MsdMediaDigest],
    ) -> Result<u64, ImageDigestError> {
        let source_before = image_version(
            &tokio::fs::metadata(source)
                .await
                .map_err(|e| ImageDigestError::Storage(e.to_string()))?,
        );
        if crate::bmc::media_digest::calculate_digests(source)
            .await
            .map_err(|e| ImageDigestError::Storage(e.to_string()))?
            != digests
            || image_version(
                &tokio::fs::metadata(source)
                    .await
                    .map_err(|e| ImageDigestError::Storage(e.to_string()))?,
            ) != source_before
        {
            return Err(ImageDigestError::Changed);
        }
        let size = self
            .upload_image_file_locked(device_id, filename, source, size)
            .await
            .map_err(ImageDigestError::Storage)?;
        let target = Self::image_target(&self.images_dir(device_id).await, filename)
            .map_err(ImageDigestError::Storage)?;
        let published_version = image_version(
            &tokio::fs::metadata(target)
                .await
                .map_err(|error| ImageDigestError::Storage(error.to_string()))?,
        );
        if source_before == published_version {
            self.write_image_metadata(device_id, filename, published_version, digests)
                .await?;
        } else {
            self.store_image_digests_locked(device_id, filename, digests)
                .await?;
        }
        Ok(size)
    }

    /// Public image identifiers are flat names; dot names belong to storage internals.
    pub fn is_image_name(name: &str) -> bool {
        !name.is_empty()
            && !name.starts_with('.')
            && name.len() <= 255
            && Self::safe_filename(name) == name
    }

    fn image_target(base: &Path, name: &str) -> Result<PathBuf, String> {
        if !Self::is_image_name(name) {
            return Err("invalid published image name".to_owned());
        }
        Self::safe_join_under(base, name).ok_or_else(|| "invalid published image path".to_owned())
    }

    pub fn new(storage_root: String, config: Arc<RwLock<crate::config::Config>>) -> Self {
        Self {
            storage_root,
            config,
            drive_write: Arc::new(tokio::sync::Mutex::new(())),
            image_locks: Default::default(),
        }
    }

    fn default_root_dir(&self, device_id: &str) -> PathBuf {
        // Device ids originate in configuration but are also present in HTTP
        // paths.  Never allow an unexpected id to become a path component.
        let component = if !device_id.is_empty()
            && device_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            device_id.to_owned()
        } else {
            // '.' cannot appear in an accepted id, so this namespace cannot alias it.
            format!(
                ".id-{}",
                device_id
                    .as_bytes()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            )
        };
        PathBuf::from(&self.storage_root).join(component)
    }

    async fn root_dir(&self, device_id: &str) -> PathBuf {
        self.config
            .read()
            .await
            .msd
            .get(device_id)
            .and_then(|config| config.storage_dir.as_ref())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.default_root_dir(device_id))
    }

    pub(crate) async fn images_dir(&self, device_id: &str) -> PathBuf {
        self.root_dir(device_id).await.join("images")
    }

    pub async fn create_staged_upload(
        &self,
        device_id: &str,
    ) -> std::io::Result<tempfile::NamedTempFile> {
        let staging_dir = self.images_dir(device_id).await.join(".staging");
        tokio::fs::create_dir_all(&staging_dir).await?;
        tempfile::Builder::new()
            .prefix("upload-")
            .suffix(".part")
            .tempfile_in(staging_dir)
    }

    pub(crate) async fn staging_dir(&self, device_id: &str) -> PathBuf {
        self.images_dir(device_id).await.join(".staging")
    }

    pub async fn store_image_digests(
        &self,
        device_id: &str,
        filename: &str,
        digests: &[crate::bmc::feature::msd::MsdMediaDigest],
    ) -> Result<(), ImageDigestError> {
        let _guard = self.image_guard(device_id, filename).await;
        self.store_image_digests_locked(device_id, filename, digests)
            .await
    }

    async fn store_image_digests_locked(
        &self,
        device_id: &str,
        filename: &str,
        digests: &[crate::bmc::feature::msd::MsdMediaDigest],
    ) -> Result<(), ImageDigestError> {
        let images_dir = self.images_dir(device_id).await;
        let path = Self::image_target(&images_dir, filename).map_err(ImageDigestError::Storage)?;
        let before = image_version(
            &std::fs::metadata(&path).map_err(|e| ImageDigestError::Storage(e.to_string()))?,
        );
        let existing_metadata = images_dir
            .join(".metadata")
            .join(format!("{filename}.json"));
        if let Ok(bytes) = tokio::fs::read(existing_metadata).await {
            if let Ok(record) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if record["version"] == before && record["digests"] == serde_json::json!(digests) {
                    return Ok(());
                }
            }
        }
        let actual = crate::bmc::media_digest::calculate_digests(&path)
            .await
            .map_err(|e| ImageDigestError::Storage(e.to_string()))?;
        let after = image_version(
            &std::fs::metadata(&path).map_err(|e| ImageDigestError::Storage(e.to_string()))?,
        );
        if before != after || actual != digests {
            return Err(ImageDigestError::Changed);
        }
        self.write_image_metadata(device_id, filename, after, digests)
            .await
    }

    async fn write_image_metadata(
        &self,
        device_id: &str,
        filename: &str,
        version: serde_json::Value,
        digests: &[crate::bmc::feature::msd::MsdMediaDigest],
    ) -> Result<(), ImageDigestError> {
        let metadata_dir = self.images_dir(device_id).await.join(".metadata");
        tokio::fs::create_dir_all(&metadata_dir)
            .await
            .map_err(|error| ImageDigestError::Storage(error.to_string()))?;
        let metadata_name = format!("{}.json", Self::safe_filename(filename));
        let target = metadata_dir.join(metadata_name);
        let encoded =
            serde_json::to_vec(&serde_json::json!({"version": version, "digests": digests}))
                .map_err(|error| ImageDigestError::Storage(error.to_string()))?;
        Self::atomic_write(&target, &encoded)
            .map_err(|error| ImageDigestError::Storage(error.to_string()))
    }

    /// Resolve an image ID to a regular file below this device's image root.
    /// Backend adapters receive the resulting filesystem path only after this
    /// storage boundary has rejected traversal and symlink escape attempts.
    pub async fn image_file(
        &self,
        device_id: &str,
        image_id: &str,
    ) -> Result<Option<(PathBuf, u64, Arc<std::fs::File>)>, String> {
        let images_dir = self.images_dir(device_id).await;
        if !Self::is_image_name(image_id) {
            return Err("invalid published image id".to_owned());
        }
        #[cfg(target_os = "linux")]
        let file: std::fs::File = {
            use rustix::fs::{Mode, OFlags, ResolveFlags};
            let directory = rustix::fs::open(
                &images_dir,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| error.to_string())?;
            match rustix::fs::openat2(
                &directory,
                image_id,
                OFlags::RDONLY | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
            ) {
                Ok(file) => file.into(),
                Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
                Err(error) => return Err(error.to_string()),
            }
        };
        #[cfg(not(target_os = "linux"))]
        let file = std::fs::OpenOptions::new()
            .read(true)
            .open(
                Self::safe_join_under(&images_dir, image_id)
                    .ok_or_else(|| "invalid image id".to_string())?,
            )
            .map_err(|error| error.to_string())?;
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        if !metadata.is_file() {
            return Ok(None);
        }
        let file = Arc::new(file);
        #[cfg(target_os = "linux")]
        let path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        #[cfg(not(target_os = "linux"))]
        let path = Self::safe_join_under(&images_dir, image_id)
            .ok_or_else(|| "invalid image id".to_string())?;
        Ok(Some((path, metadata.len(), file)))
    }

    async fn drive_dir(&self, device_id: &str) -> PathBuf {
        self.root_dir(device_id).await.join("drive")
    }

    fn safe_join_under(base: &Path, rel: &str) -> Option<PathBuf> {
        let trimmed = rel.trim_start_matches('/');
        let candidate = base.join(trimmed);
        if candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return None;
        }

        let root = std::fs::canonicalize(base).ok()?;
        // If the target already exists, canonicalizing it catches a final
        // symlink.  For a new target, canonicalize the nearest existing
        // ancestor so a symlink in its parent chain cannot escape `root`.
        let existing = if candidate.exists() {
            candidate.clone()
        } else {
            let mut ancestor = candidate.parent()?;
            while !ancestor.exists() {
                ancestor = ancestor.parent()?;
            }
            ancestor.to_path_buf()
        };
        let resolved = std::fs::canonicalize(existing).ok()?;
        resolved.starts_with(root).then_some(candidate)
    }

    pub fn safe_filename(input: &str) -> String {
        let mut name = input.rsplit('/').next().unwrap_or(input).to_string();
        name = name.rsplit('\\').next().unwrap_or(&name).to_string();
        if name.is_empty() {
            return format!("file-{}", uuid::Uuid::new_v4());
        }
        let cleaned: String = name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        if cleaned.is_empty() {
            format!("file-{}", uuid::Uuid::new_v4())
        } else {
            cleaned
        }
    }

    fn folder_size_bytes(path: &Path) -> u64 {
        let mut total = 0u64;
        let mut stack = vec![path.to_path_buf()];
        while let Some(current) = stack.pop() {
            let entries = match std::fs::read_dir(&current) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(_) => continue,
                };
                // Do not follow local symlinks while computing usage: a
                // symlink added outside the HTTP API must not escape storage.
                if file_type.is_symlink() {
                    continue;
                }
                if file_type.is_dir() {
                    stack.push(entry.path());
                } else if file_type.is_file() {
                    if let Ok(metadata) = entry.metadata() {
                        total = total.saturating_add(metadata.len());
                    }
                }
            }
        }
        total
    }

    fn capacity_bytes(capacity_mb: u64) -> u64 {
        capacity_mb.saturating_mul(BYTES_PER_MIB)
    }

    fn atomic_write(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let parent = target.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "storage target has no parent",
            )
        })?;
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file");
        let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, target)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

#[async_trait::async_trait]
impl MsdStorageProvider for FsMsdStorage {
    async fn ensure_dirs(&self, device_id: &str) -> std::io::Result<()> {
        let root = self.root_dir(device_id).await;
        tokio::fs::create_dir_all(root.join("images")).await?;
        tokio::fs::create_dir_all(root.join("drive")).await?;
        #[cfg(unix)]
        tokio::fs::set_permissions(&root, {
            use std::os::unix::fs::PermissionsExt;
            std::fs::Permissions::from_mode(0o700)
        })
        .await?;
        Ok(())
    }

    async fn list_images(&self, device_id: &str) -> Vec<serde_json::Value> {
        let images_dir = self.images_dir(device_id).await;
        tokio::task::spawn_blocking(move || {
          let mut images = Vec::new();
          if let Ok(entries) = std::fs::read_dir(images_dir) {
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_file() && Self::is_image_name(&entry.file_name().to_string_lossy()) {
                    if let Ok(metadata) = entry.metadata() {
                        images.push(serde_json::json!({
                            "id": entry.file_name().to_string_lossy().to_string(),
                            "name": entry.file_name().to_string_lossy().to_string(),
                            "size": metadata.len(),
                            "digests": std::fs::read(
                                entry.path().parent().unwrap().join(".metadata").join(format!(
                                    "{}.json",
                                    entry.file_name().to_string_lossy()
                                ))
                            )
                            .ok()
                            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                            .filter(|record| record["version"] == image_version(&metadata))
                            .and_then(|record| record.get("digests").cloned())
                            .unwrap_or_else(|| serde_json::json!([]))
                        }));
                    }
                }
            }
          }
          images
        }).await.unwrap_or_default()
    }

    async fn upload_image(
        &self,
        device_id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<u64, String> {
        let _image_guard = self.image_guard(device_id, filename).await;
        let images_dir = self.images_dir(device_id).await;
        let bytes = bytes.to_vec();
        let filename = filename.to_string();
        if !Self::is_image_name(&filename) {
            return Err("invalid filename".to_string());
        }
        tokio::task::spawn_blocking(move || {
            let _image_guard = _image_guard;
            let target = Self::safe_join_under(&images_dir, &filename)
                .ok_or_else(|| "invalid filename".to_string())?;
            Self::atomic_write(&target, &bytes).map_err(|e| e.to_string())?;
            Ok(bytes.len() as u64)
        })
        .await
        .map_err(|error| error.to_string())?
    }

    async fn upload_image_file(
        &self,
        device_id: &str,
        filename: &str,
        source: &Path,
        size: u64,
    ) -> Result<u64, String> {
        let _image_guard = self.image_guard(device_id, filename).await;
        self.upload_image_file_locked(device_id, filename, source, size)
            .await
    }

    async fn delete_image(&self, device_id: &str, image_id: &str) -> std::io::Result<()> {
        if !Self::is_image_name(image_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid published image id",
            ));
        }
        let _image_guard = self.image_guard(device_id, image_id).await;
        let images_dir = self.images_dir(device_id).await;
        let image_id = image_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let _image_guard = _image_guard;
            let path = Self::safe_join_under(&images_dir, &image_id).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid image id")
            })?;
            std::fs::remove_file(path)?;
            let metadata = images_dir
                .join(".metadata")
                .join(format!("{}.json", Self::safe_filename(&image_id)));
            match std::fs::remove_file(metadata) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        })
        .await
        .map_err(std::io::Error::other)?
    }

    async fn write_downloaded_image(
        &self,
        device_id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<String, String> {
        let _image_guard = self.image_guard(device_id, filename).await;
        let cleaned = Self::safe_filename(filename);
        if !Self::is_image_name(&cleaned) {
            return Err("invalid published image name".to_owned());
        }
        let images_dir = self.images_dir(device_id).await;
        let payload = bytes.to_vec();
        let output_name = cleaned.clone();
        tokio::task::spawn_blocking(move || {
            let _image_guard = _image_guard;
            let target = Self::safe_join_under(&images_dir, &output_name)
                .ok_or_else(|| "invalid filename".to_string())?;
            Self::atomic_write(&target, &payload).map_err(|e| e.to_string())
        })
        .await
        .map_err(|error| error.to_string())??;
        Ok(cleaned)
    }

    async fn drive_info(&self, device_id: &str) -> (u64, u64, u64, u64) {
        let drive = self.drive_dir(device_id).await;
        let used = tokio::task::spawn_blocking(move || Self::folder_size_bytes(&drive))
            .await
            .unwrap_or(0);
        let capacity_mb = self
            .config
            .read()
            .await
            .msd
            .get(device_id)
            .map(|c| c.drive_size_mb)
            .unwrap_or(1024);
        let size = Self::capacity_bytes(capacity_mb);
        let free = size.saturating_sub(used);
        (size, used, free, capacity_mb)
    }

    async fn drive_files_list(&self, device_id: &str, rel_path: &str) -> Vec<serde_json::Value> {
        let drive_root = self.drive_dir(device_id).await;
        let rel_path = rel_path.to_owned();
        tokio::task::spawn_blocking(move || {
            let target = match Self::safe_join_under(&drive_root, &rel_path) {
                Some(path) => path,
                None => return Vec::new(),
            };
            let mut items = Vec::new();
            if let Ok(entries) = std::fs::read_dir(target) {
                for entry in entries.flatten() {
                    let Ok(file_type) = entry.file_type() else {
                        continue;
                    };
                    if file_type.is_symlink() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().to_string();
                    let item_path = if rel_path == "/" {
                        format!("/{}", name)
                    } else {
                        format!("{}/{}", rel_path.trim_end_matches('/'), name)
                    };
                    let size = if file_type.is_file() {
                        entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
                    } else {
                        0
                    };
                    items.push(serde_json::json!({
                        "path": item_path,
                        "name": name,
                        "is_dir": file_type.is_dir(),
                        "size": size
                    }));
                }
            }
            items
        })
        .await
        .unwrap_or_default()
    }

    async fn drive_upload_file(
        &self,
        device_id: &str,
        rel_path: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<u64, String> {
        let staged = self
            .create_staged_upload(device_id)
            .await
            .map_err(|e| e.to_string())?;
        tokio::fs::write(staged.path(), bytes)
            .await
            .map_err(|e| e.to_string())?;
        self.drive_upload_file_from_path(
            device_id,
            rel_path,
            filename,
            staged.path(),
            bytes.len() as u64,
        )
        .await
    }

    async fn drive_upload_file_from_path(
        &self,
        device_id: &str,
        rel_path: &str,
        filename: &str,
        source: &Path,
        size: u64,
    ) -> Result<u64, String> {
        let guard = self.drive_write.clone().lock_owned().await;
        let base = self.drive_dir(device_id).await;
        let rel_path = rel_path.to_owned();
        let capacity_mb = self
            .config
            .read()
            .await
            .msd
            .get(device_id)
            .map(|c| c.drive_size_mb)
            .unwrap_or(1024);
        let cleaned = Self::safe_filename(filename);
        let source = source.to_owned();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let dir =
                Self::safe_join_under(&base, &rel_path).ok_or_else(|| "invalid path".to_owned())?;
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let target = Self::safe_join_under(&dir, &cleaned)
                .ok_or_else(|| "invalid filename".to_owned())?;
            let used = Self::folder_size_bytes(&base);
            let replaced = std::fs::metadata(&target)
                .ok()
                .filter(|m| m.is_file())
                .map_or(0, |m| m.len());
            if used.saturating_sub(replaced).saturating_add(size)
                > Self::capacity_bytes(capacity_mb)
            {
                return Err("drive capacity exceeded".into());
            }
            let mut input = std::fs::File::open(source).map_err(|e| e.to_string())?;
            if input.metadata().map_err(|e| e.to_string())?.len() != size {
                return Err("staged upload size changed".into());
            }
            let mut output = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
            let copied = std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
            if copied != size {
                return Err("staged upload size changed".into());
            }
            output.as_file().sync_all().map_err(|e| e.to_string())?;
            output.persist(&target).map_err(|e| e.to_string())?;
            Ok(copied)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn drive_read_file(
        &self,
        device_id: &str,
        path: &str,
    ) -> Result<Option<(String, Vec<u8>)>, String> {
        let base = self.drive_dir(device_id).await;
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let file_path =
                Self::safe_join_under(&base, &path).ok_or_else(|| "invalid path".to_string())?;
            let metadata = match std::fs::metadata(&file_path) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err.to_string()),
            };
            if !metadata.is_file() {
                return Ok(None);
            }
            if metadata.len() > MAX_DRIVE_FILE_DOWNLOAD_BYTES {
                return Err(format!(
                    "file exceeds {} MiB download limit",
                    MAX_DRIVE_FILE_DOWNLOAD_BYTES / 1024 / 1024
                ));
            }
            let name = file_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(Self::safe_filename)
                .unwrap_or_else(|| "download.bin".to_string());
            let data = std::fs::read(file_path).map_err(|e| e.to_string())?;
            Ok(Some((name, data)))
        })
        .await
        .map_err(|error| error.to_string())?
    }

    async fn drive_delete_file(&self, device_id: &str, path: &str) -> std::io::Result<()> {
        let base = self.drive_dir(device_id).await;
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let path = Self::safe_join_under(&base, &path).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid drive path")
            })?;
            std::fs::remove_file(path)
        })
        .await
        .map_err(std::io::Error::other)?
    }

    async fn drive_mkdir(&self, device_id: &str, path: &str) -> std::io::Result<()> {
        let base = self.drive_dir(device_id).await;
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&base)?;
            let path = Self::safe_join_under(&base, &path).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid drive path")
            })?;
            std::fs::create_dir_all(path)
        })
        .await
        .map_err(std::io::Error::other)?
    }

    async fn drive_delete_all(&self, device_id: &str) -> std::io::Result<()> {
        let drive = self.drive_dir(device_id).await;
        tokio::task::spawn_blocking(move || {
            let _ = std::fs::remove_dir_all(&drive);
            std::fs::create_dir_all(&drive)
        })
        .await
        .map_err(std::io::Error::other)?
    }

    async fn drive_init_size(&self, _device_id: &str, size_mb: u64) -> u64 {
        size_mb
    }

    async fn storage_base(&self, device_id: &str) -> String {
        self.root_dir(device_id).await.to_string_lossy().to_string()
    }
}

/// In-memory MSD storage adapter for testing.
#[cfg(test)]
pub struct InMemoryStorage {
    images: std::sync::RwLock<HashMap<String, HashMap<String, Vec<u8>>>>,
    drives: std::sync::RwLock<HashMap<String, HashMap<String, Vec<u8>>>>,
    capacity_mb: u64,
}

#[cfg(test)]
impl InMemoryStorage {
    pub fn new(capacity_mb: u64) -> Self {
        Self {
            images: std::sync::RwLock::new(HashMap::new()),
            drives: std::sync::RwLock::new(HashMap::new()),
            capacity_mb,
        }
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl MsdStorageProvider for InMemoryStorage {
    async fn ensure_dirs(&self, device_id: &str) -> std::io::Result<()> {
        self.images
            .write()
            .unwrap()
            .entry(device_id.to_string())
            .or_default();
        self.drives
            .write()
            .unwrap()
            .entry(device_id.to_string())
            .or_default();
        Ok(())
    }

    async fn list_images(&self, device_id: &str) -> Vec<serde_json::Value> {
        let images = self.images.read().unwrap();
        let dev_images = match images.get(device_id) {
            Some(m) => m,
            None => return Vec::new(),
        };
        dev_images
            .iter()
            .map(|(name, data)| {
                serde_json::json!({
                    "id": name,
                    "name": name,
                    "size": data.len(),
                })
            })
            .collect()
    }

    async fn upload_image(
        &self,
        device_id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<u64, String> {
        let mut images = self.images.write().unwrap();
        let dev = images.entry(device_id.to_string()).or_default();
        dev.insert(filename.to_string(), bytes.to_vec());
        Ok(bytes.len() as u64)
    }

    async fn upload_image_file(
        &self,
        device_id: &str,
        filename: &str,
        source: &Path,
        size: u64,
    ) -> Result<u64, String> {
        let bytes = tokio::fs::read(source)
            .await
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 != size {
            return Err("staged upload size changed".to_string());
        }
        self.upload_image(device_id, filename, &bytes).await
    }

    async fn delete_image(&self, device_id: &str, image_id: &str) -> std::io::Result<()> {
        let mut images = self.images.write().unwrap();
        if let Some(dev) = images.get_mut(device_id) {
            dev.remove(image_id);
        }
        Ok(())
    }

    async fn write_downloaded_image(
        &self,
        device_id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<String, String> {
        let cleaned = FsMsdStorage::safe_filename(filename);
        let mut images = self.images.write().unwrap();
        let dev = images.entry(device_id.to_string()).or_default();
        dev.insert(cleaned.clone(), bytes.to_vec());
        Ok(cleaned)
    }

    async fn drive_info(&self, device_id: &str) -> (u64, u64, u64, u64) {
        let drives = self.drives.read().unwrap();
        let used: u64 = drives
            .get(device_id)
            .map(|d| d.values().map(|v| v.len() as u64).sum())
            .unwrap_or(0);
        let capacity = FsMsdStorage::capacity_bytes(self.capacity_mb);
        let free = capacity.saturating_sub(used);
        (capacity, used, free, self.capacity_mb)
    }

    async fn drive_files_list(&self, device_id: &str, rel_path: &str) -> Vec<serde_json::Value> {
        let drives = self.drives.read().unwrap();
        let dev = match drives.get(device_id) {
            Some(d) => d,
            None => return Vec::new(),
        };
        let prefix = if rel_path == "/" {
            String::new()
        } else {
            format!("{}/", rel_path.trim_end_matches('/'))
        };
        let mut items = Vec::new();
        for (name, data) in dev.iter() {
            if !name.starts_with(&prefix) {
                continue;
            }
            let rel_name = name.strip_prefix(&prefix).unwrap_or(name);
            if rel_name.contains('/') {
                continue;
            }
            let item_path = if rel_path == "/" {
                format!("/{}", rel_name)
            } else {
                format!("{}/{}", rel_path.trim_end_matches('/'), rel_name)
            };
            items.push(serde_json::json!({
                "path": item_path,
                "name": rel_name,
                "is_dir": false,
                "size": data.len(),
            }));
        }
        items
    }

    async fn drive_upload_file(
        &self,
        device_id: &str,
        rel_path: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<u64, String> {
        let full_path = if rel_path == "/" || rel_path.is_empty() {
            filename.to_string()
        } else {
            format!("{}/{}", rel_path.trim_end_matches('/'), filename)
        };
        let mut drives = self.drives.write().unwrap();
        let dev = drives.entry(device_id.to_string()).or_default();

        let used: u64 = dev.values().map(|v| v.len() as u64).sum();
        let new_size = bytes.len() as u64;
        let replaced_size = dev
            .get(&full_path)
            .map(|existing| existing.len() as u64)
            .unwrap_or(0);
        if used.saturating_sub(replaced_size).saturating_add(new_size)
            > FsMsdStorage::capacity_bytes(self.capacity_mb)
        {
            return Err("drive capacity exceeded".to_string());
        }
        dev.insert(full_path, bytes.to_vec());
        Ok(new_size)
    }

    async fn drive_upload_file_from_path(
        &self,
        device_id: &str,
        rel_path: &str,
        filename: &str,
        source: &Path,
        size: u64,
    ) -> Result<u64, String> {
        let bytes = tokio::fs::read(source).await.map_err(|e| e.to_string())?;
        if bytes.len() as u64 != size {
            return Err("staged upload size changed".into());
        }
        self.drive_upload_file(device_id, rel_path, filename, &bytes)
            .await
    }

    async fn drive_read_file(
        &self,
        device_id: &str,
        path: &str,
    ) -> Result<Option<(String, Vec<u8>)>, String> {
        let drives = self.drives.read().unwrap();
        let Some(dev) = drives.get(device_id) else {
            return Ok(None);
        };
        let normalized = path.trim_start_matches('/');
        let Some(data) = dev.get(normalized).or_else(|| dev.get(path)) else {
            return Ok(None);
        };
        let name = normalized
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .map(FsMsdStorage::safe_filename)
            .unwrap_or_else(|| "download.bin".to_string());
        Ok(Some((name, data.clone())))
    }

    async fn drive_delete_file(&self, device_id: &str, path: &str) -> std::io::Result<()> {
        let mut drives = self.drives.write().unwrap();
        if let Some(dev) = drives.get_mut(device_id) {
            dev.remove(path);
        }
        Ok(())
    }

    async fn drive_mkdir(&self, _device_id: &str, _path: &str) -> std::io::Result<()> {
        Ok(())
    }

    async fn drive_delete_all(&self, device_id: &str) -> std::io::Result<()> {
        let mut drives = self.drives.write().unwrap();
        drives.remove(device_id);
        Ok(())
    }

    async fn drive_init_size(&self, _device_id: &str, size_mb: u64) -> u64 {
        size_mb
    }

    async fn storage_base(&self, device_id: &str) -> String {
        format!("/in-memory/msd/{}", device_id)
    }
}

fn image_version(metadata: &std::fs::Metadata) -> serde_json::Value {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        serde_json::json!([
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
        ])
    }
    #[cfg(not(unix))]
    {
        serde_json::json!([
            metadata.len(),
            metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos().to_string())
        ])
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::config::{Config, MsdDeviceConfig, ServerConfig, WebTransportConfig};

    #[tokio::test]
    async fn internal_objects_never_enter_the_public_image_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let storage = filesystem_provider(dir.path(), 8);
        storage.ensure_dirs("dev1").await.unwrap();
        let images = storage.images_dir("dev1").await;
        std::fs::write(images.join(".copy.in-progress.tmp"), b"partial").unwrap();
        std::fs::create_dir_all(images.join(".staging")).unwrap();
        std::fs::write(images.join(".staging/upload.part"), b"partial").unwrap();
        assert!(storage.list_images("dev1").await.is_empty());
        for name in [
            ".metadata",
            ".staging/upload.part",
            ".copy.in-progress.tmp",
            "../outside",
            "sub/image.iso",
        ] {
            assert!(storage.upload_image("dev1", name, b"bad").await.is_err());
            assert!(storage.image_file("dev1", name).await.is_err());
            assert!(storage.delete_image("dev1", name).await.is_err());
        }
        storage
            .upload_image("dev1", "valid.iso", b"old")
            .await
            .unwrap();
        let staged = dir.path().join("source.part");
        std::fs::write(&staged, b"new").unwrap();
        assert!(storage
            .publish_image_file("dev1", "valid.iso", &staged, 3, &[])
            .await
            .is_err());
        assert_eq!(std::fs::read(images.join("valid.iso")).unwrap(), b"old");
    }

    #[tokio::test]
    async fn unrelated_image_publications_do_not_share_the_operation_lock() {
        let dir = tempfile::tempdir().unwrap();
        let storage = filesystem_provider(dir.path(), 8);
        storage.ensure_dirs("dev1").await.unwrap();
        let _first = storage.image_guard("dev1", "first.iso").await;
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            storage.upload_image("dev1", "second.iso", b"data"),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            storage.upload_image("dev1", "first.iso", b"data")
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn stale_checksum_cannot_replace_new_image_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let storage = filesystem_provider(dir.path(), 8);
        storage.ensure_dirs("dev1").await.unwrap();
        storage
            .upload_image("dev1", "shared.iso", b"aaaa")
            .await
            .unwrap();
        let path = dir.path().join("dev1/images/shared.iso");
        let old = crate::bmc::media_digest::calculate_digests(&path)
            .await
            .unwrap();
        storage
            .store_image_digests("dev1", "shared.iso", &old)
            .await
            .unwrap();
        storage
            .upload_image("dev1", "shared.iso", b"bbbb")
            .await
            .unwrap();
        assert_eq!(
            storage.list_images("dev1").await[0]["digests"],
            serde_json::json!([])
        );
        let new = crate::bmc::media_digest::calculate_digests(&path)
            .await
            .unwrap();
        storage
            .store_image_digests("dev1", "shared.iso", &new)
            .await
            .unwrap();
        assert!(storage
            .store_image_digests("dev1", "shared.iso", &old)
            .await
            .is_err());
        assert_eq!(
            storage.list_images("dev1").await[0]["digests"],
            serde_json::json!(new)
        );
        let restarted = filesystem_provider(dir.path(), 8);
        assert_eq!(
            restarted.list_images("dev1").await[0]["digests"],
            serde_json::json!(new)
        );
    }

    fn test_provider() -> InMemoryStorage {
        InMemoryStorage::new(1024)
    }

    fn filesystem_provider(root: &Path, capacity_mb: u64) -> FsMsdStorage {
        let config = Config {
            server: ServerConfig {
                listen: "127.0.0.1:0".to_string(),
                frontend_dir: root.to_path_buf(),
                data_dir: root.to_path_buf(),
                allow_insecure_http: false,
                download_headers: HashMap::new(),
                download_header_origins: Vec::new(),
                download_connect_timeout_seconds: 15,
                download_idle_timeout_seconds: 60,
                allow_private_downloads: false,
                webtransport: WebTransportConfig::default(),
            },
            devices: Vec::new(),
            msd: HashMap::from([(
                "dev1".to_string(),
                MsdDeviceConfig {
                    drive_size_mb: capacity_mb,
                    ..Default::default()
                },
            )]),
        };
        FsMsdStorage::new(
            root.to_string_lossy().to_string(),
            Arc::new(RwLock::new(config)),
        )
    }

    #[test]
    fn storage_ids_cannot_alias_after_sanitization() {
        let directory = tempfile::tempdir().unwrap();
        let storage = filesystem_provider(directory.path(), 1);
        let ids = ["a.b", "a_b", "a/b", "a?b", "", "..", "safe", "你好"];
        let paths: std::collections::HashSet<_> =
            ids.iter().map(|id| storage.default_root_dir(id)).collect();
        assert_eq!(paths.len(), ids.len());
        assert_eq!(
            storage.default_root_dir("safe"),
            directory.path().join("safe")
        );
    }

    #[tokio::test]
    async fn filesystem_streaming_commit_preserves_sizes_above_four_gib_without_buffering() {
        let directory = tempfile::tempdir().unwrap();
        let storage = filesystem_provider(directory.path(), 1);
        storage.ensure_dirs("dev1").await.unwrap();
        let staging = storage.staging_dir("dev1").await;
        tokio::fs::create_dir_all(&staging).await.unwrap();
        let source = staging.join("large.part");
        let size = 4 * 1024 * 1024 * 1024u64 + 4096;
        tokio::fs::File::create(&source)
            .await
            .unwrap()
            .set_len(size)
            .await
            .unwrap();

        assert_eq!(
            storage
                .upload_image_file("dev1", "large.iso", &source, size)
                .await
                .unwrap(),
            size
        );
        let (_, stored_size, _file) = storage
            .image_file("dev1", "large.iso")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored_size, size);
    }

    #[tokio::test]
    async fn filesystem_storage_replacing_a_file_does_not_double_count_capacity() {
        let directory = tempfile::tempdir().unwrap();
        let storage = filesystem_provider(directory.path(), 1);
        storage.ensure_dirs("dev1").await.unwrap();
        let megabyte = vec![7u8; BYTES_PER_MIB as usize];

        storage
            .drive_upload_file("dev1", "/", "disk.img", &megabyte)
            .await
            .unwrap();
        storage
            .drive_upload_file("dev1", "/", "disk.img", &megabyte)
            .await
            .unwrap();
        assert!(storage
            .drive_upload_file("dev1", "/", "one-byte-more", &[1])
            .await
            .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filesystem_storage_rejects_traversal_and_symlink_escape() {
        let directory = tempfile::tempdir().unwrap();
        let storage = filesystem_provider(directory.path(), 8);
        storage.ensure_dirs("dev1").await.unwrap();

        let outside = directory.path().join("outside.iso");
        std::fs::write(&outside, b"outside").unwrap();
        let images = storage.images_dir("dev1").await;
        std::os::unix::fs::symlink(&outside, images.join("outside-link.iso")).unwrap();

        assert!(storage
            .upload_image("dev1", "../outside.iso", b"blocked")
            .await
            .is_err());
        assert!(storage
            .image_file("dev1", "outside-link.iso")
            .await
            .is_err());
        assert!(storage
            .list_images("dev1")
            .await
            .iter()
            .all(|image| image["id"] != "outside-link.iso"));

        let drive = storage.drive_dir("dev1").await;
        std::os::unix::fs::symlink(&outside, drive.join("outside-link.bin")).unwrap();
        assert!(storage.drive_files_list("dev1", "/").await.is_empty());
        assert!(storage
            .drive_read_file("dev1", "outside-link.bin")
            .await
            .is_err());
        assert_eq!(storage.drive_info("dev1").await.1, 0);
    }

    #[tokio::test]
    async fn test_ensure_dirs_and_list_images_empty() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        let images = s.list_images("dev1").await;
        assert!(images.is_empty());
    }

    #[tokio::test]
    async fn test_upload_and_list_images() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        s.upload_image("dev1", "test.iso", b"hello").await.unwrap();
        let images = s.list_images("dev1").await;
        assert_eq!(images.len(), 1);
        assert_eq!(images[0]["name"], "test.iso");
        assert_eq!(images[0]["size"], 5);
    }

    #[tokio::test]
    async fn test_delete_image() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        s.upload_image("dev1", "del.iso", b"data").await.unwrap();
        assert_eq!(s.list_images("dev1").await.len(), 1);
        s.delete_image("dev1", "del.iso").await.unwrap();
        assert!(s.list_images("dev1").await.is_empty());
    }

    #[tokio::test]
    async fn test_write_downloaded_image_clean_filename() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        let name = s
            .write_downloaded_image("dev1", "my image.iso", b"content")
            .await
            .unwrap();
        assert_eq!(name, "my_image.iso");
        assert_eq!(s.list_images("dev1").await.len(), 1);
    }

    #[tokio::test]
    async fn test_drive_info_empty() {
        let s = test_provider();
        let (capacity, used, free, _mb) = s.drive_info("dev1").await;
        assert_eq!(used, 0);
        assert_eq!(free, capacity);
    }

    #[tokio::test]
    async fn test_drive_upload_file_tracks_usage() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        s.drive_upload_file("dev1", "/", "a.bin", &[0; 100])
            .await
            .unwrap();
        let (capacity, used, free, _mb) = s.drive_info("dev1").await;
        assert_eq!(used, 100);
        assert_eq!(free, capacity - 100);
    }

    #[tokio::test]
    async fn test_drive_capacity_exceeded() {
        let s = InMemoryStorage::new(1); // 1 MB
        s.ensure_dirs("dev1").await.unwrap();
        s.drive_upload_file("dev1", "/", "big.bin", &[0; 1024 * 1024])
            .await
            .unwrap();
        let result = s.drive_upload_file("dev1", "/", "extra.bin", &[0; 1]).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("capacity exceeded"));
    }

    #[tokio::test]
    async fn test_drive_files_list() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        s.drive_upload_file("dev1", "/", "a.txt", b"aaa")
            .await
            .unwrap();
        s.drive_upload_file("dev1", "/", "b.txt", b"bbb")
            .await
            .unwrap();
        let items = s.drive_files_list("dev1", "/").await;
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn test_drive_delete_file() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        s.drive_upload_file("dev1", "/", "del.bin", b"x")
            .await
            .unwrap();
        assert_eq!(s.drive_files_list("dev1", "/").await.len(), 1);
        s.drive_delete_file("dev1", "del.bin").await.unwrap();
        assert!(s.drive_files_list("dev1", "/").await.is_empty());
    }

    #[tokio::test]
    async fn test_drive_delete_all() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        s.drive_upload_file("dev1", "/", "f1.bin", b"1")
            .await
            .unwrap();
        s.drive_upload_file("dev1", "/", "f2.bin", b"2")
            .await
            .unwrap();
        s.drive_delete_all("dev1").await.unwrap();
        assert!(s.drive_files_list("dev1", "/").await.is_empty());
    }

    #[tokio::test]
    async fn test_images_isolated_per_device() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        s.ensure_dirs("dev2").await.unwrap();
        s.upload_image("dev1", "img1.iso", b"x").await.unwrap();
        s.upload_image("dev2", "img2.iso", b"y").await.unwrap();
        assert_eq!(s.list_images("dev1").await.len(), 1);
        assert_eq!(s.list_images("dev2").await.len(), 1);
    }

    #[tokio::test]
    async fn test_drive_init_size_passthrough() {
        let s = test_provider();
        assert_eq!(s.drive_init_size("dev1", 512).await, 512);
        assert_eq!(s.drive_init_size("dev1", 2048).await, 2048);
    }

    #[tokio::test]
    async fn test_storage_base_format() {
        let s = test_provider();
        assert_eq!(s.storage_base("dev1").await, "/in-memory/msd/dev1");
        assert_eq!(s.storage_base("other").await, "/in-memory/msd/other");
    }
}
