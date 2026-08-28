/// Workspace indexer: walks a codebase, respects .gitignore, generates a structure.md
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::info;

use crate::config::Config;

/// Max size of a single file to include in the workspace index (1 MiB)
const MAX_FILE_SIZE: u64 = 1024 * 1024;

/// Max total files to index per workspace
const MAX_FILES: usize = 2000;

/// File tracked by the indexer
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TrackedFile {
    pub path: String,
    pub size: u64,
    pub checksum: String,
    pub category: FileCategory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoSymbol {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexManifest {
    pub schema_version: u32,
    pub generated_at: String,
    pub files: usize,
    pub symbols: usize,
    pub backends: Vec<String>,
}

/// Broad category of a source file — useful for agent context
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum FileCategory {
    /// Application source code
    Source,
    /// Configuration files (yml, json, toml, env, etc.)
    Config,
    /// Documentation
    Docs,
    /// Tests
    Tests,
    /// Build / infrastructure (Dockerfile, Makefile, CI, etc.)
    Infra,
    /// Asset (images, fonts, etc.)
    Asset,
    /// Other / unknown
    Other,
}

impl std::fmt::Display for FileCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source => write!(f, "source"),
            Self::Config => write!(f, "config"),
            Self::Docs => write!(f, "docs"),
            Self::Tests => write!(f, "tests"),
            Self::Infra => write!(f, "infra"),
            Self::Asset => write!(f, "asset"),
            Self::Other => write!(f, "other"),
        }
    }
}

impl FileCategory {
    pub fn from_path(path: &str) -> Self {
        let lower = path.to_lowercase();
        let stem = path.rsplit('/').next().unwrap_or(path);
        let stem_no_ext = stem.split('.').next().unwrap_or(stem);
        let stem_lower = stem_no_ext.to_lowercase();
        let ext = stem.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
        let ext = ext.to_lowercase();

        // Test files by directory
        if lower.contains("/test/")
            || lower.contains("/tests/")
            || lower.contains("/spec/")
            || lower.starts_with("test/")
            || lower.starts_with("tests/")
            || lower.starts_with("spec/")
        {
            return FileCategory::Tests;
        }
        // Test files by stem suffix
        if stem_lower.ends_with("_test")
            || stem_lower.ends_with("_spec")
            || stem_lower.ends_with("test")
        {
            return FileCategory::Tests;
        }

        if ext == "md" || ext == "rst" || ext == "txt" || lower.ends_with("/readme") {
            return FileCategory::Docs;
        }

        if ext == "yml"
            || ext == "yaml"
            || ext == "json"
            || ext == "toml"
            || ext == "env"
            || ext == "ini"
            || ext == "cfg"
            || ext == "conf"
            || ext == "properties"
            || ext == "xml"
        {
            return FileCategory::Config;
        }

        if lower.contains("docker")
            || lower.contains("makefile")
            || lower.contains("git")
            || lower.contains("ci")
            || lower.contains("github")
            || lower.contains(".git")
            || lower.ends_with(".dockerignore")
            || lower.ends_with("flake.nix")
            || lower.ends_with("flake.lock")
            || lower.ends_with("shell.nix")
        {
            return FileCategory::Infra;
        }

        if matches!(
            ext.as_str(),
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "svg"
                | "webp"
                | "ico"
                | "woff"
                | "woff2"
                | "ttf"
                | "otf"
                | "eot"
        ) {
            return FileCategory::Asset;
        }

        // Source code by extension
        let source_exts = [
            "rs", "py", "js", "ts", "jsx", "tsx", "go", "java", "rb", "php", "c", "h", "cpp", "cc",
            "cxx", "cs", "swift", "kt", "kts", "scala", "lua", "r", "m", "mm", "sh", "bash", "zsh",
            "ps1", "dart", "ex", "exs", "erl", "clj", "cljs", "hs", "ml", "fs", "ml", "v", "zig",
            "svelte", "vue", "astro", "qmd", "ipynb",
        ];
        if source_exts.contains(&ext.as_str()) {
            return FileCategory::Source;
        }

        FileCategory::Other
    }
}

