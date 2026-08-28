/// Index management: workspace watch, chunking, embedding, storage
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::workspace::WorkspaceIndexer;
use crate::config::Config;

const MAX_FETCH_BYTES: usize = 2 * 1024 * 1024; // 2 MiB

/// Index state: watched dirs, last sync time, chunk count
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexState {
    pub workspace: PathBuf,
    pub chunks: usize,
    pub last_sync: String,
    pub status: String,
}

/// A document stored under `<workspace>/.klepto/index/docs/`
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexedDoc {
    pub path: String,
    pub title: String,
    pub url: Option<String>,
    pub fetched_at: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FetchDocResult {
    pub path: String,
    pub title: String,
    pub url: String,
    pub bytes: usize,
}

#[derive(Clone)]
pub struct IndexManager {
    config: Config,
    state: Arc<Mutex<HashMap<String, IndexState>>>,
    _tx: mpsc::UnboundedSender<String>,
    workspace_indexer: WorkspaceIndexer,
}

impl IndexManager {
    pub fn new(config: Config) -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self {
            config: config.clone(),
            state: Arc::new(Mutex::new(HashMap::new())),
            _tx: tx,
            workspace_indexer: WorkspaceIndexer::new(config),
        }
    }

    pub async fn index_workspace(&self, workspace: &str) -> Result<IndexState, String> {
        ensure_klepto_layout(Path::new(workspace))?;

        // Build a first index or refresh it when the workspace has changed.
        let ws_path = PathBuf::from(workspace);
        let should_index = if self.workspace_indexer.is_indexed(&ws_path) {
            self.workspace_indexer
                .has_changes(&ws_path)
                .await
                .unwrap_or(true)
        } else {
            true
        };
        if should_index {
            match self.workspace_indexer.index_workspace(&ws_path).await {
                Ok(ws_index) => {
                    info!(
                        "workspace code index: {} ({} files, {} bytes)",
                        workspace,
                        ws_index.files.len(),
                        ws_index.total_bytes
                    );
                }
                Err(e) => {
                    warn!("workspace code index failed for {}: {}", workspace, e);
                    // Non-fatal: continue with doc index
                }
            }
        }

        let docs = self.list_docs(workspace).await.unwrap_or_default();
        let state = IndexState {
            workspace: PathBuf::from(workspace),
            chunks: docs.len(),
            last_sync: Utc::now().to_rfc3339(),
            status: "indexed".to_string(),
        };

        let mut states = self.state.lock().unwrap();
        states.insert(workspace.to_string(), state.clone());
        info!("indexed workspace: {}", workspace);
        Ok(state)
    }

    pub async fn search(&self, workspace: &str, query: &str) -> Result<Vec<SearchHit>, String> {
        // Use ripgrep JSON as the deterministic lexical backend.
        let output = tokio::process::Command::new("rg")
            .arg("--json")
            .arg("-F")
            .args(["-g", "!.klepto/**"])
            .arg(query)
            .arg(workspace)
            .output()
            .await;

        match output {
            Ok(result) => {
                let text = String::from_utf8_lossy(&result.stdout);
                let mut hits = Vec::new();
                for line in text.lines() {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                        continue;
                    };
                    if value.get("type").and_then(|v| v.as_str()) != Some("match") {
                        continue;
                    }
                    let data = &value["data"];
                    hits.push(SearchHit {
                        file: data["path"]["text"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        line: data["line_number"].as_u64().unwrap_or(0) as usize,
                        text: data["lines"]["text"]
                            .as_str()
                            .unwrap_or_default()
                            .trim_end()
                            .to_string(),
                        score: 1.0,
                        backend: "ripgrep".into(),
                        provenance: "live workspace lexical match".into(),
                        freshness: Utc::now().to_rfc3339(),
                    });
                }
                hits.extend(symbol_hits(workspace, query));
                hits.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(a.file.cmp(&b.file))
                        .then(a.line.cmp(&b.line))
                });
                hits.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.text == b.text);
                Ok(hits)
            }
            Err(e) => Err(format!("ripgrep search failed for '{query}': {e}")),
        }
    }

    pub async fn list_workspaces(&self) -> Vec<IndexState> {
        let state = self.state.lock().unwrap();
        state.values().cloned().collect()
    }

    pub async fn delete_workspace(&self, workspace: &str) -> Result<(), String> {
        let mut states = self.state.lock().unwrap();
        if states.remove(workspace).is_some() {
            Ok(())
        } else {
            Err(format!("workspace '{}' not indexed", workspace))
        }
    }

    /// Fetch a URL and store it under `<workspace>/.klepto/index/docs/<slug>.md`.
    pub async fn fetch_and_store(
        &self,
        workspace: &str,
        url: &str,
    ) -> Result<FetchDocResult, String> {
        let url = url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("url must start with http:// or https://".into());
        }

        let root = Path::new(workspace);
        ensure_klepto_layout(root)?;

        let effective = crate::profiles::resolve(&self.config, root, None, None)?;
        if effective.network.mode == crate::profiles::NetworkMode::None {
            return Err("network profile denies URL fetching".into());
        }
        let mut client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("Klepto/0.1 (+local index fetch)");
        if effective.network.mode == crate::profiles::NetworkMode::Socks5h {
            let url = effective
                .network
                .proxy_url
                .as_deref()
                .ok_or("socks5h network profile has no proxy_url")?;
            client = client
                .proxy(reqwest::Proxy::all(url).map_err(|e| format!("invalid proxy URL: {e}"))?);
        }
        let client = client.build().map_err(|e| format!("http client: {e}"))?;

        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("fetch failed: {e}"))?;

        if !response.status().is_success() {
            return Err(format!("fetch returned HTTP {}", response.status()));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("read body: {e}"))?;

        if bytes.len() > MAX_FETCH_BYTES {
            return Err(format!(
                "response too large ({} bytes, max {})",
                bytes.len(),
                MAX_FETCH_BYTES
            ));
        }

        let raw = String::from_utf8_lossy(&bytes);
        let (title, body, stored_ct) = if content_type.contains("html") {
            let title = extract_html_title(&raw).unwrap_or_else(|| host_path_label(url));
            let text = html_to_text(&raw);
            (title, text, "text/html".to_string())
        } else if content_type.starts_with("text/")
            || content_type.contains("json")
            || content_type.contains("xml")
            || content_type.contains("markdown")
        {
            let title = host_path_label(url);
            (title, raw.to_string(), content_type.clone())
        } else {
            return Err(format!(
                "unsupported content-type for index docs: {content_type}"
            ));
        };

        let slug = url_slug(url);
        let rel = format!(".klepto/index/docs/{slug}.md");
        let abs = root.join(&rel);
        let fetched_at = Utc::now().to_rfc3339();

        let markdown = format!(
            "---\nurl: {url}\nfetched_at: {fetched_at}\ntitle: {}\ncontent_type: {stored_ct}\n---\n\n# {}\n\n{}\n",
            yaml_escape(&title),
            title,
            body.trim()
        );

        std::fs::write(&abs, markdown).map_err(|e| format!("write doc: {e}"))?;

        // Count as indexed
        let _ = self.index_workspace(workspace).await;

        Ok(FetchDocResult {
            path: abs.to_string_lossy().to_string(),
            title,
            url: url.to_string(),
            bytes: bytes.len(),
        })
    }

    pub async fn list_docs(&self, workspace: &str) -> Result<Vec<IndexedDoc>, String> {
        let docs_dir = Path::new(workspace).join(".klepto/index/docs");
        if !docs_dir.exists() {
            return Ok(vec![]);
        }

        let mut docs = Vec::new();
        let entries = std::fs::read_dir(&docs_dir).map_err(|e| format!("read docs dir: {e}"))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let meta = parse_frontmatter(&content);
            docs.push(IndexedDoc {
                path: path.to_string_lossy().to_string(),
                title: meta.title.unwrap_or_else(|| {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("doc")
                        .to_string()
                }),
                url: meta.url,
                fetched_at: meta.fetched_at,
                content_type: meta.content_type,
            });
        }
        docs.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        Ok(docs)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub score: f64,
    pub backend: String,
    pub provenance: String,
    pub freshness: String,
}

