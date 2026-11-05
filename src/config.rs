use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub devices: Vec<DeviceConfig>,
    #[serde(default)]
    pub msd: HashMap<String, MsdDeviceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsdDeviceConfig {
    #[serde(default = "default_msd_backend")]
    pub backend: String,
    #[serde(default = "default_msd_drive_size_mb")]
    pub drive_size_mb: u64,
    #[serde(default)]
    pub storage_dir: Option<String>,
}

impl Default for MsdDeviceConfig {
    fn default() -> Self {
        Self {
            backend: default_msd_backend(),
            drive_size_mb: default_msd_drive_size_mb(),
            storage_dir: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub listen: String,
    pub frontend_dir: PathBuf,
    /// Persistent application data. Relative paths are resolved by the
    /// process working directory, matching `frontend_dir` and the config path.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// Plain HTTP must stay on loopback unless an operator explicitly accepts
    /// the risk (normally because a trusted TLS reverse proxy owns the host).
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default)]
    pub download_headers: HashMap<String, String>,
    /// Exact URL origins allowed to receive server-configured download
    /// headers, for example `https://images.example.com`.
    #[serde(default)]
    pub download_header_origins: Vec<String>,
    #[serde(default = "default_download_connect_timeout_seconds")]
    pub download_connect_timeout_seconds: u64,
    #[serde(default = "default_download_idle_timeout_seconds")]
    pub download_idle_timeout_seconds: u64,
    /// Remote image download is SSRF-sensitive.  Keep RFC1918/loopback
    /// targets disabled unless an operator explicitly needs an internal image
    /// repository.
    #[serde(default)]
    pub allow_private_downloads: bool,
    #[serde(default)]
    pub webtransport: WebTransportConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebTransportConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_webtransport_listen")]
    pub listen: String,
    #[serde(default)]
    pub public_url: Option<String>,
    #[serde(default)]
    pub cert_path: Option<PathBuf>,
    #[serde(default)]
    pub key_path: Option<PathBuf>,
}

impl Default for WebTransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: default_webtransport_listen(),
            public_url: None,
            cert_path: None,
            key_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    /// Port for KVM/Mahogany protocol (defaults to `port` if not set)
    /// For HP iLO 4, this is typically 17990
    #[serde(default)]
    pub kvm_port: Option<u16>,
    /// IPMI RMCP+ host (defaults to `host` if not set)
    #[serde(default)]
    pub ipmi_host: Option<String>,
    /// IPMI RMCP+ port (defaults to 623 if not set)
    #[serde(default)]
    pub ipmi_port: Option<u16>,
    pub auth: AuthConfig,
    #[serde(default = "default_max_fps", skip_serializing)]
    pub max_fps: u32,
    #[serde(default = "default_video_backend", skip_serializing)]
    pub video_backend: String,
    #[serde(default = "default_encoder_backend", skip_serializing)]
    pub encoder_backend: String,
    #[serde(default)]
    pub audio_backend: Option<String>,
    #[serde(default)]
    pub features: HashMap<String, DeviceFeatureConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_fps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_quality: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceFeatureConfig {
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub backend_params: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub feature_params: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<DeviceFeatureProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceFeatureProviderConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub backend: String,
    #[serde(default)]
    pub backend_params: HashMap<String, serde_json::Value>,
}

impl DeviceConfig {
    pub fn feature_u64_param(&self, feature: &str, key: &str) -> Option<u64> {
        self.features
            .get(feature)
            .and_then(|config| config.feature_params.get(key))
            .and_then(serde_json::Value::as_u64)
    }

    pub fn feature_backend(&self, feature: &str) -> Option<&str> {
        self.features
            .get(feature)
            .filter(|cfg| !cfg.disabled)
            .and_then(|cfg| cfg.backend.as_deref())
            .filter(|backend| !backend.trim().is_empty())
    }

    pub fn feature_providers(&self, feature: &str) -> &[DeviceFeatureProviderConfig] {
        self.features
            .get(feature)
            .filter(|config| !config.disabled)
            .map(|config| config.providers.as_slice())
            .unwrap_or_default()
    }

