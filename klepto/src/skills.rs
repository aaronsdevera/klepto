//! Installation of small, progressively disclosed built-in skills.

use std::fs;
use std::path::PathBuf;

use crate::config::Config;

const CODE_UNDERSTANDING: &str = include_str!("../skills/code-understanding/SKILL.md");

pub fn ensure_builtin_skills() -> Result<(), String> {
    let klepto_path = Config::home_dir()
        .join("skills")
        .join("code-understanding")
        .join("SKILL.md");
    write_if_changed(&klepto_path, CODE_UNDERSTANDING)?;

    // omp owns skill loading; mirror the Klepto-owned skill into its standard
    // discovery path so every subsequent session inherits it.
    let omp_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".omp/agent/skills/klepto-code-understanding/SKILL.md");
    write_if_changed(&omp_path, CODE_UNDERSTANDING)
}

fn write_if_changed(path: &PathBuf, content: &str) -> Result<(), String> {
    if matches!(fs::read_to_string(path), Ok(existing) if existing == content) {
        return Ok(());
    }
    let parent = path.parent().ok_or("skill path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))
}
