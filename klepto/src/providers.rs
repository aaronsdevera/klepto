//! Klepto-owned provider catalog with a narrow adapter to omp's models.yml.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use crate::artifacts::SCHEMA_VERSION;
use crate::config::Config;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    OpenaiCompatible,
    Ollama,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDefinition {
    pub id: String,
    #[serde(default)]
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    pub api: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCatalog {
    pub schema_version: u32,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderDefinition>,
}

impl Default for ProviderCatalog {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            providers: BTreeMap::new(),
        }
    }
}

pub fn load_catalog() -> ProviderCatalog {
    fs::read_to_string(catalog_path())
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn upsert(
    mut definition: ProviderDefinition,
    api_key: Option<&str>,
) -> Result<ProviderCatalog, String> {
    definition.id = definition.id.trim().to_string();
    if definition.id.is_empty()
        || !definition
            .id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("provider id must contain only letters, numbers, '-' or '_'".into());
    }
    definition.models.retain(|model| !model.trim().is_empty());
    definition.models.sort();
    definition.models.dedup();
    let mut catalog = load_catalog();
    catalog
        .providers
        .insert(definition.id.clone(), definition.clone());
    let path = catalog_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create provider dir: {e}"))?;
    }
    fs::write(
        &path,
        toml::to_string_pretty(&catalog).map_err(|e| format!("serialize providers: {e}"))?,
    )
    .map_err(|e| format!("write provider catalog: {e}"))?;
    materialize_omp(&definition, api_key)?;
    Ok(catalog)
}

pub fn remove(id: &str) -> Result<ProviderCatalog, String> {
    let mut catalog = load_catalog();
    if catalog.providers.remove(id).is_none() {
        return Err(format!("provider '{id}' not found"));
    }
    fs::write(
        catalog_path(),
        toml::to_string_pretty(&catalog).map_err(|e| format!("serialize providers: {e}"))?,
    )
    .map_err(|e| format!("write provider catalog: {e}"))?;
    remove_from_omp(id)?;
    Ok(catalog)
}

pub fn api_key(id: &str) -> Option<String> {
    let root = read_models_yml();
    let key = root
        .get("providers")
        .and_then(|providers| providers.get(id))
        .and_then(|provider| provider.get("apiKey"))
        .and_then(|key| key.as_str())
        .filter(|key| !key.is_empty() && *key != "no-key" && *key != "none")?;
    // omp treats apiKey as env-var-name-or-literal.
    if let Ok(from_env) = std::env::var(key) {
        if !from_env.is_empty() {
            return Some(from_env);
        }
    }
    Some(key.to_string())
}

/// Default local Ollama — omp discovers it keylessly; do not duplicate in models.yml.
pub fn is_default_local_ollama(definition: &ProviderDefinition) -> bool {
    if definition.kind != ProviderKind::Ollama && definition.id != "ollama" {
        return false;
    }
    let Some(base) = definition.base_url.as_deref() else {
        return definition.id == "ollama";
    };
    let root = ollama_root_url(base);
    root == "http://127.0.0.1:11434" || root == "http://localhost:11434"
}

fn materialize_omp(definition: &ProviderDefinition, api_key: Option<&str>) -> Result<(), String> {
    if is_default_local_ollama(definition) {
        // Prefer omp's built-in keyless Ollama discovery.
        remove_from_omp(&definition.id)?;
        return Ok(());
    }

    let dir = omp_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("create omp agent dir: {e}"))?;
    let models_path = dir.join("models.yml");
    let mut root = read_models_yml();
    let providers = root
        .as_object_mut()
        .ok_or("omp models.yml root must be a mapping")?
        .entry("providers".to_string())
        .or_insert_with(|| json!({}));
    let providers = providers
        .as_object_mut()
        .ok_or("omp models.yml providers must be a mapping")?;

    let mut provider = providers
        .get(&definition.id)
        .cloned()
        .unwrap_or_else(|| json!({}));
    let provider_obj = provider
        .as_object_mut()
        .ok_or("omp provider entry must be a mapping")?;

    let existing_had_overrides = provider_obj.contains_key("modelOverrides");
    let existing_had_discovery = provider_obj.contains_key("discovery");

    if let Some(base_url) = definition.base_url.as_deref() {
        let base_url = match definition.kind {
            ProviderKind::Ollama => ollama_openai_base_url(base_url),
            ProviderKind::OpenaiCompatible => base_url.trim_end_matches('/').to_string(),
        };
        provider_obj.insert("baseUrl".into(), json!(base_url));
        provider_obj.insert(
            "api".into(),
            json!(
                definition
                    .api
                    .clone()
                    .unwrap_or_else(|| "openai-completions".into())
            ),
        );
        if !existing_had_discovery
            && (definition.kind == ProviderKind::OpenaiCompatible
                || definition.kind == ProviderKind::Ollama)
        {
            provider_obj.insert("discovery".into(), json!({ "type": "openai-models-list" }));
        }
    }

    // Do not clobber user-authored modelOverrides / discovery-only catalogs.
    if !definition.models.is_empty() && !existing_had_overrides {
        let models: Vec<serde_json::Value> = definition
            .models
            .iter()
            .map(|id| {
                json!({
                    "id": id,
                    "name": id,
                    "contextWindow": 128000,
                    "maxTokens": 8192,
                })
            })
            .collect();
        provider_obj.insert("models".into(), json!(models));
    }

    match api_key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => {
            provider_obj.insert("apiKey".into(), json!(key));
            provider_obj.remove("auth");
        }
        None if definition.kind == ProviderKind::Ollama
            || definition
                .base_url
                .as_deref()
                .is_some_and(|url| url.contains("127.0.0.1") || url.contains("localhost")) =>
        {
            provider_obj.insert("auth".into(), json!("none"));
            provider_obj.remove("apiKey");
        }
        None => {}
    }

    if provider_obj.is_empty() {
        return Ok(());
    }

    providers.insert(definition.id.clone(), provider);
    write_private_yaml(&models_path, &root)
}

