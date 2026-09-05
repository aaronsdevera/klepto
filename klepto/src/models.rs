//! Discover models available to omp (`omp models --json` + live provider APIs).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::process::Command;
use std::time::Duration;

use crate::config::Config;
use crate::deps;
use crate::providers::{DiscoveryTarget, ProviderKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub id: String,
    /// `provider/id` — preferred value for `--model`
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub models: Vec<ModelInfo>,
    pub providers: Vec<String>,
    /// true when we fell back to a curated suggestion list
    pub suggested: bool,
    pub message: Option<String>,
}

/// Curated defaults when omp has no authenticated models yet.
fn suggested_models() -> Vec<ModelInfo> {
    const ENTRIES: &[(&str, &str)] = &[
        ("anthropic", "claude-sonnet-4-20250514"),
        ("anthropic", "claude-opus-4-20250514"),
        ("anthropic", "claude-haiku-4-20250414"),
        ("openai", "gpt-4o"),
        ("openai", "gpt-4o-mini"),
        ("openai", "o3"),
        ("openai", "o4-mini"),
        ("google", "gemini-2.5-pro"),
        ("google", "gemini-2.5-flash"),
        ("openrouter", "anthropic/claude-sonnet-4"),
        ("ollama", "llama3.1:8b"),
        ("ollama", "qwen2.5-coder:7b"),
    ];
    ENTRIES
        .iter()
        .map(|(provider, id)| ModelInfo {
            label: format!("{provider}/{id}"),
            provider: (*provider).to_string(),
            id: (*id).to_string(),
        })
        .collect()
}

fn from_models_yml() -> Vec<ModelInfo> {
    crate::providers::from_models_yml()
        .into_iter()
        .map(|(provider, id)| ModelInfo {
            label: format!("{provider}/{id}"),
            provider,
            id,
        })
        .collect()
}

fn parse_omp_models_json(raw: &str) -> Result<Vec<ModelInfo>, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("parse omp models --json: {e}"))?;
    let models = value
        .get("models")
        .and_then(|m| m.as_array())
        .ok_or_else(|| "omp models --json missing models array".to_string())?;
    let mut out = Vec::new();
    for model in models {
        let provider = model
            .get("provider")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        let id = model
            .get("id")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        if provider.is_empty() || id.is_empty() {
            continue;
        }
        let label = model
            .get("selector")
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{provider}/{id}"));
        out.push(ModelInfo {
            provider,
            id,
            label,
        });
    }
    Ok(out)
}

pub fn omp_models_args(refresh: bool) -> &'static [&'static str] {
    if refresh {
        &["models", "refresh", "--json"]
    } else {
        &["models", "--json"]
    }
}