    pub fn video_feature_params(&self) -> &HashMap<String, serde_json::Value> {
        static EMPTY: std::sync::LazyLock<HashMap<String, serde_json::Value>> =
            std::sync::LazyLock::new(HashMap::new);
        self.features
            .get("video")
            .map(|cfg| &cfg.feature_params)
            .unwrap_or(&EMPTY)
    }

    pub fn video_feature_string_param(&self, key: &str) -> Option<String> {
        self.video_feature_params()
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    }

    pub fn video_feature_u32_param(&self, key: &str) -> Option<u32> {
        self.video_feature_params()
            .get(key)
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
    }

    pub fn canonical_encoder_backend(&self) -> String {
        self.video_feature_string_param("encoder_backend")
            .unwrap_or_else(default_encoder_backend)
    }

    pub fn canonical_max_fps(&self) -> u32 {
        self.video_feature_u32_param("max_fps")
            .unwrap_or_else(default_max_fps)
    }

    pub fn canonical_video_device(&self) -> Option<String> {
        self.video_feature_string_param("video_device")
    }

    pub fn canonical_video_format(&self) -> Option<String> {
        self.video_feature_string_param("video_format")
    }

    pub fn canonical_video_width(&self) -> Option<u32> {
        self.video_feature_u32_param("video_width")
    }

    pub fn canonical_video_height(&self) -> Option<u32> {
        self.video_feature_u32_param("video_height")
    }

    pub fn canonical_video_fps(&self) -> Option<u32> {
        self.video_feature_u32_param("video_fps")
    }

    pub fn canonical_video_quality(&self) -> Option<u32> {
        self.video_feature_u32_param("video_quality")
    }

    fn ensure_video_param(&mut self, key: &str, value: serde_json::Value) {
        let entry = self.features.entry("video".to_string()).or_default();
        entry.feature_params.entry(key.to_string()).or_insert(value);
    }

    fn ensure_idrac_feature_defaults(&mut self) {
        if self.protocol != "idrac" {
            return;
        }

        for feature_id in ["video", "hid"] {
            let feature = self.features.entry(feature_id.to_string()).or_default();
            feature
                .backend
                .get_or_insert_with(|| "idrac-avct-console".to_string());
            if feature.backend.as_deref() == Some("idrac-avct-console") {
                feature
                    .backend_params
                    .entry("avct_port".to_string())
                    .or_insert_with(|| serde_json::json!(5900));
                feature
                    .backend_params
                    .entry("avct_tls_mode".to_string())
                    .or_insert_with(|| serde_json::json!("idrac-legacy-rsa"));
                feature
                    .backend_params
                    .entry("avct_virtual_media_privilege".to_string())
                    .or_insert_with(|| serde_json::json!(false));
            }
        }

        // iDRAC6 and iDRAC8 use distinct, source-profiled AVMP branches.
        // Do not expose virtual media for a generic `protocol: idrac` device:
        // selecting a generation by default would make the backend guess a
        // wire protocol. An explicitly configured MSD feature retains the
        // native backend defaults below and is validated by its connector.
        if let Some(msd) = self.features.get_mut("msd") {
            if msd.backend.as_deref() == Some("idrac-legacy-media") {
                msd.backend_params
                    .entry("avct_vm_port".to_string())
                    .or_insert_with(|| serde_json::json!(5900));
                msd.backend_params
                    .entry("avct_tls_mode".to_string())
                    .or_insert_with(|| serde_json::json!("idrac-legacy-rsa"));
            }
        }

        let power = self.features.entry("power".to_string()).or_default();
        power.backend.get_or_insert_with(|| "ipmi".to_string());
        if power.backend.as_deref() == Some("ipmi") {
            power
                .backend_params
                .entry("port".to_string())
                .or_insert_with(|| serde_json::json!(623));
        }
        let boot = self.features.entry("boot".to_string()).or_default();
        if boot.backend.as_deref() == Some("idrac-boot") {
            boot.backend = None;
        }
        if boot.backend.is_none() && boot.providers.is_empty() {
            boot.providers = vec![
                DeviceFeatureProviderConfig {
                    id: "ipmi".to_string(),
                    backend: "ipmi".to_string(),
                    backend_params: HashMap::new(),
                },
                DeviceFeatureProviderConfig {
                    id: "idrac-wsman".to_string(),
                    backend: "idrac-wsman".to_string(),
                    backend_params: HashMap::new(),
                },
            ];
        }
        let identify = self.features.entry("identify".to_string()).or_default();
        identify.backend.get_or_insert_with(|| "ipmi".to_string());
        if identify.backend.as_deref() == Some("ipmi") {
            identify
                .backend_params
                .entry("port".to_string())
                .or_insert_with(|| serde_json::json!(623));
        }
        self.video_backend = "idrac-avct-console".to_string();
    }

