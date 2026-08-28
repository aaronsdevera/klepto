//! Declarative task profiles and network policy resolution.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    #[default]
    Host,
    Oci,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    #[default]
    Direct,
    None,
    Socks5h,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkProfile {
    #[serde(default)]
    pub mode: NetworkMode,
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub no_proxy: Vec<String>,
    #[serde(default)]
    pub deny_direct: bool,
}

impl Default for NetworkProfile {
    fn default() -> Self {
        Self {
            mode: NetworkMode::Direct,
            proxy_url: None,
            no_proxy: vec!["127.0.0.1".into(), "localhost".into()],
            deny_direct: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelPreference {
    pub preferred: Option<String>,
    #[serde(default)]
    pub fallbacks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    pub thinking: Option<String>,
    #[serde(default)]
    pub model: ModelPreference,
    #[serde(default)]
    pub runner: RunnerKind,
    #[serde(default = "direct_network_name")]
    pub network: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceConfig {
    pub default_profile: Option<String>,
    pub default_model: Option<String>,
    pub runner: Option<RunnerKind>,
    pub network: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveSessionConfig {
    pub schema_version: u32,
    pub profile: Profile,
    pub model: Option<String>,
    pub runner: RunnerKind,
    pub network_name: String,
    pub network: NetworkProfile,
    pub env: BTreeMap<String, String>,
}

pub fn builtin_profiles() -> BTreeMap<String, Profile> {
    [
        profile(
            "coding",
            "General implementation with verification",
            "Implement the smallest correct change. Inspect the diff and run relevant tests before finishing.",
            &[],
            None,
        ),
        profile(
            "commit",
            "Concise Git commit message generation",
            "Write a concise Git commit message from the supplied diff. Return only the commit message: an imperative subject no longer than 72 characters, followed by an optional short body when it adds useful context.",
            &[],
            Some("low"),
        ),
        profile(
            "review",
            "Code audit and review",
            "Review changes skeptically. Prioritize correctness, regressions, security, and missing tests. Do not edit unless explicitly asked.",
            &["read", "bash", "grep", "find", "ls"],
            Some("high"),
        ),
        profile(
            "research",
            "Read-only code and documentation research",
            "Gather evidence before conclusions. Cite concrete files and distinguish verified facts from inference. Do not modify files.",
            &["read", "bash", "grep", "find", "ls"],
            Some("high"),
        ),
        profile(
            "fact-check",
            "Claim verification and source comparison",
            "Check each material claim against primary evidence. State uncertainty and contradictions explicitly. Do not modify files.",
            &["read", "bash", "grep", "find", "ls"],
            Some("high"),
        ),
        profile(
            "plan",
            "Read-only implementation planning",
            "Explore with read-only tools and ask only material questions. Finish with one actionable Markdown plan using YAML frontmatter with `name`, `overview`, `todos` (stable id, content, pending status), and `isProject: false`, followed by the implementation plan body. Do not modify project files.",
            &["read", "bash", "grep", "find", "ls"],
            Some("high"),
        ),
        profile(
            "debug",
            "Evidence-driven debugging",
            "Reproduce the failure, isolate the root cause, implement the smallest fix, and verify it.",
            &[],
            Some("high"),
        ),
    ]
    .into_iter()
    .map(|p| (p.name.clone(), p))
    .collect()
}

pub fn list_profiles() -> BTreeMap<String, Profile> {
    let mut profiles = builtin_profiles();
    let dir = Config::home_dir().join("profiles");
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("toml") {
                continue;
            }
            if let Ok(raw) = fs::read_to_string(&path) {
                if let Ok(profile) = toml::from_str::<Profile>(&raw) {
                    profiles.insert(profile.name.clone(), profile);
                }
            }
        }
    }
    profiles
}

pub fn load_workspace_config(workspace: &Path) -> WorkspaceConfig {
    let path = workspace.join(".klepto/config.toml");
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn resolve(
    global: &Config,
    workspace: &Path,
    profile_name: Option<&str>,
    model: Option<&str>,
) -> Result<EffectiveSessionConfig, String> {
    let workspace_config = load_workspace_config(workspace);
    let selected = profile_name
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
        .or(workspace_config.default_profile)
        .or_else(|| global.default_profile.clone())
        .unwrap_or_else(|| "coding".into());
    let profiles = list_profiles();
    let profile = profiles
        .get(&selected)
        .cloned()
        .ok_or_else(|| format!("unknown profile '{selected}'"))?;
    let model = model
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
        .or(workspace_config.default_model)
        .or_else(|| profile.model.preferred.clone())
        .or_else(|| global.default_model.clone());
    let runner = workspace_config
        .runner
        .unwrap_or_else(|| profile.runner.clone());
    let network_name = workspace_config
        .network
        .unwrap_or_else(|| profile.network.clone());
    let network = global
        .networks
        .get(&network_name)
        .cloned()
        .ok_or_else(|| format!("unknown network profile '{network_name}'"))?;
    let mut env: BTreeMap<String, String> =
        global.env.clone().unwrap_or_default().into_iter().collect();
    env.extend(workspace_config.env);
    apply_network_env(&network, &mut env)?;
    Ok(EffectiveSessionConfig {
        schema_version: crate::artifacts::SCHEMA_VERSION,
        profile,
        model,
        runner,
        network_name,
        network,
        env,
    })
}

pub fn apply_network_env(
    network: &NetworkProfile,
    env: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    match network.mode {
        NetworkMode::Direct => {}
        NetworkMode::None => {
            env.insert("KLEPTO_NETWORK_DISABLED".into(), "1".into());
        }
        NetworkMode::Socks5h => {
            let proxy = network
                .proxy_url
                .as_deref()
                .ok_or("socks5h network profile requires proxy_url")?;
            if !proxy.starts_with("socks5h://") {
                return Err("SOCKS proxy URL must start with socks5h:// for remote DNS".into());
            }
            env.insert("ALL_PROXY".into(), proxy.into());
            env.insert("all_proxy".into(), proxy.into());
        }
    }
    if !network.no_proxy.is_empty() {
        let value = network.no_proxy.join(",");
        env.insert("NO_PROXY".into(), value.clone());
        env.insert("no_proxy".into(), value);
    }
    Ok(())
}

pub fn ensure_builtin_profile_examples() -> Result<PathBuf, String> {
    let dir = Config::home_dir().join("profiles");
    fs::create_dir_all(&dir).map_err(|e| format!("create profiles dir: {e}"))?;
    let readme = dir.join("README.md");
    if !readme.exists() {
        fs::write(
            &readme,
            "# Klepto profiles\n\nAdd `<name>.toml` files here to override or extend the built-in coding, commit, review, research, fact-check, plan, and debug profiles.\n",
        )
        .map_err(|e| format!("write profile README: {e}"))?;
    }
    Ok(dir)
}

fn profile(
    name: &str,
    description: &str,
    system_prompt: &str,
    tools: &[&str],
    thinking: Option<&str>,
) -> Profile {
    Profile {
        name: name.into(),
        description: description.into(),
        system_prompt: system_prompt.into(),
        skills: Vec::new(),
        tools: tools.iter().map(|v| (*v).into()).collect(),
        thinking: thinking.map(str::to_string),
        model: ModelPreference::default(),
        runner: RunnerKind::Host,
        network: direct_network_name(),
    }
}

fn direct_network_name() -> String {
    "direct".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socks_requires_remote_dns_scheme() {
        let mut env = BTreeMap::new();
        let profile = NetworkProfile {
            mode: NetworkMode::Socks5h,
            proxy_url: Some("socks5://localhost:9050".into()),
            ..NetworkProfile::default()
        };
        assert!(apply_network_env(&profile, &mut env).is_err());
    }
}