/// Workspace index: directory listing, file checksums, structure document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceIndex {
    pub workspace: PathBuf,
    pub files: Vec<TrackedFile>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub last_indexed: String,
    pub status: WorkspaceIndexStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkspaceIndexStatus {
    /// Files have been enumerated and structure.md written
    Ready,
    /// Partial index (hit file/size limits)
    Partial,
    /// Indexing failed
    Failed { reason: String },
}

/// Manages workspace-level code indexing
#[derive(Clone)]
pub struct WorkspaceIndexer {
    _config: Config,
    /// Indexed workspaces: workspace path → WorkspaceIndex
    indexes: Arc<Mutex<HashSet<String>>>,
}

impl WorkspaceIndexer {
    pub fn new(config: Config) -> Self {
        Self {
            _config: config,
            indexes: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Index a workspace: walk the tree, compute checksums, write structure.md
    pub async fn index_workspace(&self, workspace_path: &Path) -> Result<WorkspaceIndex, String> {
        let workspace_str = workspace_path.to_string_lossy().to_string();

        info!("indexing workspace: {}", workspace_str);

        // Walk the workspace respecting .gitignore
        let walker = WalkBuilder::new(workspace_path)
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .follow_links(false)
            .build();

        let mut files = Vec::new();
        let mut skipped = 0u64;

        for entry in walker {
            if files.len() >= MAX_FILES {
                skipped += 1;
                break;
            }

            let entry = entry.map_err(|e| format!("walk error: {e}"))?;
            let path = entry.path();

            // Skip directories, symlinks, and zero-byte files
            let metadata = path.metadata().map_err(|e| format!("stat {path:?}: {e}"))?;
            if !metadata.is_file() {
                continue;
            }

            let file_size = metadata.len();
            if file_size == 0 {
                continue;
            }

            // Skip large files
            if file_size > MAX_FILE_SIZE {
                skipped += 1;
                continue;
            }

            // Skip binary files by checking for null bytes in the first 512 bytes
            if Self::is_binary(path)? {
                skipped += 1;
                continue;
            }

            // Compute checksum
            let checksum = Self::file_checksum(path)?;

            let relative = path
                .strip_prefix(workspace_path)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            if relative == ".klepto" || relative.starts_with(".klepto/") {
                continue;
            }

            let category = FileCategory::from_path(&relative);

            files.push(TrackedFile {
                path: relative,
                size: file_size,
                checksum,
                category,
            });
        }

        // Sort by category then path for stable output
        files.sort_by(|a, b| {
            a.category
                .to_string()
                .cmp(&b.category.to_string())
                .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
        });

        let total_bytes: u64 = files.iter().map(|f| f.size).sum();
        let total_files_count = files.len();
        let status = if skipped > 0 {
            WorkspaceIndexStatus::Partial
        } else {
            WorkspaceIndexStatus::Ready
        };

        // Write the workspace index files
        let symbols = Self::extract_symbols(workspace_path, &files);
        Self::write_index_files(workspace_path, &files, &symbols)?;

        let index = WorkspaceIndex {
            workspace: workspace_path.to_path_buf(),
            files,
            total_files: skipped as usize + total_files_count,
            total_bytes,
            last_indexed: Utc::now().to_rfc3339(),
            status: status.clone(),
        };

        if let Ok(mut locked) = self.indexes.lock() {
            let ws_str = workspace_str.clone();
            locked.insert(ws_str);
        }

        info!(
            "indexed workspace: {} ({} files, {} skipped, {} bytes)",
            workspace_str, total_files_count, skipped, total_bytes
        );

        Ok(index)
    }

    /// Check if a workspace has been indexed
    pub fn is_indexed(&self, workspace_path: &Path) -> bool {
        let workspace_str = workspace_path.to_string_lossy().to_string();
        let locked = self.indexes.lock().unwrap();
        locked.contains(&workspace_str)
            || workspace_path.join(".klepto/index/manifest.json").exists()
            || workspace_path
                .join(".klepto/index/workspace/checksums.json")
                .exists()
    }

    /// Get the list of indexed workspaces
    pub fn list_workspaces(&self) -> Vec<String> {
        let locked = self.indexes.lock().unwrap();
        locked.iter().cloned().collect()
    }

    /// Write index metadata to disk under .klepto/index/workspace/
    fn write_index_files(
        workspace_path: &Path,
        files: &[TrackedFile],
        symbols: &[RepoSymbol],
    ) -> Result<(), String> {
        let workspace_root = workspace_path;
        let klepto_dir = workspace_root.join(".klepto");
        let index_dir = klepto_dir.join("index");
        let workspace_index_dir = index_dir.join("workspace");

        std::fs::create_dir_all(&workspace_index_dir)
            .map_err(|e| format!("create index dir: {e}"))?;

        // Write structure.md — the file the agent reads for repo understanding
        let structure_path = workspace_index_dir.join("structure.md");
        let structure = Self::generate_structure_md(files, workspace_path);
        std::fs::write(&structure_path, &structure)
            .map_err(|e| format!("write structure.md: {e}"))?;

        // Write file checksums JSON for change detection
        let checksums_path = workspace_index_dir.join("checksums.json");
        let checksums_json =
            serde_json::to_string_pretty(files).map_err(|e| format!("serialize checksums: {e}"))?;
        std::fs::write(&checksums_path, &checksums_json)
            .map_err(|e| format!("write checksums.json: {e}"))?;

        let symbols_path = index_dir.join("symbols.json");
        std::fs::write(
            &symbols_path,
            serde_json::to_string_pretty(symbols).map_err(|e| format!("serialize symbols: {e}"))?,
        )
        .map_err(|e| format!("write symbols.json: {e}"))?;

        let repo_map_path = index_dir.join("repo-map.md");
        std::fs::write(&repo_map_path, Self::generate_repo_map(files, symbols))
            .map_err(|e| format!("write repo-map.md: {e}"))?;

        let manifest = IndexManifest {
            schema_version: crate::artifacts::SCHEMA_VERSION,
            generated_at: Utc::now().to_rfc3339(),
            files: files.len(),
            symbols: symbols.len(),
            backends: vec!["tree-sitter".into(), "ripgrep".into()],
        };
        std::fs::write(
            index_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest)
                .map_err(|e| format!("serialize index manifest: {e}"))?,
        )
        .map_err(|e| format!("write index manifest: {e}"))?;

        Ok(())
    }

    /// Generate the structure.md document that gives pi context about the repo
    fn generate_structure_md(files: &[TrackedFile], workspace_path: &Path) -> String {
        let mut output = String::new();

        output.push_str("# Workspace Structure\n\n");
        output.push_str(&format!(
            "Auto-generated on {}\n\n",
            chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
        ));
        output.push_str(&format!(
            "Total files: {} ({} skipped)\n\n",
            files.len(),
            files.iter().filter(|f| f.path.contains("..")).count()
        ));

        // Group by category
        let categories = [
            FileCategory::Source,
            FileCategory::Config,
            FileCategory::Docs,
            FileCategory::Tests,
            FileCategory::Infra,
            FileCategory::Asset,
            FileCategory::Other,
        ];

        for category in &categories {
            let category_files: Vec<&TrackedFile> =
                files.iter().filter(|f| &f.category == category).collect();

            if category_files.is_empty() {
                continue;
            }

            let label = match category {
                FileCategory::Source => "Source Code",
                FileCategory::Config => "Configuration",
                FileCategory::Docs => "Documentation",
                FileCategory::Tests => "Tests",
                FileCategory::Infra => "Infrastructure & Build",
                FileCategory::Asset => "Assets",
                FileCategory::Other => "Other",
            };

            output.push_str(&format!("## {}\n\n", label));
            output.push_str("| Path | Size | Category |\n");
            output.push_str("|------|------|----------|\n");

            for file in &category_files {
                output.push_str(&format!(
                    "| `{}` | {} B | {:?} |\n",
                    file.path, file.size, file.category
                ));
            }
            output.push_str("\n");
        }

        // Add the full file tree as raw data for grep/read operations
        output.push_str("## Full File List\n\n");
        for file in files {
            output.push_str(&format!("- `{}`\n", file.path));
        }
        output.push_str("\n");

        // Important root files hint
        let root_files: Vec<&str> = vec![
            "Cargo.toml",
            "package.json",
            "requirements.txt",
            "go.mod",
            "pyproject.toml",
            "Makefile",
            "Dockerfile",
            "docker-compose.yml",
            "README.md",
            "LICENSE",
            "flake.nix",
            "shell.nix",
            ".github/workflows",
        ];

        let found_root: Vec<&str> = files
            .iter()
            .filter(|f| root_files.contains(&f.path.as_str()))
            .map(|f| f.path.as_str())
            .collect();

        if !found_root.is_empty() {
            output.push_str("## Key Root Files\n\n");
            for path in &found_root {
                output.push_str(&format!("- `{}`\n", path));
            }
            output.push_str("\n");
        }

        format!(
            "{}\n\n---\nworkspace_root: {}\n",
            output,
            workspace_path.display()
        )
    }

    fn generate_repo_map(files: &[TrackedFile], symbols: &[RepoSymbol]) -> String {
        let mut output = format!(
            "# Repository Map\n\nGenerated: {}\n\nFiles: {} · Symbols: {}\n\n",
            Utc::now().to_rfc3339(),
            files.len(),
            symbols.len()
        );
        let mut by_path: std::collections::BTreeMap<&str, Vec<&RepoSymbol>> =
            std::collections::BTreeMap::new();
        for symbol in symbols.iter().take(400) {
            by_path.entry(&symbol.path).or_default().push(symbol);
        }
        for (path, path_symbols) in by_path {
            output.push_str(&format!("## `{path}`\n"));
            for symbol in path_symbols {
                output.push_str(&format!(
                    "- {} `{}` (line {})\n",
                    symbol.kind, symbol.name, symbol.line
                ));
            }
            output.push('\n');
        }
        output.push_str(
            "---\nThis is a token-budgeted orientation artifact. Confirm definitions and references with lexical or language-server tools before editing.\n",
        );
        output
    }

    fn extract_symbols(workspace_path: &Path, files: &[TrackedFile]) -> Vec<RepoSymbol> {
        let mut symbols = Vec::new();
        for file in files
            .iter()
            .filter(|file| file.category == FileCategory::Source)
        {
            if symbols.len() >= 2_000 {
                break;
            }
            let path = workspace_path.join(&file.path);
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let extension = path.extension().and_then(|v| v.to_str()).unwrap_or("");
            let language: Option<tree_sitter::Language> = match extension {
                "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
                "py" => Some(tree_sitter_python::LANGUAGE.into()),
                "ts" | "tsx" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
                "js" | "jsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
                _ => None,
            };
            let Some(language) = language else {
                continue;
            };
            let mut parser = tree_sitter::Parser::new();
            if parser.set_language(&language).is_err() {
                continue;
            }
            let Some(tree) = parser.parse(&source, None) else {
                continue;
            };
            let mut stack = vec![tree.root_node()];
            while let Some(node) = stack.pop() {
                if let Some(kind) = symbol_kind(node.kind()) {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                            symbols.push(RepoSymbol {
                                name: name.to_string(),
                                kind: kind.into(),
                                path: file.path.clone(),
                                line: name_node.start_position().row + 1,
                            });
                        }
                    }
                }
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    stack.push(child);
                }
            }
        }
        symbols.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        symbols
    }

    /// Check if a file is binary by looking for null bytes in the first 512 bytes
    fn is_binary(path: &Path) -> Result<bool, String> {
        let mut file =
            std::fs::File::open(path).map_err(|e| format!("open file for binary check: {e}"))?;

        use std::io::Read;
        let mut buffer = [0u8; 512];
        let n = file
            .read(&mut buffer)
            .map_err(|e| format!("read file for binary check: {e}"))?;

        if n == 0 {
            return Ok(true); // empty files treated as binary (skip)
        }

        // If we find a null byte in the first 512 bytes, it's binary
        Ok(buffer[..n].contains(&0u8))
    }

    /// Compute SHA-256 checksum of a file
    fn file_checksum(path: &Path) -> Result<String, String> {
        let mut file =
            std::fs::File::open(path).map_err(|e| format!("open file for checksum: {e}"))?;

        use std::io::Read;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];

        loop {
            let bytes_read = file
                .read(&mut buffer)
                .map_err(|e| format!("read file for checksum: {e}"))?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Check for changed files since last index by comparing checksums.json on disk
    pub async fn has_changes(&self, workspace_path: &Path) -> Result<bool, String> {
        let index_dir = workspace_path
            .join(".klepto")
            .join("index")
            .join("workspace")
            .join("checksums.json");

        if !index_dir.exists() {
            return Ok(true); // no index yet, needs full index
        }

        let existing: Vec<TrackedFile> = serde_json::from_str(
            &std::fs::read_to_string(&index_dir).map_err(|e| format!("read checksums: {e}"))?,
        )
        .map_err(|e| format!("parse checksums: {e}"))?;

        if existing.is_empty() {
            return Ok(true);
        }

        // Quick check: walk and compare checksums without building full index
        let walker = WalkBuilder::new(workspace_path)
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .follow_links(false)
            .build();

        let mut compared = 0usize;
        for entry in walker {
            if compared >= MAX_FILES {
                break;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            let metadata = match path.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };

            let file_size = metadata.len();
            if file_size == 0 || file_size > MAX_FILE_SIZE {
                continue;
            }

            if Self::is_binary(path)? {
                continue;
            }

            let relative = path
                .strip_prefix(workspace_path)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            if relative == ".klepto" || relative.starts_with(".klepto/") {
                continue;
            }
            compared += 1;

            let existing_file = existing.iter().find(|f| f.path == relative);

            match existing_file {
                Some(old) => {
                    let current_checksum = Self::file_checksum(path)?;
                    if old.checksum != current_checksum {
                        return Ok(true);
                    }
                }
                None => return Ok(true), // file added
            }
        }

        // Check for removed files
        for old_file in &existing {
            let full_path = workspace_path.join(&old_file.path);
            if !full_path.exists() {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Re-index only changed files (lightweight)
    pub async fn reindex_changed(&self, workspace_path: &Path) -> Result<WorkspaceIndex, String> {
        // Just re-index the whole workspace — the checksum file enables fast change detection
        // Full re-index is fine; change detection avoids triggering on every keystroke
        self.index_workspace(workspace_path).await
    }
}

fn symbol_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_item" | "function_definition" | "function_declaration" => Some("function"),
        "method_definition" => Some("method"),
        "struct_item" | "class_definition" | "class_declaration" => Some("type"),
        "enum_item" | "interface_declaration" | "type_alias_declaration" => Some("type"),
        "trait_item" => Some("trait"),
        "impl_item" => Some("implementation"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_category_source() {
        assert_eq!(FileCategory::from_path("src/main.rs"), FileCategory::Source);
        assert_eq!(
            FileCategory::from_path("lib/handler.py"),
            FileCategory::Source
        );
        assert_eq!(FileCategory::from_path("go/main.go"), FileCategory::Source);
    }

    #[test]
    fn test_file_category_tests() {
        assert_eq!(
            FileCategory::from_path("tests/test_foo.rs"),
            FileCategory::Tests
        );
        assert_eq!(
            FileCategory::from_path("src/foo_test.rs"),
            FileCategory::Tests
        );
        assert_eq!(
            FileCategory::from_path("src/spec/bar_spec.js"),
            FileCategory::Tests
        );
    }

    #[test]
    fn test_file_category_docs() {
        assert_eq!(FileCategory::from_path("docs/api.md"), FileCategory::Docs);
        assert_eq!(FileCategory::from_path("README.md"), FileCategory::Docs);
    }

    #[test]
    fn test_file_category_config() {
        assert_eq!(FileCategory::from_path("config.yml"), FileCategory::Config);
        assert_eq!(FileCategory::from_path(".env"), FileCategory::Config);
    }

    #[test]
    fn test_file_category_infra() {
        assert_eq!(FileCategory::from_path("Dockerfile"), FileCategory::Infra);
        assert_eq!(FileCategory::from_path("Makefile"), FileCategory::Infra);
        assert_eq!(FileCategory::from_path("flake.nix"), FileCategory::Infra);
    }

    #[test]
    fn test_file_category_asset() {
        assert_eq!(
            FileCategory::from_path("assets/logo.png"),
            FileCategory::Asset
        );
        assert_eq!(
            FileCategory::from_path("fonts/font.woff2"),
            FileCategory::Asset
        );
    }

    #[test]
    fn test_workspace_structure_generation() {
        let files = vec![
            TrackedFile {
                path: "src/main.rs".into(),
                size: 1024,
                checksum: "abc".into(),
                category: FileCategory::Source,
            },
            TrackedFile {
                path: "README.md".into(),
                size: 2048,
                checksum: "def".into(),
                category: FileCategory::Docs,
            },
            TrackedFile {
                path: "Cargo.toml".into(),
                size: 512,
                checksum: "ghi".into(),
                category: FileCategory::Config,
            },
        ];

        let structure = WorkspaceIndexer::generate_structure_md(&files, Path::new("/tmp/test"));
        assert!(structure.contains("Source Code"));
        assert!(structure.contains("Documentation"));
        assert!(structure.contains("Configuration"));
        assert!(structure.contains("src/main.rs"));
        assert!(structure.contains("Key Root Files"));
        assert!(structure.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn index_writes_tree_sitter_repo_map_and_manifest() {
        let root = std::env::temp_dir().join(format!("klepto-index-{}", crate::short_id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub struct Widget;\npub fn build_widget() -> Widget { Widget }\n",
        )
        .unwrap();
        let index = WorkspaceIndexer::new(Config::default());
        index.index_workspace(&root).await.unwrap();
        let map = std::fs::read_to_string(root.join(".klepto/index/repo-map.md")).unwrap();
        assert!(map.contains("build_widget"));
        assert!(root.join(".klepto/index/manifest.json").exists());
        assert!(index.is_indexed(&root));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn change_detection_refreshes_source_but_ignores_klepto_artifacts() {
        let root = std::env::temp_dir().join(format!("klepto-refresh-{}", crate::short_id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();
        crate::artifacts::WorkspacePaths::new(&root)
            .ensure()
            .unwrap();

        let index = WorkspaceIndexer::new(Config::default());
        index.index_workspace(&root).await.unwrap();
        assert!(!index.has_changes(&root).await.unwrap());
        assert_eq!(
            std::fs::read_to_string(root.join(".gitignore")).unwrap(),
            "target/\n"
        );

        std::fs::write(root.join(".klepto/artifacts/transient"), "ignored").unwrap();
        assert!(!index.has_changes(&root).await.unwrap());

        std::fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 2 }\n").unwrap();
        assert!(index.has_changes(&root).await.unwrap());
        index.index_workspace(&root).await.unwrap();
        assert!(!index.has_changes(&root).await.unwrap());

        std::fs::write(root.join("src/new.rs"), "pub fn added() {}\n").unwrap();
        assert!(index.has_changes(&root).await.unwrap());
        let _ = std::fs::remove_dir_all(root);
    }
}
