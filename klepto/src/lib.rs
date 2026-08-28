//! Shared library for the Klepto daemon and CLI.

pub mod artifacts;
pub mod client;
pub mod config;
pub mod daemon;
pub mod deps;
pub mod index;
pub mod memory;
pub mod models;
pub mod profiles;
pub mod providers;
pub mod runner;
pub mod search;
pub mod service;
pub mod session;
pub mod skills;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Chat spawn profile (Cursor-like modes). Maps to omp args — not separate runtimes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    #[default]
    Agent,
    Plan,
    Debug,
}

impl AgentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Plan => "plan",
            Self::Debug => "debug",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "plan" => Self::Plan,
            "debug" => Self::Debug,
            _ => Self::Agent,
        }
    }
}

impl fmt::Display for AgentMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Session status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionStatus {
    Running,
    Waiting,
    Exited,
    Killed,
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionStatus::Running => write!(f, "Running"),
            SessionStatus::Waiting => write!(f, "Waiting"),
            SessionStatus::Exited => write!(f, "Exited"),
            SessionStatus::Killed => write!(f, "Killed"),
        }
    }
}

/// Session metadata stored in ~/.klepto/sessions/
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub tmux_name: String,
    pub cwd: String,
    pub created_at: DateTime<Utc>,
    pub status: SessionStatus,
    /// omp transport mode (rpc / json / text)
    #[serde(default = "default_omp_mode", alias = "pi_mode")]
    pub omp_mode: String,
    /// Agent / Plan / Debug spawn profile
    #[serde(default)]
    pub agent_mode: AgentMode,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub runner: Option<String>,
    pub network: Option<String>,
    pub pi_args: Option<Vec<String>>,
    pub parent_id: Option<String>,
    pub worker_role: Option<String>,
}

/// Health response
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub tmux_available: bool,
    pub omp_available: bool,
    pub omp_bin: String,
    pub uptime_seconds: u64,
}

/// Create session request
#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub cwd: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub agent_mode: Option<AgentMode>,
    pub pi_args: Option<Vec<String>>,
}