    pub fn normalize_feature_config(&mut self) {
        // Keep YAML-loaded iDRAC devices equivalent to the API preset. AVCT
        // must never be silently rewritten to the unrelated generic VNC
        // backend, and a missing power feature must not fall through to WOL.
        self.ensure_idrac_feature_defaults();
        let video_fallback = if self.protocol == "idrac" {
            "idrac-avct-console"
        } else {
            &self.video_backend
        };
        self.features
            .entry("video".to_string())
            .or_default()
            .backend
            .get_or_insert_with(|| video_fallback.to_string());

        let hid_backend = if self.protocol == "idrac" {
            "idrac-avct-console"
        } else {
            self.protocol.as_str()
        };
        self.features
            .entry("hid".to_string())
            .or_default()
            .backend
            .get_or_insert_with(|| hid_backend.to_string());

        // Legacy non-iDRAC configurations used `protocol` as the MSD backend.
        // Materialize that compatibility choice here so runtime registration
        // never needs to fall through from one backend to another.
        if self.protocol != "idrac" {
            self.features
                .entry("msd".to_string())
                .or_default()
                .backend
                .get_or_insert_with(|| self.protocol.clone());
        }
        if self.protocol == "mahogany" {
            self.features
                .entry("firmware".to_string())
                .or_default()
                .backend
                .get_or_insert_with(|| "mahogany".to_string());
        }

        let power_backend = if self.protocol == "idrac" || self.ipmi_host.is_some() {
            "ipmi"
        } else {
            "wol"
        };
        self.features
            .entry("power".to_string())
            .or_default()
            .backend
            .get_or_insert_with(|| power_backend.to_string());

        if self.protocol == "idrac" {
            self.features
                .entry("identify".to_string())
                .or_default()
                .backend
                .get_or_insert_with(|| "ipmi".to_string());
        } else if self.protocol == "redfish" {
            self.features
                .entry("identify".to_string())
                .or_default()
                .backend
                .get_or_insert_with(|| "redfish".to_string());
        }
        self.ensure_video_param("max_fps", serde_json::json!(self.max_fps));
        self.ensure_video_param("encoder_backend", serde_json::json!(self.encoder_backend));
        if let Some(value) = self.video_device.take() {
            self.ensure_video_param("video_device", serde_json::json!(value));
        }
        if let Some(value) = self.video_format.take() {
            self.ensure_video_param("video_format", serde_json::json!(value));
        }
        if let Some(value) = self.video_width.take() {
            self.ensure_video_param("video_width", serde_json::json!(value));
        }
        if let Some(value) = self.video_height.take() {
            self.ensure_video_param("video_height", serde_json::json!(value));
        }
        if let Some(value) = self.video_fps.take() {
            self.ensure_video_param("video_fps", serde_json::json!(value));
        }
        if let Some(value) = self.video_quality.take() {
            self.ensure_video_param("video_quality", serde_json::json!(value));
        }
        if let Some(audio) = self.audio_backend.as_deref() {
            self.features
                .entry("audio".to_string())
                .or_default()
                .backend
                .get_or_insert_with(|| audio.to_string());
        }
        for feature in self.features.values_mut() {
            let mut used = std::collections::HashSet::new();
            for (index, provider) in feature.providers.iter_mut().enumerate() {
                if provider.id.trim().is_empty() || !used.insert(provider.id.clone()) {
                    let base = provider
                        .backend
                        .chars()
                        .map(|character| {
                            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                                character
                            } else {
                                '-'
                            }
                        })
                        .collect::<String>();
                    let mut candidate = format!("{base}-{}", index + 1);
                    let mut suffix = index + 1;
                    while !used.insert(candidate.clone()) {
                        suffix += 1;
                        candidate = format!("{base}-{suffix}");
                    }
                    provider.id = candidate;
                }
            }
            if feature.disabled {
                feature.backend = None;
                feature.providers.clear();
            }
        }
    }
}

