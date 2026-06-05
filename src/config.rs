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
    #[serde(default)]
    pub download_headers: HashMap<String, String>,
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
    #[serde(default = "default_max_fps")]
    pub max_fps: u32,
    #[serde(default = "default_video_backend")]
    pub video_backend: String,
    #[serde(default = "default_encoder_backend")]
    pub encoder_backend: String,
    #[serde(default)]
    pub audio_backend: Option<String>,
    #[serde(default)]
    pub video_device: Option<String>,
    #[serde(default)]
    pub video_format: Option<String>,
    #[serde(default)]
    pub video_width: Option<u32>,
    #[serde(default)]
    pub video_height: Option<u32>,
    #[serde(default)]
    pub video_fps: Option<u32>,
    #[serde(default)]
    pub video_quality: Option<u32>,
}

fn default_max_fps() -> u32 { 60 }
fn default_video_backend() -> String { "mahogany".to_string() }
fn default_encoder_backend() -> String { "software".to_string() }
fn default_msd_backend() -> String { "mahogany".to_string() }
fn default_msd_drive_size_mb() -> u64 { 1024 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub username: String,
    pub password: String,
}

impl Config {
    pub fn load(path: &str) -> Result<Self, anyhow::Error> {
        let content = std::fs::read_to_string(path)?;
        let content = Self::subst_env(&content);
        Ok(serde_yaml::from_str(&content)?)
    }

    pub fn save(&self, path: &str) -> Result<(), anyhow::Error> {
        let content = serde_yaml::to_string(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    fn subst_env(content: &str) -> String {
        let mut result = content.to_string();
        loop {
            let start = match result.find("${") {
                Some(s) => s,
                None => break,
            };
            let end = match result[start..].find('}') {
                Some(e) => start + e,
                None => break,
            };
            let var_name = &result[start + 2..end];
            let value = std::env::var(var_name).unwrap_or_default();
            result.replace_range(start..=end, &value);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_subst_env_no_vars() {
        let result = Config::subst_env("hello world");
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_subst_env_single_var() {
        env::set_var("TEST_VAR", "test_value");
        let result = Config::subst_env("prefix_${TEST_VAR}_suffix");
        assert_eq!(result, "prefix_test_value_suffix");
        env::remove_var("TEST_VAR");
    }

    #[test]
    fn test_subst_env_missing_var() {
        // Missing vars expand to empty string
        let result = Config::subst_env("hello ${NONEXISTENT_VAR_12345} world");
        assert_eq!(result, "hello  world");
    }

    #[test]
    fn test_subst_env_multiple_vars() {
        env::set_var("A", "1");
        env::set_var("B", "2");
        let result = Config::subst_env("${A}-${B}");
        assert_eq!(result, "1-2");
        env::remove_var("A");
        env::remove_var("B");
    }

    #[test]
    fn test_subst_env_nested_not_supported() {
        env::set_var("INNER", "x");
        let result = Config::subst_env("${IN${INNER}}");
        // Finds first ${ at 0, first } at 11 → var_name = "IN${INNER}"
        // which doesn't exist → replaced with "" → result is "}"
        assert_eq!(result, "}");
        env::remove_var("INNER");
    }

    #[test]
    fn test_subst_env_dollar_brace_no_close() {
        let result = Config::subst_env("${UNCLOSED");
        assert_eq!(result, "${UNCLOSED");
    }

    #[test]
    fn test_subst_env_empty_var() {
        env::set_var("EMPTY", "");
        let result = Config::subst_env("before${EMPTY}after");
        assert_eq!(result, "beforeafter");
        env::remove_var("EMPTY");
    }

    #[test]
    fn test_load_with_env() {
        env::set_var("TEST_PASS", "hunter2");
        let dir = std::env::temp_dir();
        let path = dir.join("test_config_XXXX.yaml");
        let yaml = format!(
            "server:\n  listen: \"127.0.0.1:0\"\n  frontend_dir: \"/tmp\"\ndevices:\n  - id: test\n    name: Test\n    protocol: mahogany\n    host: 192.168.1.1\n    port: 443\n    auth:\n      username: admin\n      password: \"${{TEST_PASS}}\"\nmsd: {{}}"
        );
        std::fs::write(&path, &yaml).unwrap();
        let config = Config::load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.devices[0].auth.password, "hunter2");
        std::fs::remove_file(&path).unwrap();
        env::remove_var("TEST_PASS");
    }

    #[test]
    fn test_default_max_fps() {
        assert_eq!(default_max_fps(), 60);
    }
}
