use serde::Deserialize;
/// Configuration loading from ~/.klepto/config.toml.
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::profiles::NetworkProfile;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Path or name of the oh-my-pi (`omp`) binary.
    #[serde(default = "default_omp_bin", alias = "pi_bin")]
    pub omp_bin: String,
    pub default_model: Option<String>,
    pub default_profile: Option<String>,
    #[serde(default)]
    pub env: Option<std::collections::HashMap<String, String>>,
    #[serde(default = "default_networks")]
    pub networks: BTreeMap<String, NetworkProfile>,
    pub token: Option<String>,
    /// When true (default), missing tmux/omp/rg are installed automatically.
    #[serde(default = "default_true")]
    pub auto_install_deps: bool,
}

fn default_listen() -> String {
    "127.0.0.1:7420".to_string()
}

fn default_omp_bin() -> String {
    "omp".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            omp_bin: default_omp_bin(),
            default_model: None,
            default_profile: Some("coding".into()),
            env: None,
            networks: default_networks(),
            token: None,
            auto_install_deps: true,
        }
    }
}

impl Config {
    /// Klepto intentionally uses one portable home instead of platform-specific config dirs.
    pub fn home_dir() -> PathBuf {
        if let Ok(path) = std::env::var("KLEPTO_HOME") {
            if !path.trim().is_empty() {
                return PathBuf::from(path);
            }
        }
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".klepto")
    }

    pub fn path() -> PathBuf {
        Self::home_dir().join("config.toml")
    }

    pub fn data_dir() -> PathBuf {
        Self::home_dir()
    }

    pub fn load() -> Result<Self, String> {
        let path = Self::path();
        if !path.exists() {
            // Read the previous platform-specific location as a migration fallback.
            let legacy = dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("klepto/config.toml");
            if legacy.exists() {
                let content = std::fs::read_to_string(&legacy)
                    .map_err(|e| format!("failed to read legacy config: {e}"))?;
                return toml::from_str(&content)
                    .map_err(|e| format!("failed to parse legacy config: {e}"));
            }
            tracing::info!("no config found at {}, using defaults", path.display());
            return Ok(Self::default());
        }
        let content =
            std::fs::read_to_string(&path).map_err(|e| format!("failed to read config: {}", e))?;
        toml::from_str(&content).map_err(|e| format!("failed to parse config: {}", e))
    }

    /// Resolve listen address: CLI flag > KLEPTO_LISTEN > config.toml > default.
    pub fn resolve_listen(cli_listen: Option<&str>) -> String {
        if let Some(l) = cli_listen.map(str::trim).filter(|s| !s.is_empty()) {
            return l.to_string();
        }
        if let Ok(l) = std::env::var("KLEPTO_LISTEN") {
            let l = l.trim();
            if !l.is_empty() {
                return l.to_string();
            }
        }
        Self::load()
            .map(|c| c.listen)
            .unwrap_or_else(|_| default_listen())
    }

    pub fn ensure_data_dir(&self) -> Result<(), String> {
        let data = Self::data_dir();
        std::fs::create_dir_all(&data).map_err(|e| format!("failed to create data dir: {}", e))?;
        crate::profiles::ensure_builtin_profile_examples()?;
        crate::skills::ensure_builtin_skills()?;
        Ok(())
    }
}

fn default_networks() -> BTreeMap<String, NetworkProfile> {
    [
        ("direct".into(), NetworkProfile::default()),
        (
            "none".into(),
            NetworkProfile {
                mode: crate::profiles::NetworkMode::None,
                ..NetworkProfile::default()
            },
        ),
    ]
    .into_iter()
    .collect()
}