fn symbol_hits(workspace: &str, query: &str) -> Vec<SearchHit> {
    let path = Path::new(workspace).join(".klepto/index/symbols.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(symbols) = serde_json::from_str::<Vec<super::workspace::RepoSymbol>>(&raw) else {
        return Vec::new();
    };
    let query = query.to_ascii_lowercase();
    symbols
        .into_iter()
        .filter_map(|symbol| {
            let name = symbol.name.to_ascii_lowercase();
            if !name.contains(&query) {
                return None;
            }
            let score = if name == query {
                3.0
            } else if name.starts_with(&query) {
                2.5
            } else {
                2.0
            };
            Some(SearchHit {
                file: symbol.path,
                line: symbol.line,
                text: format!("{} {}", symbol.kind, symbol.name),
                score,
                backend: "tree-sitter-symbols".into(),
                provenance: "workspace symbol index".into(),
                freshness: Utc::now().to_rfc3339(),
            })
        })
        .take(50)
        .collect()
}

struct Frontmatter {
    title: Option<String>,
    url: Option<String>,
    fetched_at: Option<String>,
    content_type: Option<String>,
}

fn ensure_klepto_layout(workspace: &Path) -> Result<(), String> {
    crate::artifacts::ensure_workspace(workspace).map(|_| ())
}