fn remove_from_omp(id: &str) -> Result<(), String> {
    let models_path = omp_dir().join("models.yml");
    if !models_path.exists() {
        return Ok(());
    }
    let mut root = read_models_yml();
    if let Some(providers) = root
        .get_mut("providers")
        .and_then(|value| value.as_object_mut())
    {
        providers.remove(id);
        write_private_yaml(&models_path, &root)?;
    }
    Ok(())
}

fn read_models_yml() -> serde_json::Value {
    let path = omp_dir().join("models.yml");
    let Ok(raw) = fs::read_to_string(&path) else {
        return json!({ "providers": {} });
    };
    serde_yaml::from_str::<serde_json::Value>(&raw).unwrap_or_else(|_| json!({ "providers": {} }))
}

fn write_private_yaml(path: &PathBuf, value: &serde_json::Value) -> Result<(), String> {
    let yaml =
        serde_yaml::to_string(value).map_err(|e| format!("serialize omp models.yml: {e}"))?;
    fs::write(path, yaml).map_err(|e| format!("write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(())
}

fn catalog_path() -> PathBuf {
    Config::home_dir().join("models.toml")
}

pub fn omp_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".omp/agent")
}

pub fn ollama_root_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/api")
        .or_else(|| trimmed.strip_suffix("/v1"))
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

pub fn ollama_openai_base_url(base_url: &str) -> String {
    format!("{}/v1", ollama_root_url(base_url))
}

/// Static model ids listed under providers in models.yml.
pub fn from_models_yml() -> Vec<(String, String)> {
    let root = read_models_yml();
    let Some(providers) = root.get("providers").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (provider, cfg) in providers {
        let Some(models) = cfg.get("models").and_then(|m| m.as_array()) else {
            continue;
        };
        for m in models {
            let id = m
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            out.push((provider.clone(), id));
        }
    }
    out
}

/// Endpoint that can be probed for a live model list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryTarget {
    pub id: String,
    pub base_url: String,
    pub kind: ProviderKind,
    /// Write discovered ids back into the Klepto catalog (never user-owned yml).
    pub persist: bool,
    /// Surface fetch errors in the models response. Implicit probes stay quiet.
    pub required: bool,
}

pub fn is_loopback_url(base_url: &str) -> bool {
    let host = url_host(base_url);
    host == "127.0.0.1" || host == "localhost" || host == "::1"
}

fn url_host(base_url: &str) -> String {
    let trimmed = base_url.trim();
    let after_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    if let Some(rest) = hostport.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(hostport).to_string();
    }
    hostport
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(hostport)
        .to_string()
}

/// Infer whether a models.yml endpoint is Ollama or OpenAI-compatible.
pub fn infer_discovery_kind(id: &str, api: Option<&str>, base_url: &str) -> ProviderKind {
    let id_l = id.to_ascii_lowercase();
    let api_l = api.unwrap_or("").to_ascii_lowercase();
    if id_l == "ollama"
        || id_l.starts_with("ollama-")
        || api_l.contains("ollama")
        || ollama_root_url(base_url).ends_with(":11434")
    {
        return ProviderKind::Ollama;
    }
    ProviderKind::OpenaiCompatible
}

