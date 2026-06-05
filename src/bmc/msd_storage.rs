use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

/// Runtime MSD state for a device.
#[derive(Clone, Debug)]
pub struct MsdRuntimeState {
    pub backend: String,
    pub mode: String,
    pub connected: bool,
    pub current_image: Option<String>,
}

impl Default for MsdRuntimeState {
    fn default() -> Self {
        Self {
            backend: "mahogany".to_string(),
            mode: "drive".to_string(),
            connected: false,
            current_image: None,
        }
    }
}

/// MsdStorageProvider — the seam for MSD filesystem operations.
/// One adapter exists (FsMsdStorage); tests provide an in-memory adapter.
#[async_trait::async_trait]
pub trait MsdStorageProvider: Send + Sync {
    async fn ensure_dirs(&self, device_id: &str) -> std::io::Result<()>;
    async fn list_images(&self, device_id: &str) -> Vec<serde_json::Value>;
    async fn upload_image(&self, device_id: &str, filename: &str, bytes: &[u8]) -> Result<u64, String>;
    async fn delete_image(&self, device_id: &str, image_id: &str) -> std::io::Result<()>;
    async fn write_downloaded_image(&self, device_id: &str, filename: &str, bytes: &[u8]) -> Result<String, String>;
    async fn drive_info(&self, device_id: &str) -> (u64, u64, u64, u64);
    async fn drive_files_list(&self, device_id: &str, rel_path: &str) -> Vec<serde_json::Value>;
    async fn drive_upload_file(&self, device_id: &str, rel_path: &str, filename: &str, bytes: &[u8]) -> Result<u64, String>;
    async fn drive_delete_file(&self, device_id: &str, path: &str) -> std::io::Result<()>;
    async fn drive_mkdir(&self, device_id: &str, path: &str) -> std::io::Result<()>;
    async fn drive_delete_all(&self, device_id: &str) -> std::io::Result<()>;
    async fn drive_init_size(&self, device_id: &str, size_mb: u64) -> u64;
    async fn storage_base(&self, device_id: &str) -> String;
}

/// Filesystem-backed MSD storage.
#[derive(Clone)]
pub struct FsMsdStorage {
    pub storage_root: String,
    pub config: Arc<RwLock<crate::config::Config>>,
}

impl FsMsdStorage {
    pub fn new(storage_root: String, config: Arc<RwLock<crate::config::Config>>) -> Self {
        Self { storage_root, config }
    }

    fn root_dir(&self, device_id: &str) -> PathBuf {
        PathBuf::from(&self.storage_root).join(device_id)
    }

    pub(crate) fn images_dir(&self, device_id: &str) -> PathBuf {
        self.root_dir(device_id).join("images")
    }

    fn drive_dir(&self, device_id: &str) -> PathBuf {
        self.root_dir(device_id).join("drive")
    }

    fn safe_join_under(base: &Path, rel: &str) -> Option<PathBuf> {
        let trimmed = rel.trim_start_matches('/');
        let candidate = base.join(trimmed);
        if candidate.components().any(|component| matches!(component, std::path::Component::ParentDir)) {
            return None;
        }
        Some(candidate)
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
                let p = entry.path();
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_dir() {
                        stack.push(p);
                    } else if metadata.is_file() {
                        total = total.saturating_add(metadata.len());
                    }
                }
            }
        }
        total
    }
}

#[async_trait::async_trait]
impl MsdStorageProvider for FsMsdStorage {
    async fn ensure_dirs(&self, device_id: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(self.images_dir(device_id))?;
        std::fs::create_dir_all(self.drive_dir(device_id))?;
        Ok(())
    }