fn url_slug(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let hash = hasher.finalize();
    let hex = format!("{hash:x}");
    let label = host_path_label(url)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let label = label.trim_matches('-');
    let label = if label.is_empty() {
        "doc"
    } else {
        &label[..label.len().min(48)]
    };
    format!("{label}-{}", &hex[..10])
}

fn host_path_label(url: &str) -> String {
    if let Ok(u) = reqwest::Url::parse(url) {
        let host = u.host_str().unwrap_or("page");
        let path = u.path().trim_matches('/');
        if path.is_empty() {
            host.to_string()
        } else {
            format!("{host}/{}", path.split('/').next().unwrap_or(""))
        }
    } else {
        "page".into()
    }
}

fn yaml_escape(s: &str) -> String {
    if s.contains(':') || s.contains('#') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\\\"").replace('\n', " "))
    } else {
        s.to_string()
    }
}

fn parse_frontmatter(content: &str) -> Frontmatter {
    let mut meta = Frontmatter {
        title: None,
        url: None,
        fetched_at: None,
        content_type: None,
    };
    if !content.starts_with("---") {
        return meta;
    }
    let rest = &content[3..];
    let Some(end) = rest.find("\n---") else {
        return meta;
    };
    for line in rest[..end].lines() {
        let line = line.trim();
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches('"').to_string();
            match k.trim() {
                "title" => meta.title = Some(v),
                "url" => meta.url = Some(v),
                "fetched_at" => meta.fetched_at = Some(v),
                "content_type" => meta.content_type = Some(v),
                _ => {}
            }
        }
    }
    meta
}

fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after = &html[start..];
    let gt = after.find('>')?;
    let inner = &after[gt + 1..];
    let end = inner.to_ascii_lowercase().find("</title>")?;
    let title = html_entities_basic(inner[..end].trim());
    if title.is_empty() { None } else { Some(title) }
}

fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut tag_buf = String::new();
    let mut skip_depth: i32 = 0;
    let chars: Vec<char> = html.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            in_tag = true;
            tag_buf.clear();
            i += 1;
            continue;
        }
        if c == '>' && in_tag {
            in_tag = false;
            let tag = tag_buf.to_ascii_lowercase();
            let name = tag
                .trim_start_matches('/')
                .split(|ch: char| ch.is_whitespace() || ch == '>')
                .next()
                .unwrap_or("");
            let closing = tag.starts_with('/');
            if matches!(name, "script" | "style" | "noscript") {
                if closing {
                    skip_depth = (skip_depth - 1).max(0);
                } else if !tag.ends_with('/') {
                    skip_depth += 1;
                }
            } else if skip_depth == 0
                && matches!(
                    name,
                    "p" | "div" | "br" | "li" | "h1" | "h2" | "h3" | "tr" | "section" | "article"
                )
            {
                out.push('\n');
            }
            i += 1;
            continue;
        }
        if in_tag {
            tag_buf.push(c);
            i += 1;
            continue;
        }
        if skip_depth > 0 {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    let decoded = html_entities_basic(&out);
    // Collapse blank lines
    let mut lines = Vec::new();
    let mut blank = false;
    for line in decoded.lines() {
        let t = line.trim();
        if t.is_empty() {
            if !blank {
                lines.push(String::new());
                blank = true;
            }
        } else {
            blank = false;
            lines.push(t.to_string());
        }
    }
    lines.join("\n")
}

fn html_entities_basic(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}
