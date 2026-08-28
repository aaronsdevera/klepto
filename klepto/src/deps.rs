//! Runtime dependency detection and auto-install (tmux, omp, ripgrep).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use tracing::{info, warn};

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct DepStatus {
    pub name: &'static str,
    pub path: Option<PathBuf>,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub struct DepsReport {
    pub tmux: DepStatus,
    pub omp: DepStatus,
    pub rg: DepStatus,
}

impl DepsReport {
    pub fn all_required_ok(&self) -> bool {
        self.tmux.path.is_some() && self.omp.path.is_some()
    }

    pub fn print(&self) {
        for dep in [&self.tmux, &self.omp, &self.rg] {
            match &dep.path {
                Some(p) => println!("  ✓ {:<6} {}", dep.name, p.display()),
                None if dep.required => println!("  ✗ {:<6} missing (required)", dep.name),
                None => println!("  · {:<6} missing (optional)", dep.name),
            }
        }
    }
}

/// Resolve a binary on PATH plus common Homebrew / nvm / cargo locations.
pub fn resolve_bin(name: &str) -> Option<PathBuf> {
    if let Ok(path) = which::which(name) {
        return Some(path);
    }

    let mut dirs: Vec<PathBuf> = [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();

    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));

        let nvm_root = home.join(".local/share/nvm");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            for entry in entries.flatten() {
                let bin = entry.path().join("bin");
                if bin.is_dir() {
                    dirs.push(bin);
                }
            }
        }

        let classic = home.join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(&classic) {
            for entry in entries.flatten() {
                let bin = entry.path().join("bin");
                if bin.is_dir() {
                    dirs.push(bin);
                }
            }
        }
    }

    for dir in dirs {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn check(config: &Config) -> DepsReport {
    DepsReport {
        tmux: DepStatus {
            name: "tmux",
            path: resolve_bin("tmux"),
            required: true,
        },
        omp: DepStatus {
            name: "omp",
            path: resolve_bin(&config.omp_bin),
            required: true,
        },
        rg: DepStatus {
            name: "rg",
            path: resolve_bin("rg"),
            required: false,
        },
    }
}

/// Check deps and install anything missing. Returns the post-install report.
pub async fn ensure(config: &Config) -> Result<DepsReport, String> {
    let mut report = check(config);

    if report.tmux.path.is_none() {
        info!("tmux not found — attempting install");
        install_tmux().await?;
        report.tmux.path = resolve_bin("tmux");
        if report.tmux.path.is_none() {
            return Err("tmux is still missing after install attempt".into());
        }
        info!(
            "tmux installed at {}",
            report.tmux.path.as_ref().unwrap().display()
        );
    }

    if report.omp.path.is_none() {
        info!("omp not found — attempting install");
        install_omp().await?;
        report.omp.path = resolve_bin(&config.omp_bin);
        if report.omp.path.is_none() {
            return Err(format!(
                "omp ('{}') is still missing after install attempt",
                config.omp_bin
            ));
        }
        info!(
            "omp installed at {}",
            report.omp.path.as_ref().unwrap().display()
        );
    }

    if report.rg.path.is_none() {
        info!("rg (ripgrep) not found — attempting install");
        match install_rg().await {
            Ok(()) => {
                report.rg.path = resolve_bin("rg");
                if let Some(p) = &report.rg.path {
                    info!("rg installed at {}", p.display());
                } else {
                    warn!("rg install finished but binary not found on PATH");
                }
            }
            Err(e) => warn!("optional ripgrep install failed: {e}"),
        }
    }

    Ok(report)
}

async fn install_tmux() -> Result<(), String> {
    if let Some(brew) = resolve_bin("brew") {
        return run_cmd(&brew, &["install", "tmux"], "brew install tmux").await;
    }
    if resolve_bin("apt-get").is_some() {
        return run_shell(
            "sudo apt-get update -y && sudo apt-get install -y tmux",
            "apt-get install tmux",
        )
        .await;
    }
    if resolve_bin("dnf").is_some() {
        return run_shell("sudo dnf install -y tmux", "dnf install tmux").await;
    }
    if resolve_bin("pacman").is_some() {
        return run_shell("sudo pacman -S --noconfirm tmux", "pacman install tmux").await;
    }
    Err("no supported package manager found for tmux (tried brew, apt, dnf, pacman)".into())
}

async fn install_rg() -> Result<(), String> {
    if let Some(brew) = resolve_bin("brew") {
        return run_cmd(&brew, &["install", "ripgrep"], "brew install ripgrep").await;
    }
    if resolve_bin("apt-get").is_some() {
        return run_shell(
            "sudo apt-get update -y && sudo apt-get install -y ripgrep",
            "apt-get install ripgrep",
        )
        .await;
    }
    if resolve_bin("dnf").is_some() {
        return run_shell("sudo dnf install -y ripgrep", "dnf install ripgrep").await;
    }
    if resolve_bin("pacman").is_some() {
        return run_shell(
            "sudo pacman -S --noconfirm ripgrep",
            "pacman install ripgrep",
        )
        .await;
    }
    // cargo fallback when toolchain is present
    if let Some(cargo) = resolve_bin("cargo") {
        return run_cmd(&cargo, &["install", "ripgrep"], "cargo install ripgrep").await;
    }
    Err("no supported installer found for ripgrep".into())
}

async fn install_omp() -> Result<(), String> {
    if let Some(brew) = resolve_bin("brew") {
        match run_cmd(
            &brew,
            &["install", "can1357/tap/omp"],
            "brew install can1357/tap/omp",
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) => warn!("brew omp install failed, falling back to curl installer: {error}"),
        }
    }
    run_shell(
        "curl -fsSL https://omp.sh/install | sh",
        "omp.sh install",
    )
    .await
}

async fn run_cmd(bin: &Path, args: &[&str], label: &str) -> Result<(), String> {
    info!("running: {} {}", bin.display(), args.join(" "));
    let output = tokio::process::Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("failed to spawn {label}: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(format!(
            "{label} failed (status {}):\n{stdout}{stderr}",
            output.status
        ))
    }
}

async fn run_shell(script: &str, label: &str) -> Result<(), String> {
    info!("running: {script}");
    let output = tokio::process::Command::new("sh")
        .args(["-lc", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("failed to spawn {label}: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(format!(
            "{label} failed (status {}):\n{stdout}{stderr}",
            output.status
        ))
    }
}
