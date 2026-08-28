use std::collections::HashMap;
/// Memory store: notes, remember-recall, file-backed under ~/.klepto/memory/
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::Config;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub created_at: String,
    pub workspace: Option<String>,
}

#[derive(Clone)]
pub struct MemoryManager {
    _config: Config,
    entries: Arc<Mutex<HashMap<String, MemoryEntry>>>,
    _tx: mpsc::UnboundedSender<String>,
}

impl MemoryManager {
    pub fn new(config: Config) -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self {
            _config: config,
            entries: Arc::new(Mutex::new(HashMap::new())),
            _tx: tx,
        }
    }

    fn memory_dir(&self) -> PathBuf {
        Config::data_dir().join("memory")
    }

    fn memory_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.memory_dir()];
        dirs.extend(
            crate::artifacts::registered_workspaces()
                .into_iter()
                .map(|workspace| PathBuf::from(workspace).join(".klepto/memory")),
        );
        dirs
    }

    pub fn ensure_memory_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(self.memory_dir())
            .map_err(|e| format!("failed to create memory dir: {}", e))
    }

    pub async fn remember(
        &self,
        content: &str,
        workspace: Option<&str>,
    ) -> Result<MemoryEntry, String> {
        self.ensure_memory_dir().ok();
        let id = crate::short_id();
        let entry = MemoryEntry {
            id: id.clone(),
            content: content.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            workspace: workspace.map(String::from),
        };

        // Persist to disk
        let dir = workspace
            .map(|workspace| PathBuf::from(workspace).join(".klepto/memory"))
            .unwrap_or_else(|| self.memory_dir());
        std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create memory dir: {e}"))?;
        let path = dir.join(format!("{}.json", id));
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&entry)
                .map_err(|e| format!("failed to serialize memory entry: {}", e))?,
        )
        .map_err(|e| format!("failed to write memory entry: {}", e))?;

        // Also keep in-memory
        let mut entries = self.entries.lock().unwrap();
        entries.insert(id.clone(), entry.clone());

        info!("remembered memory entry: {}", id);
        Ok(entry)
    }

    pub async fn recall(&self, query: &str) -> Result<Vec<MemoryEntry>, String> {
        self.recall_scoped(query, None).await
    }

    pub async fn recall_scoped(
        &self,
        query: &str,
        workspace: Option<&str>,
    ) -> Result<Vec<MemoryEntry>, String> {
        // Simple text match across all memory entries
        let mut results = Vec::new();
        let entries = self.entries.lock().unwrap();
        for entry in entries.values() {
            if entry.content.to_lowercase().contains(&query.to_lowercase())
                && workspace_matches(entry, workspace)
            {
                results.push(entry.clone());
            }
        }
        // Also check disk for any missing entries
        for dir in self.memory_dirs().into_iter().filter(|dir| dir.exists()) {
            let files =
                std::fs::read_dir(&dir).map_err(|e| format!("failed to read memory dir: {e}"))?;
            for f in files.flatten() {
                if f.path().extension().and_then(|e| e.to_str()) == Some("json") {
                    let content = std::fs::read_to_string(f.path()).ok();
                    if let Some(c) = content {
                        match serde_json::from_str::<MemoryEntry>(&c) {
                            Ok(entry) => {
                                if entry.content.to_lowercase().contains(&query.to_lowercase())
                                    && workspace_matches(&entry, workspace)
                                    && !results.iter().any(|r| r.id == entry.id)
                                {
                                    results.push(entry);
                                }
                            }
                            Err(e) => warn!("failed to parse memory entry: {}", e),
                        }
                    }
                }
            }
        }

        Ok(results)
    }

    pub async fn list(&self) -> Result<Vec<MemoryEntry>, String> {
        let mut results: Vec<MemoryEntry> =
            self.entries.lock().unwrap().values().cloned().collect();

        // Also load from disk
        for dir in self.memory_dirs().into_iter().filter(|dir| dir.exists()) {
            let files =
                std::fs::read_dir(&dir).map_err(|e| format!("failed to read memory dir: {e}"))?;
            for f in files.flatten() {
                if f.path().extension().and_then(|e| e.to_str()) == Some("json") {
                    let content = std::fs::read_to_string(f.path()).ok();
                    if let Some(c) = content {
                        match serde_json::from_str::<MemoryEntry>(&c) {
                            Ok(entry) => {
                                if !results.iter().any(|r| r.id == entry.id) {
                                    results.push(entry);
                                }
                            }
                            Err(e) => warn!("failed to parse memory entry: {}", e),
                        }
                    }
                }
            }
        }

        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(results)
    }

    pub async fn forget(&self, id: &str) -> Result<(), String> {
        let mut entries = self.entries.lock().unwrap();
        let in_memory = entries.remove(id).is_some();
        let path = self
            .memory_dirs()
            .into_iter()
            .map(|dir| dir.join(format!("{id}.json")))
            .find(|path| path.exists());
        if in_memory || path.is_some() {
            if let Some(path) = path {
                std::fs::remove_file(&path).map_err(|e| format!("remove memory entry: {e}"))?;
            }
            Ok(())
        } else {
            Err(format!("memory entry {} not found", id))
        }
    }
}

fn workspace_matches(entry: &MemoryEntry, workspace: Option<&str>) -> bool {
    match workspace {
        None => true,
        Some(workspace) => entry.workspace.as_deref() == Some(workspace),
    }
}