fn default_max_fps() -> u32 {
    60
}
fn is_false(value: &bool) -> bool {
    !*value
}
fn default_video_backend() -> String {
    "mahogany".to_string()
}
fn default_encoder_backend() -> String {
    "software".to_string()
}
fn default_msd_backend() -> String {
    "mahogany".to_string()
}
fn default_msd_drive_size_mb() -> u64 {
    1024
}
fn default_webtransport_listen() -> String {
    "0.0.0.0:8443".to_string()
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}

fn default_download_connect_timeout_seconds() -> u64 {
    15
}

fn default_download_idle_timeout_seconds() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub username: String,
    pub password: String,
}

impl Config {
    pub fn load(path: &str) -> Result<Self, anyhow::Error> {
        let content = std::fs::read_to_string(path)?;
        let content = Self::subst_env(&content)?;
        let mut config: Self = serde_yaml::from_str(&content)?;
        config.validate_device_ids()?;
        config.normalize_feature_config();
        Ok(config)
    }

    pub fn validate_device_ids(&self) -> Result<(), anyhow::Error> {
        let mut ids = std::collections::HashSet::new();
        for device in &self.devices {
            anyhow::ensure!(
                !device.id.is_empty()
                    && device
                        .id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
                "invalid device id: use letters, numbers, '-' or '_'"
            );
            anyhow::ensure!(ids.insert(&device.id), "duplicate device id");
        }
        Ok(())
    }

    pub fn save(&self, path: &str) -> Result<(), anyhow::Error> {
        self.save_with_directory_sync(path, |directory| directory.sync_all())
    }

    fn save_with_directory_sync(
        &self,
        path: &str,
        sync: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
    ) -> Result<(), anyhow::Error> {
        use std::io::Write;

        self.validate_device_ids()?;
        let mut normalized = self.clone();
        normalized.normalize_feature_config();
        let content = serde_yaml::to_string(&normalized)?;
        let path = std::path::Path::new(path);
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("configuration path has no valid filename"))?;
        let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<(), anyhow::Error> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            // Open the directory before the commit point. Permissions are already
            // established on the temporary inode and preserved by rename.
            let directory = std::fs::File::open(parent)?;
            std::fs::rename(&temporary, path)?;
            // Rename has published the configuration. Report degraded crash durability
            // without pretending publication failed and leaving memory on the old version.
            if let Err(error) = sync(&directory) {
                tracing::error!(%error, "configuration published, directory durability is uncertain");
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        Ok(())
    }

    pub fn normalize_feature_config(&mut self) {
        for dev in &mut self.devices {
            dev.normalize_feature_config();
        }
    }