pub fn refresh_requested(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

async fn run_omp_list_models(config: &Config, refresh: bool) -> Result<Vec<ModelInfo>, String> {
    let omp = deps::resolve_bin(&config.omp_bin).ok_or_else(|| {
        format!(
            "omp binary '{}' not found — run `klepto doctor --install`",
            config.omp_bin
        )
    })?;
    let args: Vec<String> = omp_models_args(refresh)
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
    let output = tokio::task::spawn_blocking(move || Command::new(&omp).args(&args).output())
        .await
        .map_err(|e| format!("join omp models: {e}"))?
        .map_err(|e| format!("failed to run omp models --json: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "omp models --json failed (status {}): {stderr}",
            output.status
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_omp_models_json(&stdout)
}

fn finalize(
    mut models: Vec<ModelInfo>,
    suggested: bool,
    message: Option<String>,
) -> ModelsResponse {
    // Dedupe by label, stable order
    let mut seen = BTreeMap::new();
    for m in models.drain(..) {
        seen.entry(m.label.clone()).or_insert(m);
    }
    let models: Vec<ModelInfo> = seen.into_values().collect();
    let mut providers: Vec<String> = models.iter().map(|m| m.provider.clone()).collect();
    providers.sort();
    providers.dedup();
    ModelsResponse {
        models,
        providers,
        suggested,
        message,
    }
}

fn replace_provider_models(models: &mut Vec<ModelInfo>, provider: &str, ids: &[String]) {
    models.retain(|model| model.provider != provider);
    models.extend(ids.iter().cloned().map(|id| ModelInfo {
        label: format!("{provider}/{id}"),
        provider: provider.to_string(),
        id,
    }));
}

fn discovery_timeout(base_url: &str) -> Duration {
    if crate::providers::is_loopback_url(base_url) {
        Duration::from_millis(800)
    } else {
        Duration::from_secs(5)
    }
}

pub async fn list_models(config: &Config) -> ModelsResponse {
    list_models_with_refresh(config, false).await
}

pub async fn list_models_with_refresh(config: &Config, refresh: bool) -> ModelsResponse {
    let mut models = Vec::new();
    let mut messages = Vec::new();

    match run_omp_list_models(config, refresh).await {
        Ok(listed) => models.extend(listed),
        Err(e) => messages.push(e),
    }

    models.extend(from_models_yml());

    let catalog = crate::providers::load_catalog();
    let mut targets = crate::providers::discovery_targets();
    if models.iter().any(|model| model.provider == "ollama")
        && !targets.iter().any(|target| target.id == "ollama")
    {
        targets.push(DiscoveryTarget {
            id: "ollama".into(),
            base_url: "http://127.0.0.1:11434".into(),
            kind: ProviderKind::Ollama,
            persist: false,
            required: false,
        });
    }

    let discoveries = futures::future::join_all(targets.into_iter().map(|target| async move {
        let timeout = discovery_timeout(&target.base_url);
        let result = match target.kind {
            ProviderKind::OpenaiCompatible => {
                discover_openai_models(&target.id, &target.base_url, timeout).await
            }
            ProviderKind::Ollama => {
                discover_ollama_models_timed(&target.id, &target.base_url, timeout).await
            }
        };
        (target, result)
    }))
    .await;

    for (target, result) in discoveries {
        match result {
            Ok(discovered) => {
                replace_provider_models(&mut models, &target.id, &discovered);
                if target.persist {
                    if let Some(definition) = catalog.providers.get(&target.id) {
                        if definition.models != discovered {
                            let mut refreshed = definition.clone();
                            refreshed.models = discovered;
                            if let Err(error) = crate::providers::upsert(refreshed, None) {
                                messages.push(format!("{}: {error}", target.id));
                            }
                        }
                    }
                }
            }
            Err(error) => {
                if target.required {
                    messages.push(format!("{}: {error}", target.id));
                }
                if let Some(definition) = catalog.providers.get(&target.id) {
                    if !definition.models.is_empty()
                        && !models.iter().any(|model| model.provider == target.id)
                    {
                        replace_provider_models(&mut models, &target.id, &definition.models);
                    }
                }
            }
        }
    }

    if models.is_empty() {
        return finalize(
            suggested_models(),
            true,
            Some(
                (!messages.is_empty()).then(|| messages.join("; ")).unwrap_or_else(|| {
                    "No authenticated omp models yet — showing common suggestions. Configure providers in Klepto or run `omp` `/login`.".into()
                }),
            ),
        );
    }

    finalize(
        models,
        false,
        (!messages.is_empty()).then(|| messages.join("; ")),
    )
}

async fn discover_ollama_models(provider: &str, base_url: &str) -> Result<Vec<String>, String> {
    discover_ollama_models_timed(provider, base_url, discovery_timeout(base_url)).await
}

async fn discover_ollama_models_timed(
    provider: &str,
    base_url: &str,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let url = format!("{}/api/tags", crate::providers::ollama_root_url(base_url));
    let mut request = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| format!("build model discovery client: {error}"))?
        .get(&url);
    if let Some(key) = crate::providers::api_key(provider) {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("fetch {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("fetch {url}: HTTP {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("parse {url}: {error}"))?;
    parse_ollama_models(&value).ok_or_else(|| format!("parse {url}: missing models[].name"))
}

async fn discover_openai_models(
    provider: &str,
    base_url: &str,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut request = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| format!("build model discovery client: {error}"))?
        .get(&url);
    if let Some(key) = crate::providers::api_key(provider) {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("fetch {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("fetch {url}: HTTP {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("parse {url}: {error}"))?;
    parse_openai_models(&value).ok_or_else(|| format!("parse {url}: missing data[].id"))
}

fn parse_ollama_models(value: &serde_json::Value) -> Option<Vec<String>> {
    let models: BTreeSet<String> = value
        .get("models")?
        .as_array()?
        .iter()
        .filter_map(|model| {
            model
                .get("name")
                .or_else(|| model.get("model"))
                .and_then(|name| name.as_str())
        })
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    (!models.is_empty()).then(|| models.into_iter().collect())
}

fn parse_openai_models(value: &serde_json::Value) -> Option<Vec<String>> {
    let models: BTreeSet<String> = value
        .get("data")?
        .as_array()?
        .iter()
        .filter_map(|model| model.get("id").and_then(|id| id.as_str()))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    (!models.is_empty()).then(|| models.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::{
        ModelInfo, discover_ollama_models, omp_models_args, parse_ollama_models,
        parse_omp_models_json, parse_openai_models, refresh_requested, replace_provider_models,
    };

    #[test]
    fn parses_openai_compatible_model_list() {
        let value = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "ornith-35b-fp8", "object": "model" },
                { "id": "another-model", "object": "model" }
            ]
        });
        assert_eq!(
            parse_openai_models(&value),
            Some(vec!["another-model".into(), "ornith-35b-fp8".into()])
        );
    }

    #[test]
    fn parses_ollama_tags_model_list() {
        let value = serde_json::json!({
            "models": [
                { "name": "qwen3-coder:30b", "model": "qwen3-coder:30b" },
                { "name": "gemma3:latest" }
            ]
        });
        assert_eq!(
            parse_ollama_models(&value),
            Some(vec!["gemma3:latest".into(), "qwen3-coder:30b".into()])
        );
    }

    #[test]
    fn parses_omp_models_json_payload() {
        let raw = r#"{
          "models": [
            {"provider":"cursor","id":"claude-4-sonnet","selector":"cursor/claude-4-sonnet"},
            {"provider":"ollama","id":"qwen3:8b"}
          ]
        }"#;
        let models = parse_omp_models_json(raw).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].label, "cursor/claude-4-sonnet");
        assert_eq!(models[1].label, "ollama/qwen3:8b");
    }

    #[test]
    fn refresh_flag_selects_omp_refresh_command() {
        assert_eq!(omp_models_args(false), &["models", "--json"]);
        assert_eq!(omp_models_args(true), &["models", "refresh", "--json"]);
        assert!(refresh_requested(Some("true")));
        assert!(refresh_requested(Some("1")));
        assert!(!refresh_requested(Some("false")));
        assert!(!refresh_requested(None));
    }

    #[test]
    fn live_discovery_replaces_stale_provider_rows() {
        let mut models = vec![
            ModelInfo {
                provider: "litellm".into(),
                id: "old".into(),
                label: "litellm/old".into(),
            },
            ModelInfo {
                provider: "openrouter".into(),
                id: "keep".into(),
                label: "openrouter/keep".into(),
            },
        ];
        replace_provider_models(
            &mut models,
            "litellm",
            &["[B] Smol".into(), "[B] BigBang V1".into()],
        );
        let litellm: Vec<_> = models
            .iter()
            .filter(|model| model.provider == "litellm")
            .map(|model| model.id.as_str())
            .collect();
        assert_eq!(litellm, vec!["[B] Smol", "[B] BigBang V1"]);
        assert!(models.iter().any(|model| model.id == "keep"));
    }

    #[tokio::test]
    async fn discovers_models_from_ollama_tags_endpoint() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 2048];
            let read = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("GET /api/tags "));
            let body = r#"{"models":[{"name":"qwen3:8b"},{"name":"gemma3:latest"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let models =
            discover_ollama_models("ollama-test-without-auth", &format!("http://{address}/v1"))
                .await
                .unwrap();
        assert_eq!(models, vec!["gemma3:latest", "qwen3:8b"]);
        server.await.unwrap();
    }
}