    async fn list_images(&self, device_id: &str) -> Vec<serde_json::Value> {
        let images_dir = self.images_dir(device_id);
        let mut images = Vec::new();
        if let Ok(entries) = std::fs::read_dir(images_dir) {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_file() {
                        images.push(serde_json::json!({
                            "id": entry.file_name().to_string_lossy().to_string(),
                            "name": entry.file_name().to_string_lossy().to_string(),
                            "size": metadata.len()
                        }));
                    }
                }
            }
        }
        images
    }

    async fn upload_image(&self, device_id: &str, filename: &str, bytes: &[u8]) -> Result<u64, String> {
        let images_dir = self.images_dir(device_id);
        let target = Self::safe_join_under(&images_dir, filename)
            .ok_or_else(|| "invalid filename".to_string())?;
        std::fs::write(&target, bytes)
            .map_err(|e| e.to_string())?;
        Ok(bytes.len() as u64)
    }

    async fn delete_image(&self, device_id: &str, image_id: &str) -> std::io::Result<()> {
        let images_dir = self.images_dir(device_id);
        let path = Self::safe_join_under(&images_dir, image_id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid image id"))?;
        std::fs::remove_file(path)
    }

    async fn write_downloaded_image(&self, device_id: &str, filename: &str, bytes: &[u8]) -> Result<String, String> {
        let cleaned = Self::safe_filename(filename);
        let images_dir = self.images_dir(device_id);
        let target = Self::safe_join_under(&images_dir, &cleaned)
            .ok_or_else(|| "invalid filename".to_string())?;
        std::fs::write(&target, bytes)
            .map_err(|e| e.to_string())?;
        Ok(cleaned)
    }

    async fn drive_info(&self, device_id: &str) -> (u64, u64, u64, u64) {
        let drive = self.drive_dir(device_id);
        let used = Self::folder_size_bytes(&drive);
        let capacity_mb = self.config.read().await
            .msd.get(device_id).map(|c| c.drive_size_mb).unwrap_or(1024);
        let size = capacity_mb * 1024 * 1024;
        let free = size.saturating_sub(used);
        (size, used, free, capacity_mb)
    }

    async fn drive_files_list(&self, device_id: &str, rel_path: &str) -> Vec<serde_json::Value> {
        let drive_root = self.drive_dir(device_id);
        let target = match Self::safe_join_under(&drive_root, rel_path) {
            Some(path) => path,
            None => return Vec::new(),
        };
        let mut items = Vec::new();
        if let Ok(entries) = std::fs::read_dir(target) {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let item_path = if rel_path == "/" { format!("/{}", name) } else { format!("{}/{}", rel_path.trim_end_matches('/'), name) };
                    items.push(serde_json::json!({
                        "path": item_path,
                        "name": name,
                        "is_dir": metadata.is_dir(),
                        "size": if metadata.is_file() { metadata.len() } else { 0 }
                    }));
                }
            }
        }
        items
    }

    async fn drive_upload_file(&self, device_id: &str, rel_path: &str, filename: &str, bytes: &[u8]) -> Result<u64, String> {
        let base = self.drive_dir(device_id);
        let dir = Self::safe_join_under(&base, rel_path)
            .ok_or_else(|| "invalid path".to_string())?;
        std::fs::create_dir_all(&dir)
            .map_err(|e| e.to_string())?;

        let capacity_mb = self.config.read().await
            .msd.get(device_id).map(|c| c.drive_size_mb).unwrap_or(1024);
        let used = Self::folder_size_bytes(&base);
        let new_size = bytes.len() as u64;
        if used.saturating_add(new_size) > capacity_mb * 1024 * 1024 {
            return Err("drive capacity exceeded".to_string());
        }

        let cleaned = Self::safe_filename(filename);
        let file_path = dir.join(&cleaned);
        std::fs::write(&file_path, bytes)
            .map_err(|e| e.to_string())?;
        Ok(new_size)
    }

    async fn drive_delete_file(&self, device_id: &str, path: &str) -> std::io::Result<()> {
        let base = self.drive_dir(device_id);
        if let Some(p) = Self::safe_join_under(&base, path) {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }

    async fn drive_mkdir(&self, device_id: &str, path: &str) -> std::io::Result<()> {
        let base = self.drive_dir(device_id);
        if let Some(p) = Self::safe_join_under(&base, path) {
            std::fs::create_dir_all(p)?;
        }
        Ok(())
    }

    async fn drive_delete_all(&self, device_id: &str) -> std::io::Result<()> {
        let drive = self.drive_dir(device_id);
        let _ = std::fs::remove_dir_all(&drive);
        std::fs::create_dir_all(&drive)
    }

    async fn drive_init_size(&self, _device_id: &str, size_mb: u64) -> u64 {
        size_mb
    }

    async fn storage_base(&self, device_id: &str) -> String {
        let cfg = self.config.read().await;
        cfg.msd.get(device_id).and_then(|c| c.storage_dir.clone()).unwrap_or_else(|| self.storage_root.clone())
    }
}

/// In-memory MSD storage adapter for testing.
pub struct InMemoryStorage {
    images: std::sync::RwLock<HashMap<String, HashMap<String, Vec<u8>>>>,
    drives: std::sync::RwLock<HashMap<String, HashMap<String, Vec<u8>>>>,
    capacity_mb: u64,
}

impl InMemoryStorage {
    pub fn new(capacity_mb: u64) -> Self {
        Self {
            images: std::sync::RwLock::new(HashMap::new()),
            drives: std::sync::RwLock::new(HashMap::new()),
            capacity_mb,
        }
    }
}

#[async_trait::async_trait]
impl MsdStorageProvider for InMemoryStorage {
    async fn ensure_dirs(&self, device_id: &str) -> std::io::Result<()> {
        self.images.write().unwrap().entry(device_id.to_string()).or_default();
        self.drives.write().unwrap().entry(device_id.to_string()).or_default();
        Ok(())
    }

    async fn list_images(&self, device_id: &str) -> Vec<serde_json::Value> {
        let images = self.images.read().unwrap();
        let dev_images = match images.get(device_id) {
            Some(m) => m,
            None => return Vec::new(),
        };
        dev_images.iter().map(|(name, data)| serde_json::json!({
            "id": name,
            "name": name,
            "size": data.len(),
        })).collect()
    }

