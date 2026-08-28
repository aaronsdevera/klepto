//! Durable, workspace-local artifacts shared by the daemon, CLI, and IDE.

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{Session, SessionEvent};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct WorkspacePaths {
    pub root: PathBuf,
    pub klepto: PathBuf,
    pub sessions: PathBuf,
    pub plans: PathBuf,
    pub index: PathBuf,
    pub artifacts: PathBuf,
}

impl WorkspacePaths {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        let klepto = root.join(".klepto");
        Self {
            sessions: klepto.join("sessions"),
            plans: klepto.join("plans"),
            index: klepto.join("index"),
            artifacts: klepto.join("artifacts"),
            root,
            klepto,
        }
    }

    pub fn ensure(&self) -> Result<(), String> {
        for path in [
            &self.klepto,
            &self.sessions,
            &self.plans,
            &self.index,
            &self.artifacts,
            &self.index.join("docs"),
        ] {
            fs::create_dir_all(path).map_err(|e| format!("create {}: {e}", path.display()))?;
        }
        let gitignore = self.klepto.join(".gitignore");
        const LEGACY_GENERATED_GITIGNORE: &str = "*\n!.gitignore\n";
        const GENERATED_GITIGNORE: &str = "*\n";
        if !gitignore.exists() {
            fs::write(&gitignore, GENERATED_GITIGNORE)
                .map_err(|e| format!("write {}: {e}", gitignore.display()))?;
        } else if fs::read_to_string(&gitignore).ok().as_deref() == Some(LEGACY_GENERATED_GITIGNORE)
        {
            fs::write(&gitignore, GENERATED_GITIGNORE)
                .map_err(|e| format!("migrate {}: {e}", gitignore.display()))?;
        }
        Ok(())
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.sessions.join(id)
    }

    pub fn session_meta(&self, id: &str) -> PathBuf {
        self.session_dir(id).join("meta.json")
    }

    pub fn raw_events(&self, id: &str) -> PathBuf {
        self.session_dir(id).join("raw-events.jsonl")
    }

    pub fn events(&self, id: &str) -> PathBuf {
        self.session_dir(id).join("events.jsonl")
    }

    pub fn capture_offset(&self, id: &str) -> PathBuf {
        self.session_dir(id).join("capture-offset")
    }

    pub fn runner_socket(&self, id: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(self.root.to_string_lossy().as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        std::env::temp_dir().join(format!("klepto-{}-{id}.sock", &hash[..10]))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Versioned<T> {
    pub schema_version: u32,
    pub value: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequencedSessionEvent {
    pub schema_version: u32,
    pub seq: u64,
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub event: SessionEvent,
}

impl SequencedSessionEvent {
    pub fn new(seq: u64, event: SessionEvent) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            seq,
            at: Utc::now(),
            event,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Draft,
    Approved,
    Building,
    Completed,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanTodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl Default for PlanTodoStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanTodo {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub status: PlanTodoStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanAgentRole {
    Author,
    Builder,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanAgentReference {
    pub session_id: String,
    pub role: PlanAgentRole,
    pub label: String,
    #[serde(default)]
    pub todo_ids: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanArtifact {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    pub slug: String,
    pub workspace: String,
    pub path: String,
    pub status: PlanStatus,
    pub revision: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub todos: Vec<PlanTodo>,
    #[serde(default)]
    pub agents: Vec<PlanAgentReference>,
    #[serde(default)]
    pub is_project: bool,
    #[serde(default)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PlanDocument {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default)]
    overview: String,
    #[serde(default)]
    status: Option<PlanStatus>,
    #[serde(default)]
    revision: Option<u32>,
    #[serde(default)]
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    todos: Vec<PlanTodo>,
    #[serde(default)]
    agents: Vec<PlanAgentReference>,
    #[serde(default, rename = "isProject")]
    is_project: bool,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

pub fn ensure_workspace(workspace: impl AsRef<Path>) -> Result<WorkspacePaths, String> {
    let paths = WorkspacePaths::new(workspace);
    paths.ensure()?;
    register_workspace(&paths.root)?;
    Ok(paths)
}

pub fn persist_session(session: &Session) -> Result<(), String> {
    let paths = ensure_workspace(&session.cwd)?;
    let dir = paths.session_dir(&session.id);
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    write_json_atomic(
        &paths.session_meta(&session.id),
        &Versioned {
            schema_version: SCHEMA_VERSION,
            value: session,
        },
    )
}

pub fn load_sessions() -> Vec<Session> {
    let mut out = Vec::new();
    for workspace in registered_workspaces() {
        let sessions = WorkspacePaths::new(&workspace).sessions;
        let Ok(entries) = fs::read_dir(sessions) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path().join("meta.json");
            let Ok(raw) = fs::read_to_string(path) else {
                continue;
            };
            if let Ok(wrapper) = serde_json::from_str::<Versioned<Session>>(&raw) {
                out.push(wrapper.value);
            }
        }
    }
    out
}

pub fn append_event(
    workspace: &str,
    session_id: &str,
    event: &SequencedSessionEvent,
) -> Result<(), String> {
    let paths = ensure_workspace(workspace)?;
    let dir = paths.session_dir(session_id);
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.events(session_id))
        .map_err(|e| format!("open event journal: {e}"))?;
    let line = serde_json::to_string(event).map_err(|e| format!("serialize event: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("append event: {e}"))
}

pub fn read_events(workspace: &str, session_id: &str, after: u64) -> Vec<SequencedSessionEvent> {
    let path = WorkspacePaths::new(workspace).events(session_id);
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str::<SequencedSessionEvent>(line).ok())
        .filter(|event| event.seq > after)
        .collect()
}

pub fn create_plan(workspace: &str, title: &str, content: &str) -> Result<PlanArtifact, String> {
    create_plan_with_author(workspace, title, content, None)
}

pub fn create_plan_with_author(
    workspace: &str,
    title: &str,
    content: &str,
    author_session_id: Option<&str>,
) -> Result<PlanArtifact, String> {
    let paths = ensure_workspace(workspace)?;
    let (incoming, body) =
        parse_document_parts(content).unwrap_or_else(|| (PlanDocument::default(), content));
    let title = incoming
        .name
        .clone()
        .or(incoming.title.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| title.trim().to_string());
    let slug = short_slug(&title);
    let stamp = Local::now().format("%Y%m%d-%H%M").to_string();
    let mut id = format!("{stamp}-{slug}");
    let mut path = paths.plans.join(format!("{id}.md"));
    let mut suffix = 2;
    while path.exists() {
        id = format!("{stamp}-{slug}-{suffix}");
        path = paths.plans.join(format!("{id}.md"));
        suffix += 1;
    }
    let now = Utc::now();
    let todo_ids = incoming.todos.iter().map(|todo| todo.id.clone()).collect();
    let mut agents = incoming.agents.clone();
    if let Some(session_id) = author_session_id.filter(|id| !id.trim().is_empty()) {
        if !agents.iter().any(|agent| agent.session_id == session_id) {
            agents.push(PlanAgentReference {
                session_id: session_id.to_string(),
                role: PlanAgentRole::Author,
                label: "Plan author".into(),
                todo_ids,
                created_at: now,
            });
        }
    }
    let plan = PlanArtifact {
        schema_version: SCHEMA_VERSION,
        id,
        title,
        slug,
        workspace: workspace.to_string(),
        path: path.to_string_lossy().to_string(),
        status: PlanStatus::Draft,
        revision: 1,
        created_at: now,
        updated_at: now,
        overview: incoming.overview,
        todos: incoming.todos,
        agents,
        is_project: incoming.is_project,
        extra: incoming.extra,
        content: body.trim().to_string(),
    };
    write_plan(&plan)?;
    Ok(plan)
}

pub fn load_plan(workspace: &str, id: &str) -> Result<PlanArtifact, String> {
    let path = WorkspacePaths::new(workspace)
        .plans
        .join(format!("{id}.md"));
    let raw = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_plan(&path, workspace, &raw)
}

pub fn list_plans(workspace: &str) -> Result<Vec<PlanArtifact>, String> {
    let paths = ensure_workspace(workspace)?;
    let mut plans = Vec::new();
    for entry in fs::read_dir(paths.plans)
        .map_err(|e| format!("read plans: {e}"))?
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("md") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(plan) = parse_plan(&path, workspace, &raw) {
            plans.push(plan);
        }
    }
    plans.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(plans)
}

pub fn update_plan(
    workspace: &str,
    id: &str,
    content: Option<String>,
    status: Option<PlanStatus>,
) -> Result<PlanArtifact, String> {
    let mut plan = load_plan(workspace, id)?;
    if let Some(content) = content {
        if let Some((incoming, body)) = parse_document_parts(&content) {
            plan.title = incoming
                .name
                .or(incoming.title)
                .unwrap_or_else(|| plan.title.clone());
            plan.overview = incoming.overview;
            plan.todos = incoming.todos;
            plan.agents = incoming.agents;
            plan.is_project = incoming.is_project;
            plan.extra = incoming.extra;
            plan.content = body.trim().to_string();
        } else {
            plan.content = content;
        }
        plan.revision += 1;
    }
    if let Some(status) = status {
        plan.status = status;
    }
    plan.updated_at = Utc::now();
    write_plan(&plan)?;
    Ok(plan)
}

pub fn update_plan_todo(
    workspace: &str,
    id: &str,
    todo_id: &str,
    status: PlanTodoStatus,
) -> Result<PlanArtifact, String> {
    let mut plan = load_plan(workspace, id)?;
    let todo = plan
        .todos
        .iter_mut()
        .find(|todo| todo.id == todo_id)
        .ok_or_else(|| format!("unknown todo '{todo_id}'"))?;
    todo.status = status;
    plan.revision += 1;
    plan.updated_at = Utc::now();
    write_plan(&plan)?;
    Ok(plan)
}

pub fn add_plan_agent(
    workspace: &str,
    id: &str,
    agent: PlanAgentReference,
) -> Result<PlanArtifact, String> {
    let mut plan = load_plan(workspace, id)?;
    plan.agents
        .retain(|entry| entry.session_id != agent.session_id);
    plan.agents.push(agent);
    plan.revision += 1;
    plan.updated_at = Utc::now();
    write_plan(&plan)?;
    Ok(plan)
}

fn write_plan(plan: &PlanArtifact) -> Result<(), String> {
    let document = PlanDocument {
        schema_version: Some(plan.schema_version),
        id: Some(plan.id.clone()),
        name: Some(plan.title.clone()),
        overview: plan.overview.clone(),
        status: Some(plan.status.clone()),
        revision: Some(plan.revision),
        created_at: Some(plan.created_at),
        updated_at: Some(plan.updated_at),
        todos: plan.todos.clone(),
        agents: plan.agents.clone(),
        is_project: plan.is_project,
        extra: plan.extra.clone(),
        ..PlanDocument::default()
    };
    let yaml = serde_yaml::to_string(&document).map_err(|e| format!("serialize plan YAML: {e}"))?;
    let body = format!("---\n{}---\n\n{}\n", yaml, plan.content.trim());
    let path = Path::new(&plan.path);
    let tmp = path.with_extension("md.tmp");
    fs::write(&tmp, body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("replace {}: {e}", path.display()))
}

fn parse_plan(path: &Path, workspace: &str, raw: &str) -> Result<PlanArtifact, String> {
    let (document, content) = parse_document_parts(raw)
        .ok_or_else(|| format!("invalid plan frontmatter: {}", path.display()))?;
    let id = document
        .id
        .or_else(|| {
            path.file_stem()
                .and_then(|v| v.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "plan".into());
    let created_at = document.created_at.unwrap_or_else(Utc::now);
    let updated_at = document.updated_at.unwrap_or(created_at);
    Ok(PlanArtifact {
        schema_version: document.schema_version.unwrap_or(SCHEMA_VERSION),
        slug: id.splitn(3, '-').last().unwrap_or("plan").to_string(),
        id,
        title: document
            .name
            .or(document.title)
            .unwrap_or_else(|| "Plan".into()),
        workspace: workspace.to_string(),
        path: path.to_string_lossy().to_string(),
        status: document.status.unwrap_or(PlanStatus::Draft),
        revision: document.revision.unwrap_or(1),
        created_at,
        updated_at,
        overview: document.overview,
        todos: document.todos,
        agents: document.agents,
        is_project: document.is_project,
        extra: document.extra,
        content: content.trim().to_string(),
    })
}

fn parse_document_parts(raw: &str) -> Option<(PlanDocument, &str)> {
    let normalized = raw.strip_prefix("---\n")?;
    let (frontmatter, content) = normalized.split_once("\n---\n")?;
    let document = serde_yaml::from_str(frontmatter).ok()?;
    Some((document, content))
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("no parent for {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    // Unique temp per writer — a shared `*.tmp` races under parallel tests / processes.
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let tmp = parent.join(format!(
        ".{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        crate::short_id()
    ));
    let body = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize JSON: {e}"))?;
    fs::write(&tmp, &body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(format!("rename {}: {e}", path.display()))
        }
    }
}

fn short_slug(value: &str) -> String {
    let slug = value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let words = slug
        .split('-')
        .filter(|part| !part.is_empty())
        .take(5)
        .collect::<Vec<_>>();
    let joined = words.join("-");
    if joined.is_empty() {
        "plan".into()
    } else {
        joined
    }
}

fn register_workspace(workspace: &Path) -> Result<(), String> {
    // Serialize read-modify-write so parallel plan/session tests (and concurrent
    // daemon calls) do not clobber each other's registry updates.
    static WORKSPACES_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WORKSPACES_LOCK
        .lock()
        .map_err(|_| "workspaces registry lock poisoned".to_string())?;

    let path = crate::config::Config::home_dir().join("workspaces.json");
    let mut workspaces = registered_workspaces();
    let value = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
        .to_string_lossy()
        .to_string();
    if !workspaces.contains(&value) {
        workspaces.push(value);
        workspaces.sort();
        write_json_atomic(&path, &workspaces)?;
    }
    Ok(())
}

pub(crate) fn registered_workspaces() -> Vec<String> {
    let path = crate::config::Config::home_dir().join("workspaces.json");
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_layout_migrates_only_generated_gitignore() {
        let root = std::env::temp_dir().join(format!("klepto-layout-{}", crate::short_id()));
        let paths = WorkspacePaths::new(&root);
        paths.ensure().unwrap();
        let gitignore = paths.klepto.join(".gitignore");
        assert_eq!(fs::read_to_string(&gitignore).unwrap(), "*\n");

        fs::write(&gitignore, "*\n!.gitignore\n").unwrap();
        paths.ensure().unwrap();
        assert_eq!(fs::read_to_string(&gitignore).unwrap(), "*\n");

        fs::write(&gitignore, "sessions/\n# keep plans\n").unwrap();
        paths.ensure().unwrap();
        assert_eq!(
            fs::read_to_string(&gitignore).unwrap(),
            "sessions/\n# keep plans\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plan_names_sort_and_round_trip() {
        let root = std::env::temp_dir().join(format!("klepto-artifacts-{}", crate::short_id()));
        fs::create_dir_all(&root).unwrap();
        let created = create_plan(
            root.to_str().unwrap(),
            "Review authentication API",
            "1. Inspect auth",
        )
        .unwrap();
        assert!(created.id.contains("review-authentication-api"));
        let loaded = load_plan(root.to_str().unwrap(), &created.id).unwrap();
        assert_eq!(loaded.content, "1. Inspect auth");
        assert_eq!(loaded.status, PlanStatus::Draft);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_workspace_registration_does_not_lose_entries() {
        use std::sync::Arc;
        use std::thread;

        let roots: Vec<_> = (0..8)
            .map(|i| {
                let root = std::env::temp_dir().join(format!(
                    "klepto-concurrent-{}-{}",
                    crate::short_id(),
                    i
                ));
                fs::create_dir_all(&root).unwrap();
                root
            })
            .collect();
        let roots = Arc::new(roots);
        let mut handles = Vec::new();
        for i in 0..roots.len() {
            let roots = Arc::clone(&roots);
            handles.push(thread::spawn(move || {
                create_plan(
                    roots[i].to_str().unwrap(),
                    &format!("Concurrent plan {i}"),
                    "body",
                )
                .expect("create_plan under concurrency");
            }));
        }
        for handle in handles {
            handle.join().expect("worker panicked");
        }
        for root in roots.iter() {
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn cursor_plan_frontmatter_round_trips_multiline_todos_and_unknown_fields() {
        let root = std::env::temp_dir().join(format!("klepto-plan-yaml-{}", crate::short_id()));
        fs::create_dir_all(&root).unwrap();
        let content = r#"---
name: Fix chat controls
overview: Keep failures visible.
todos:
  - id: response-errors
    content: |-
      Preserve the first line.
      Preserve the second line.
    status: in_progress
isProject: false
custom:
  owner: platform
---

# Fix chat controls

Implementation details.
"#;
        let created = create_plan_with_author(
            root.to_str().unwrap(),
            "Fallback title",
            content,
            Some("author-session"),
        )
        .unwrap();
        assert_eq!(created.title, "Fix chat controls");
        assert_eq!(created.todos[0].status, PlanTodoStatus::InProgress);
        assert!(created.todos[0].content.contains("second line"));
        assert_eq!(created.agents[0].role, PlanAgentRole::Author);
        assert!(created.extra.contains_key("custom"));

        let updated = update_plan_todo(
            root.to_str().unwrap(),
            &created.id,
            "response-errors",
            PlanTodoStatus::Completed,
        )
        .unwrap();
        assert_eq!(updated.todos[0].status, PlanTodoStatus::Completed);
        let updated = add_plan_agent(
            root.to_str().unwrap(),
            &created.id,
            PlanAgentReference {
                session_id: "builder-session".into(),
                role: PlanAgentRole::Builder,
                label: "Build Fix chat controls".into(),
                todo_ids: vec!["response-errors".into()],
                created_at: Utc::now(),
            },
        )
        .unwrap();
        assert_eq!(updated.agents.len(), 2);
        let raw = fs::read_to_string(&updated.path).unwrap();
        assert!(raw.contains("owner: platform"));
        assert!(raw.contains("Preserve the second line."));
        assert!(raw.contains("# Fix chat controls"));
        assert!(!raw.contains("title: null"));
        let _ = fs::remove_dir_all(root);
    }
}