    fn subst_env(content: &str) -> Result<String, anyhow::Error> {
        let mut result = String::with_capacity(content.len());
        let mut remaining = content;
        while let Some(start) = remaining.find("${") {
            result.push_str(&remaining[..start]);
            let placeholder = &remaining[start + 2..];
            let end = placeholder
                .find('}')
                .ok_or_else(|| anyhow::anyhow!("unterminated environment variable placeholder"))?;
            let var_name = &placeholder[..end];
            if var_name.is_empty() || var_name.contains(['$', '{']) {
                anyhow::bail!("invalid environment variable placeholder");
            }
            let value = std::env::var(var_name).map_err(|_| {
                anyhow::anyhow!("required environment variable '{var_name}' is not set")
            })?;
            // Environment values are literal replacement data, never another template.
            result.push_str(&value);
            remaining = &placeholder[end + 1..];
        }
        result.push_str(remaining);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn relative_configuration_filename_saves_and_reloads() {
        let filename = format!(".bmc-config-test-{}.yaml", uuid::Uuid::new_v4());
        struct OwnedFile(String);
        impl Drop for OwnedFile {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _owned = OwnedFile(filename.clone());
        let config: Config = serde_yaml::from_str(
            "server:\n  listen: 127.0.0.1:12340\n  frontend_dir: frontend/dist\ndevices: []\nmsd: {}\n"
        ).unwrap();
        config.save(&filename).unwrap();
        assert_eq!(
            Config::load(&filename).unwrap().server.listen,
            "127.0.0.1:12340"
        );
    }

    #[test]
    fn environment_replacements_are_literal_and_not_recursively_expanded() {
        env::set_var("BMC_TEST_LITERAL_ENV", "${BMC_TEST_LITERAL_ENV}");
        assert_eq!(
            Config::subst_env("prefix_${BMC_TEST_LITERAL_ENV}_suffix").unwrap(),
            "prefix_${BMC_TEST_LITERAL_ENV}_suffix"
        );
        env::set_var("BMC_TEST_LITERAL_VALUE", "a${UNSET_BMC_LITERAL_VALUE}b");
        assert_eq!(
            Config::subst_env("${BMC_TEST_LITERAL_VALUE}").unwrap(),
            "a${UNSET_BMC_LITERAL_VALUE}b"
        );
    }

    #[test]
    fn test_subst_env_no_vars() {
        let result = Config::subst_env("hello world").unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_subst_env_single_var() {
        env::set_var("TEST_VAR", "test_value");
        let result = Config::subst_env("prefix_${TEST_VAR}_suffix").unwrap();
        assert_eq!(result, "prefix_test_value_suffix");
        env::remove_var("TEST_VAR");
    }

    #[test]
    fn test_subst_env_missing_var() {
        assert!(Config::subst_env("hello ${NONEXISTENT_VAR_12345} world").is_err());
    }

    #[test]
    fn test_subst_env_multiple_vars() {
        env::set_var("A", "1");
        env::set_var("B", "2");
        let result = Config::subst_env("${A}-${B}").unwrap();
        assert_eq!(result, "1-2");
        env::remove_var("A");
        env::remove_var("B");
    }

    #[test]
    fn test_subst_env_nested_not_supported() {
        env::set_var("INNER", "x");
        assert!(Config::subst_env("${IN${INNER}}").is_err());
        env::remove_var("INNER");
    }

    #[test]
    fn test_subst_env_dollar_brace_no_close() {
        assert!(Config::subst_env("${UNCLOSED").is_err());
    }

    #[test]
    fn test_subst_env_empty_var() {
        env::set_var("EMPTY", "");
        let result = Config::subst_env("before${EMPTY}after").unwrap();
        assert_eq!(result, "beforeafter");
        env::remove_var("EMPTY");
    }

    #[test]
    fn test_load_with_env() {
        env::set_var("TEST_PASS", "hunter2");
        let dir = std::env::temp_dir();
        let path = dir.join("test_config_XXXX.yaml");
        let yaml = "server:\n  listen: \"127.0.0.1:0\"\n  frontend_dir: \"/tmp\"\ndevices:\n  - id: test\n    name: Test\n    protocol: mahogany\n    host: 192.168.1.1\n    port: 443\n    auth:\n      username: admin\n      password: \"${TEST_PASS}\"\nmsd: {}";
        std::fs::write(&path, yaml).unwrap();
        let config = Config::load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.devices[0].auth.password, "hunter2");
        std::fs::remove_file(&path).unwrap();
        env::remove_var("TEST_PASS");
    }

    #[test]
    fn test_default_max_fps() {
        assert_eq!(default_max_fps(), 60);
    }

    #[test]
    fn legacy_video_values_are_migrated_once_and_never_read_by_runtime_accessors() {
        let mut device: DeviceConfig = serde_yaml::from_str(
            r#"
id: test
name: Test
protocol: vnc
host: localhost
port: 5900
auth: {username: fixture, password: fixture}
max_fps: 30
encoder_backend: webp
video_device: legacy-camera
video_format: legacy-format
video_width: 640
video_height: 480
video_fps: 20
video_quality: 70
"#,
        )
        .unwrap();
        assert_eq!(device.canonical_max_fps(), 60);
        assert_eq!(device.canonical_encoder_backend(), "software");
        assert_eq!(device.canonical_video_device(), None);
        device.normalize_feature_config();
        assert_eq!(device.canonical_max_fps(), 30);
        assert_eq!(device.canonical_encoder_backend(), "webp");
        assert_eq!(
            device.canonical_video_device().as_deref(),
            Some("legacy-camera")
        );
        assert_eq!(
            device.canonical_video_format().as_deref(),
            Some("legacy-format")
        );
        assert_eq!(device.canonical_video_width(), Some(640));
        assert_eq!(device.canonical_video_height(), Some(480));
        assert_eq!(device.canonical_video_fps(), Some(20));
        assert_eq!(device.canonical_video_quality(), Some(70));
        let feature = device.features.get_mut("video").unwrap();
        feature
            .feature_params
            .insert("max_fps".into(), serde_json::json!(15));
        for key in [
            "video_device",
            "video_format",
            "video_width",
            "video_height",
            "video_fps",
            "video_quality",
            "encoder_backend",
        ] {
            feature
                .feature_params
                .insert(key.into(), serde_json::Value::Null);
        }
        device.max_fps = 99;
        device.encoder_backend = "legacy-encoder".into();
        device.video_device = Some("stale-camera".into());
        device.video_format = Some("stale-format".into());
        device.video_width = Some(999);
        device.video_height = Some(999);
        device.video_fps = Some(99);
        device.video_quality = Some(99);
        assert_eq!(device.canonical_max_fps(), 15);
        assert_eq!(device.canonical_encoder_backend(), "software");
        assert_eq!(device.canonical_video_device(), None);
        assert_eq!(device.canonical_video_format(), None);
        assert_eq!(device.canonical_video_width(), None);
        assert_eq!(device.canonical_video_height(), None);
        assert_eq!(device.canonical_video_fps(), None);
        assert_eq!(device.canonical_video_quality(), None);
        device.normalize_feature_config();
        assert_eq!(device.canonical_max_fps(), 15);
        assert_eq!(device.canonical_video_device(), None);
        assert_eq!(device.canonical_encoder_backend(), "software");
    }

    #[test]
    fn idrac_yaml_normalization_materializes_the_native_console_and_ipmi_defaults() {
        let mut config: Config = serde_yaml::from_str(
            r#"
server:
  listen: 127.0.0.1:0
  frontend_dir: /tmp
devices:
  - id: idrac-test
    name: iDRAC test
    protocol: idrac
    host: 192.0.2.10
    port: 443
    auth:
      username: test-user
      password: test-password
    features:
      msd:
        backend: idrac-legacy-media
        backend_params:
          idrac_generation: 6
msd: {}
"#,
        )
        .unwrap();

        config.normalize_feature_config();
        let device = &config.devices[0];
        for feature_id in ["video", "hid"] {
            let feature = &device.features[feature_id];
            assert_eq!(feature.backend.as_deref(), Some("idrac-avct-console"));
            assert_eq!(feature.backend_params["avct_port"], 5900);
            assert_eq!(feature.backend_params["avct_tls_mode"], "idrac-legacy-rsa");
            assert_eq!(
                feature.backend_params["avct_virtual_media_privilege"],
                false
            );
        }
        let power = &device.features["power"];
        assert_eq!(power.backend.as_deref(), Some("ipmi"));
        assert_eq!(power.backend_params["port"], 623);
        let boot = &device.features["boot"];
        assert!(boot.backend.is_none());
        assert_eq!(
            boot.providers
                .iter()
                .map(|provider| provider.backend.as_str())
                .collect::<Vec<_>>(),
            vec!["ipmi", "idrac-wsman"]
        );
        let msd = &device.features["msd"];
        assert_eq!(msd.backend.as_deref(), Some("idrac-legacy-media"));
        assert_eq!(msd.backend_params["idrac_generation"], 6);
        assert_eq!(msd.backend_params["avct_vm_port"], 5900);
        assert_eq!(msd.backend_params["avct_tls_mode"], "idrac-legacy-rsa");
    }

    #[test]
    fn idrac_normalization_keeps_explicit_feature_backend_choices() {
        let mut config: Config = serde_yaml::from_str(
            r#"
server:
  listen: 127.0.0.1:0
  frontend_dir: /tmp
devices:
  - id: idrac-explicit
    name: iDRAC explicit
    protocol: idrac
    host: 192.0.2.11
    port: 443
    auth:
      username: test-user
      password: test-password
    features:
      video:
        backend: vnc
        backend_params:
          vnc_port: 5901
      power:
        backend: redfish
msd: {}
"#,
        )
        .unwrap();

        config.normalize_feature_config();
        let device = &config.devices[0];
        assert_eq!(device.features["video"].backend.as_deref(), Some("vnc"));
        assert!(!device.features["video"]
            .backend_params
            .contains_key("avct_port"));
        assert_eq!(device.features["power"].backend.as_deref(), Some("redfish"));
        assert!(!device.features["power"].backend_params.contains_key("port"));
        assert_eq!(
            device.features["hid"].backend.as_deref(),
            Some("idrac-avct-console")
        );
        assert!(!device.features.contains_key("msd"));
    }

    #[test]
    fn idrac_normalization_preserves_each_explicit_boot_provider() {
        for backend in ["idrac-wsman", "ipmi", "redfish"] {
            let yaml = format!(
                r#"
server:
  listen: 127.0.0.1:0
  frontend_dir: /tmp
devices:
  - id: idrac-explicit-boot
    name: iDRAC explicit boot
    protocol: idrac
    host: 192.0.2.12
    port: 443
    auth:
      username: test-user
      password: test-password
    features:
      boot:
        backend: {backend}
msd: {{}}
"#
            );
            let mut config: Config = serde_yaml::from_str(&yaml).unwrap();
            config.normalize_feature_config();
            assert_eq!(
                config.devices[0].features["boot"].backend.as_deref(),
                Some(backend)
            );
        }
    }

    #[test]
    fn legacy_mahogany_defaults_are_materialized_as_explicit_feature_backends() {
        let mut config: Config = serde_yaml::from_str(
            r#"
server:
  listen: 127.0.0.1:0
  frontend_dir: /tmp
devices:
  - id: mahogany-test
    name: Mahogany test
    protocol: mahogany
    host: 192.0.2.20
    port: 443
    auth:
      username: test-user
      password: test-password
msd: {}
"#,
        )
        .unwrap();

        config.normalize_feature_config();
        let device = &config.devices[0];
        for feature_id in ["video", "hid", "msd", "firmware"] {
            assert_eq!(
                device.features[feature_id].backend.as_deref(),
                Some("mahogany")
            );
        }
        assert_eq!(device.features["power"].backend.as_deref(), Some("wol"));
        assert_eq!(device.feature_backend("video"), Some("mahogany"));
        assert_eq!(device.feature_backend("missing"), None);
    }

    #[cfg(unix)]
    #[test]
    fn save_is_atomic_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        let config: Config = serde_yaml::from_str(
            "server:\n  listen: 127.0.0.1:1234\n  frontend_dir: /tmp\ndevices: []\nmsd: {}\n",
        )
        .unwrap();
        config.save(path.to_str().unwrap()).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            Config::load(path.to_str().unwrap()).unwrap().server.listen,
            "127.0.0.1:1234"
        );
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
    }
}