/// Prompt request
#[derive(Debug, Deserialize)]
pub struct PromptRequest {
    pub message: String,
    pub context: Option<PromptContext>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MentionKind {
    File,
    Doc,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MentionRef {
    pub kind: MentionKind,
    pub path: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AttachmentRef {
    pub path: String,
    pub mime: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UrlRef {
    pub url: String,
    pub doc_path: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PromptContext {
    pub workspace_root: Option<String>,
    pub active_file: Option<String>,
    pub selection: Option<String>,
    pub open_tabs: Option<Vec<String>>,
    pub mentions: Option<Vec<MentionRef>>,
    pub attachments: Option<Vec<AttachmentRef>>,
    pub urls: Option<Vec<UrlRef>>,
}

/// Expand editor/composer context into a preamble so omp receives paths to read.
pub fn expand_prompt_message(message: &str, context: Option<&PromptContext>) -> String {
    let Some(ctx) = context else {
        return message.to_string();
    };

    let mut parts: Vec<String> = Vec::new();

    if let Some(root) = ctx.workspace_root.as_deref() {
        parts.push(format!("Workspace: {root}"));
        let repo_map = std::path::Path::new(root).join(".klepto/index/repo-map.md");
        if repo_map.exists() {
            parts.push(format!(
                "Repository map (read on demand; verify against source): {}",
                repo_map.display()
            ));
        }
    }
    if let Some(file) = ctx.active_file.as_deref() {
        parts.push(format!("Active file: {file}"));
    }
    if let Some(sel) = ctx.selection.as_deref() {
        if !sel.is_empty() {
            let clipped = if sel.len() > 4000 {
                format!("{}…", &sel[..4000])
            } else {
                sel.to_string()
            };
            parts.push(format!("Current selection:\n```\n{clipped}\n```"));
        }
    }
    if let Some(tabs) = ctx.open_tabs.as_ref() {
        if !tabs.is_empty() {
            parts.push(format!("Open tabs:\n- {}", tabs.join("\n- ")));
        }
    }
    if let Some(mentions) = ctx.mentions.as_ref() {
        if !mentions.is_empty() {
            let lines: Vec<String> = mentions
                .iter()
                .map(|m| {
                    let kind = match m.kind {
                        MentionKind::File => "file",
                        MentionKind::Doc => "doc",
                    };
                    let label = m.label.as_deref().unwrap_or("");
                    if label.is_empty() {
                        format!("- ({kind}) {}", m.path)
                    } else {
                        format!("- ({kind}) {label}: {}", m.path)
                    }
                })
                .collect();
            parts.push(format!(
                "Mentioned context (read these with your tools):\n{}",
                lines.join("\n")
            ));
        }
    }
    if let Some(atts) = ctx.attachments.as_ref() {
        if !atts.is_empty() {
            let lines: Vec<String> = atts
                .iter()
                .map(|a| {
                    let name = a.name.as_deref().unwrap_or("");
                    if name.is_empty() {
                        format!("- {}", a.path)
                    } else {
                        format!("- {name}: {}", a.path)
                    }
                })
                .collect();
            parts.push(format!(
                "Attached files (read these with your tools):\n{}",
                lines.join("\n")
            ));
        }
    }
    if let Some(urls) = ctx.urls.as_ref() {
        if !urls.is_empty() {
            let lines: Vec<String> = urls
                .iter()
                .map(|u| {
                    if let Some(path) = u.doc_path.as_deref() {
                        let title = u.title.as_deref().unwrap_or(u.url.as_str());
                        format!("- {title} ({}) → indexed at {path}", u.url)
                    } else {
                        format!("- {} (not yet indexed; fetch with curl if needed)", u.url)
                    }
                })
                .collect();
            parts.push(format!(
                "URLs (prefer reading the indexed doc path when present):\n{}",
                lines.join("\n")
            ));
        }
    }

    if parts.is_empty() {
        return message.to_string();
    }

    format!(
        "[Context]\n{}\n\n[User message]\n{}",
        parts.join("\n\n"),
        message
    )
}

/// Session event types for WebSocket streaming
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        tool: String,
        exit_code: i32,
        output: String,
    },
    Status {
        status: String,
    },
    Terminal {
        data: String,
    },
    Error {
        message: String,
    },
    Cost {
        tokens: i64,
        cost_usd: Option<f64>,
    },
}

impl Session {
    pub fn new(cwd: &str) -> Self {
        let id = Uuid::new_v4().to_string()[..8].to_string();
        Self {
            schema_version: artifacts::SCHEMA_VERSION,
            id: id.clone(),
            tmux_name: format!("klepto-{}", id),
            cwd: cwd.to_string(),
            created_at: Utc::now(),
            status: SessionStatus::Waiting,
            omp_mode: "rpc".to_string(),
            agent_mode: AgentMode::Agent,
            provider: None,
            model: None,
            profile: None,
            runner: None,
            network: None,
            pi_args: None,
            parent_id: None,
            worker_role: None,
        }
    }
}

/// Generate a short ID for memory entries
pub fn short_id() -> String {
    Uuid::new_v4().to_string()[..8].to_string()
}

fn schema_version() -> u32 {
    artifacts::SCHEMA_VERSION
}

fn default_omp_mode() -> String {
    "rpc".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_prompt_includes_mentions_and_urls() {
        let ctx = PromptContext {
            workspace_root: Some("/tmp/ws".into()),
            active_file: Some("/tmp/ws/src/main.rs".into()),
            selection: None,
            open_tabs: None,
            mentions: Some(vec![MentionRef {
                kind: MentionKind::File,
                path: "/tmp/ws/src/lib.rs".into(),
                label: Some("lib.rs".into()),
            }]),
            attachments: None,
            urls: Some(vec![UrlRef {
                url: "https://example.com".into(),
                doc_path: Some("/tmp/ws/.klepto/index/docs/ex.md".into()),
                title: Some("Example".into()),
            }]),
        };
        let out = expand_prompt_message("hello", Some(&ctx));
        assert!(out.contains("Workspace: /tmp/ws"));
        assert!(out.contains("lib.rs"));
        assert!(out.contains(".klepto/index/docs/ex.md"));
        assert!(out.contains("[User message]\nhello"));
    }

    #[test]
    fn expand_prompt_passthrough_without_context() {
        assert_eq!(expand_prompt_message("hi", None), "hi");
    }
}