/// models.yml providers that advertise a base URL (live /models or /api/tags).
pub fn targets_from_models_yml(root: &serde_json::Value) -> Vec<DiscoveryTarget> {
    let Some(providers) = root.get("providers").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, cfg) in providers {
        let Some(base_url) = cfg
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|url| !url.is_empty())
        else {
            continue;
        };
        let api = cfg.get("api").and_then(|v| v.as_str());
        let kind = infer_discovery_kind(id, api, base_url);
        out.push(DiscoveryTarget {
            id: id.clone(),
            base_url: base_url.to_string(),
            kind,
            persist: false,
            required: !is_loopback_url(base_url),
        });
    }
    out
}

/// Catalog + models.yml endpoints to probe. Klepto-owned rows may be persisted.
pub fn discovery_targets() -> Vec<DiscoveryTarget> {
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();

    for (id, definition) in load_catalog().providers {
        let base_url = match definition.base_url.clone() {
            Some(url) if !url.trim().is_empty() => url,
            _ if definition.kind == ProviderKind::Ollama || definition.id == "ollama" => {
                "http://127.0.0.1:11434".into()
            }
            _ => continue,
        };
        let persist = !is_default_local_ollama(&definition);
        seen.insert(id.clone());
        targets.push(DiscoveryTarget {
            id,
            base_url,
            kind: definition.kind,
            persist,
            required: persist,
        });
    }

    for target in targets_from_models_yml(&read_models_yml()) {
        if seen.contains(&target.id) {
            continue;
        }
        seen.insert(target.id.clone());
        targets.push(target);
    }

    targets
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderDefinition, ProviderKind, infer_discovery_kind, is_default_local_ollama,
        is_loopback_url, ollama_openai_base_url, ollama_root_url, targets_from_models_yml,
    };
    use serde_json::json;

    #[test]
    fn normalizes_ollama_native_and_openai_urls() {
        assert_eq!(
            ollama_root_url("http://127.0.0.1:11434/api/"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            ollama_root_url("http://127.0.0.1:11434/v1"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            ollama_openai_base_url("http://127.0.0.1:11434/api"),
            "http://127.0.0.1:11434/v1"
        );
    }

    #[test]
    fn detects_default_local_ollama() {
        assert!(is_default_local_ollama(&ProviderDefinition {
            id: "ollama".into(),
            kind: ProviderKind::Ollama,
            base_url: Some("http://127.0.0.1:11434".into()),
            api: None,
            models: vec![],
        }));
        assert!(!is_default_local_ollama(&ProviderDefinition {
            id: "ollama-remote".into(),
            kind: ProviderKind::Ollama,
            base_url: Some("http://10.0.0.2:11434".into()),
            api: None,
            models: vec![],
        }));
    }

    #[test]
    fn classifies_loopback_and_remote_hosts() {
        assert!(is_loopback_url("http://127.0.0.1:11434/v1"));
        assert!(is_loopback_url("http://localhost:8000/v1"));
        assert!(!is_loopback_url("https://inference.example/v1"));
    }

    #[test]
    fn infers_ollama_from_id_port_or_api() {
        assert_eq!(
            infer_discovery_kind("ollama-work", None, "http://10.0.0.2:11434"),
            ProviderKind::Ollama
        );
        assert_eq!(
            infer_discovery_kind("local", Some("ollama"), "http://10.0.0.2:11434/v1"),
            ProviderKind::Ollama
        );
        assert_eq!(
            infer_discovery_kind(
                "tinfoil",
                Some("openai-completions"),
                "https://inference.tinfoil.sh/v1/"
            ),
            ProviderKind::OpenaiCompatible
        );
    }

    #[test]
    fn reads_live_discovery_targets_from_models_yml() {
        let root = json!({
            "providers": {
                "tinfoil": {
                    "api": "openai-completions",
                    "baseUrl": "https://inference.tinfoil.sh/v1/",
                    "discovery": { "type": "openai-models-list" }
                },
                "anthropic": { "apiKey": "sk-test" },
                "ollama-lan": {
                    "api": "openai-completions",
                    "baseUrl": "http://10.0.0.2:11434/v1"
                }
            }
        });
        let targets = targets_from_models_yml(&root);
        assert_eq!(targets.len(), 2);
        let tinfoil = targets.iter().find(|t| t.id == "tinfoil").unwrap();
        assert_eq!(tinfoil.kind, ProviderKind::OpenaiCompatible);
        assert!(tinfoil.required);
        assert!(!tinfoil.persist);
        let ollama = targets.iter().find(|t| t.id == "ollama-lan").unwrap();
        assert_eq!(ollama.kind, ProviderKind::Ollama);
    }
}