#[cfg(test)]
mod publication_failure_tests {
    use super::*;
    #[test]
    fn post_rename_sync_failure_is_a_published_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        let config: Config = serde_yaml::from_str("server:\n  listen: 127.0.0.1:12340\n  frontend_dir: frontend/dist\ndevices: []\nmsd: {}\n").unwrap();
        config
            .save_with_directory_sync(path.to_str().unwrap(), |_| {
                Err(std::io::Error::other("injected directory sync failure"))
            })
            .unwrap();
        assert_eq!(
            Config::load(path.to_str().unwrap()).unwrap().server.listen,
            config.server.listen
        );
    }
    #[test]
    fn yaml_rejects_unsafe_or_duplicate_device_ids() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        let base = "server:\n  listen: 127.0.0.1:12340\n  frontend_dir: frontend/dist\ndevices:\n";
        let device = |id: &str| {
            format!("  - id: {id}\n    name: Test\n    protocol: ipmi\n    host: localhost\n    port: 623\n    auth: {{username: admin, password: placeholder}}\n")
        };
        for devices in [
            device("a.b"),
            format!("{}{}", device("duplicate"), device("duplicate")),
        ] {
            std::fs::write(&path, format!("{base}{devices}")).unwrap();
            assert!(Config::load(path.to_str().unwrap()).is_err());
        }
    }
}