    async fn upload_image(&self, device_id: &str, filename: &str, bytes: &[u8]) -> Result<u64, String> {
        let mut images = self.images.write().unwrap();
        let dev = images.entry(device_id.to_string()).or_default();
        dev.insert(filename.to_string(), bytes.to_vec());
        Ok(bytes.len() as u64)
    }

    async fn delete_image(&self, device_id: &str, image_id: &str) -> std::io::Result<()> {
        let mut images = self.images.write().unwrap();
        if let Some(dev) = images.get_mut(device_id) {
            dev.remove(image_id);
        }
        Ok(())
    }

    async fn write_downloaded_image(&self, device_id: &str, filename: &str, bytes: &[u8]) -> Result<String, String> {
        let cleaned = FsMsdStorage::safe_filename(filename);
        let mut images = self.images.write().unwrap();
        let dev = images.entry(device_id.to_string()).or_default();
        dev.insert(cleaned.clone(), bytes.to_vec());
        Ok(cleaned)
    }

    async fn drive_info(&self, device_id: &str) -> (u64, u64, u64, u64) {
        let drives = self.drives.read().unwrap();
        let used: u64 = drives.get(device_id)
            .map(|d| d.values().map(|v| v.len() as u64).sum())
            .unwrap_or(0);
        let capacity = self.capacity_mb * 1024 * 1024;
        let free = capacity.saturating_sub(used);
        (capacity, used, free, self.capacity_mb)
    }

    async fn drive_files_list(&self, device_id: &str, rel_path: &str) -> Vec<serde_json::Value> {
        let drives = self.drives.read().unwrap();
        let dev = match drives.get(device_id) {
            Some(d) => d,
            None => return Vec::new(),
        };
        let prefix = if rel_path == "/" { String::new() } else { format!("{}/", rel_path.trim_end_matches('/')) };
        let mut items = Vec::new();
        for (name, data) in dev.iter() {
            if !name.starts_with(&prefix) { continue; }
            let rel_name = name.strip_prefix(&prefix).unwrap_or(name);
            if rel_name.contains('/') { continue; }
            let item_path = if rel_path == "/" { format!("/{}", rel_name) } else { format!("{}/{}", rel_path.trim_end_matches('/'), rel_name) };
            items.push(serde_json::json!({
                "path": item_path,
                "name": rel_name,
                "is_dir": false,
                "size": data.len(),
            }));
        }
        items
    }

    async fn drive_upload_file(&self, device_id: &str, rel_path: &str, filename: &str, bytes: &[u8]) -> Result<u64, String> {
        let full_path = if rel_path == "/" || rel_path.is_empty() {
            filename.to_string()
        } else {
            format!("{}/{}", rel_path.trim_end_matches('/'), filename)
        };
        let mut drives = self.drives.write().unwrap();
        let dev = drives.entry(device_id.to_string()).or_default();

        let used: u64 = dev.values().map(|v| v.len() as u64).sum();
        let new_size = bytes.len() as u64;
        if used.saturating_add(new_size) > self.capacity_mb * 1024 * 1024 {
            return Err("drive capacity exceeded".to_string());
        }
        dev.insert(full_path, bytes.to_vec());
        Ok(new_size)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider() -> InMemoryStorage {
        InMemoryStorage::new(1024)
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
        let name = s.write_downloaded_image("dev1", "my image.iso", b"content").await.unwrap();
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
        s.drive_upload_file("dev1", "/", "a.bin", &[0; 100]).await.unwrap();
        let (capacity, used, free, _mb) = s.drive_info("dev1").await;
        assert_eq!(used, 100);
        assert_eq!(free, capacity - 100);
    }

    #[tokio::test]
    async fn test_drive_capacity_exceeded() {
        let s = InMemoryStorage::new(1); // 1 MB
        s.ensure_dirs("dev1").await.unwrap();
        s.drive_upload_file("dev1", "/", "big.bin", &[0; 1024 * 1024]).await.unwrap();
        let result = s.drive_upload_file("dev1", "/", "extra.bin", &[0; 1]).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("capacity exceeded"));
    }

    #[tokio::test]
    async fn test_drive_files_list() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        s.drive_upload_file("dev1", "/", "a.txt", b"aaa").await.unwrap();
        s.drive_upload_file("dev1", "/", "b.txt", b"bbb").await.unwrap();
        let items = s.drive_files_list("dev1", "/").await;
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn test_drive_delete_file() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        s.drive_upload_file("dev1", "/", "del.bin", b"x").await.unwrap();
        assert_eq!(s.drive_files_list("dev1", "/").await.len(), 1);
        s.drive_delete_file("dev1", "del.bin").await.unwrap();
        assert!(s.drive_files_list("dev1", "/").await.is_empty());
    }

    #[tokio::test]
    async fn test_drive_delete_all() {
        let s = test_provider();
        s.ensure_dirs("dev1").await.unwrap();
        s.drive_upload_file("dev1", "/", "f1.bin", b"1").await.unwrap();
        s.drive_upload_file("dev1", "/", "f2.bin", b"2").await.unwrap();
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
