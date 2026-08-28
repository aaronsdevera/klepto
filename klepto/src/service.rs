//! User-level daemonization: launchd (macOS) / systemd --user (Linux).

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;

const LAUNCHD_LABEL: &str = "com.klepto.daemon";
const SYSTEMD_UNIT: &str = "klepto";

#[derive(Debug, Clone, Copy)]
pub enum Platform {
    Launchd,
    SystemdUser,
}

impl Platform {
    pub fn detect() -> Result<Self, String> {
        if cfg!(target_os = "macos") {
            Ok(Self::Launchd)
        } else if cfg!(target_os = "linux") {
            Ok(Self::SystemdUser)
        } else {
            Err(
                "klepto service is only supported on macOS (launchd) and Linux (systemd --user)"
                    .into(),
            )
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::SystemdUser => "systemd --user",
        }
    }
}

fn data_dir() -> PathBuf {
    Config::data_dir()
}

fn bin_dir() -> PathBuf {
    data_dir().join("bin")
}

fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

fn installed_bin() -> PathBuf {
    bin_dir().join("klepto")
}

fn launchd_plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

fn systemd_unit_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("systemd/user")
        .join(format!("{SYSTEMD_UNIT}.service"))
}

fn resolve_source_bin() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    // Prefer a real file (cargo run may be a symlink into target/)
    let canonical = exe.canonicalize().unwrap_or(exe);
    if canonical.is_file() && !is_empty_file(&canonical) {
        return Ok(canonical);
    }
    which::which("klepto")
        .ok()
        .and_then(|p| p.canonicalize().ok().or(Some(p)))
        .filter(|p| p.is_file() && !is_empty_file(p))
        .ok_or_else(|| {
            "could not locate a non-empty klepto binary — build/install it first (e.g. make release && ./dist/klepto service install)".into()
        })
}

fn is_empty_file(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.len() == 0).unwrap_or(true)
}

fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if let (Ok(ca), Ok(cb)) = (a.canonicalize(), b.canonicalize()) {
        if ca == cb {
            return true;
        }
    }
    // Inode compare catches hard links / path spelling differences canonicalize misses.
    if let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            return ma.dev() == mb.dev() && ma.ino() == mb.ino();
        }
        #[cfg(not(unix))]
        {
            let _ = (ma, mb);
        }
    }
    false
}

fn ensure_dirs() -> Result<(), String> {
    fs::create_dir_all(bin_dir()).map_err(|e| format!("create bin dir: {e}"))?;
    fs::create_dir_all(log_dir()).map_err(|e| format!("create log dir: {e}"))?;
    Ok(())
}

fn set_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(|e| format!("stat {}: {e}", path.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).map_err(|e| format!("chmod: {e}"))?;
    }
    let _ = path;
    Ok(())
}

fn copy_bin() -> Result<PathBuf, String> {
    ensure_dirs()?;
    let src = resolve_source_bin()?;
    let dst = installed_bin();

    // Copying a running binary onto itself truncates it to 0 bytes on Unix
    // (open(dst, CREATE|TRUNCATE) before reading src). Skip when already installed.
    if same_file(&src, &dst) {
        if is_empty_file(&dst) {
            return Err(format!(
                "installed binary {} is empty (0 bytes) — restore it from a build artifact (e.g. cp dist/klepto {}) then re-run service install",
                dst.display(),
                dst.display()
            ));
        }
        set_executable(&dst)?;
        return Ok(dst);
    }

    if is_empty_file(&src) {
        return Err(format!(
            "refusing to install empty binary {}",
            src.display()
        ));
    }

    // Atomic replace so a crash mid-copy cannot leave a truncated destination.
    let tmp = bin_dir().join(format!(".klepto.{}.tmp", std::process::id()));
    fs::copy(&src, &tmp).map_err(|e| format!("copy {} → {}: {e}", src.display(), tmp.display()))?;
    set_executable(&tmp)?;
    fs::rename(&tmp, &dst).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("install {} → {}: {e}", tmp.display(), dst.display())
    })?;
    Ok(dst)
}

fn run(cmd: &str, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run {cmd}: {e}"))
}

fn run_ok(cmd: &str, args: &[&str], label: &str) -> Result<(), String> {
    let out = run(cmd, args)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn launchd_plist(bin: &Path, listen: &str) -> String {
    let stdout = log_dir().join("klepto.out.log");
    let stderr = log_dir().join("klepto.err.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>serve</string>
    <string>--listen</string>
    <string>{listen}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
</dict>
</plist>
"#,
        bin.display(),
        stdout.display(),
        stderr.display()
    )
}

fn systemd_unit(bin: &Path, listen: &str) -> String {
    format!(
        r#"[Unit]
Description=Klepto local agent daemon
After=network.target

[Service]
Type=simple
ExecStart={} serve --listen {}
Restart=on-failure
RestartSec=2
Environment=PATH=/home/linuxbrew/.linuxbrew/bin:/usr/local/bin:/usr/bin:/bin
StandardOutput=append:{}
StandardError=append:{}

[Install]
WantedBy=default.target
"#,
        bin.display(),
        listen,
        log_dir().join("klepto.out.log").display(),
        log_dir().join("klepto.err.log").display()
    )
}

pub fn install(listen: &str) -> Result<(), String> {
    let platform = Platform::detect()?;
    let bin = copy_bin()?;
    ensure_dirs()?;

    match platform {
        Platform::Launchd => {
            let plist = launchd_plist_path();
            if let Some(parent) = plist.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("create LaunchAgents: {e}"))?;
            }
            // Unload existing if present
            let _ = run(
                "launchctl",
                &[
                    "bootout",
                    &format!("gui/{}", uid()),
                    &plist.to_string_lossy(),
                ],
            );
            fs::write(&plist, launchd_plist(&bin, listen))
                .map_err(|e| format!("write plist: {e}"))?;
            run_ok(
                "launchctl",
                &[
                    "bootstrap",
                    &format!("gui/{}", uid()),
                    &plist.to_string_lossy(),
                ],
                "launchctl bootstrap",
            )?;
            run_ok(
                "launchctl",
                &["enable", &format!("gui/{}/{}", uid(), LAUNCHD_LABEL)],
                "launchctl enable",
            )?;
            let _ = run(
                "launchctl",
                &[
                    "kickstart",
                    "-k",
                    &format!("gui/{}/{}", uid(), LAUNCHD_LABEL),
                ],
            );
            println!("Installed launchd agent: {}", plist.display());
            println!("Binary: {}", bin.display());
            println!("Listen: {listen}");
            Ok(())
        }
        Platform::SystemdUser => {
            let unit = systemd_unit_path();
            if let Some(parent) = unit.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("create systemd user dir: {e}"))?;
            }
            fs::write(&unit, systemd_unit(&bin, listen)).map_err(|e| format!("write unit: {e}"))?;
            run_ok("systemctl", &["--user", "daemon-reload"], "daemon-reload")?;
            run_ok(
                "systemctl",
                &["--user", "enable", "--now", SYSTEMD_UNIT],
                "systemctl enable --now",
            )?;
            println!("Installed systemd user unit: {}", unit.display());
            println!("Binary: {}", bin.display());
            println!("Listen: {listen}");
            Ok(())
        }
    }
}

pub fn uninstall() -> Result<(), String> {
    let platform = Platform::detect()?;
    match platform {
        Platform::Launchd => {
            let plist = launchd_plist_path();
            let _ = run(
                "launchctl",
                &[
                    "bootout",
                    &format!("gui/{}", uid()),
                    &plist.to_string_lossy(),
                ],
            );
            if plist.exists() {
                fs::remove_file(&plist).map_err(|e| format!("remove plist: {e}"))?;
            }
            println!("Uninstalled launchd agent {LAUNCHD_LABEL}");
            Ok(())
        }
        Platform::SystemdUser => {
            let _ = run("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]);
            let unit = systemd_unit_path();
            if unit.exists() {
                fs::remove_file(&unit).map_err(|e| format!("remove unit: {e}"))?;
            }
            let _ = run("systemctl", &["--user", "daemon-reload"]);
            println!("Uninstalled systemd user unit {SYSTEMD_UNIT}");
            Ok(())
        }
    }
}

pub fn start() -> Result<(), String> {
    match Platform::detect()? {
        Platform::Launchd => {
            let plist = launchd_plist_path();
            if !plist.exists() {
                return Err("service not installed — run `klepto service install` first".into());
            }
            // Prefer kickstart; fall back to bootstrap if needed
            let target = format!("gui/{}/{}", uid(), LAUNCHD_LABEL);
            if run("launchctl", &["kickstart", "-k", &target])?
                .status
                .success()
            {
                println!("Started {LAUNCHD_LABEL}");
                return Ok(());
            }
            run_ok(
                "launchctl",
                &[
                    "bootstrap",
                    &format!("gui/{}", uid()),
                    &plist.to_string_lossy(),
                ],
                "launchctl bootstrap",
            )?;
            println!("Started {LAUNCHD_LABEL}");
            Ok(())
        }
        Platform::SystemdUser => {
            run_ok(
                "systemctl",
                &["--user", "start", SYSTEMD_UNIT],
                "systemctl start",
            )?;
            println!("Started {SYSTEMD_UNIT}");
            Ok(())
        }
    }
}

pub fn stop() -> Result<(), String> {
    match Platform::detect()? {
        Platform::Launchd => {
            let target = format!("gui/{}/{}", uid(), LAUNCHD_LABEL);
            let out = run("launchctl", &["kill", "SIGTERM", &target])?;
            if !out.status.success() {
                // Older launchctl: bootout without removing plist
                let plist = launchd_plist_path();
                let _ = run(
                    "launchctl",
                    &[
                        "bootout",
                        &format!("gui/{}", uid()),
                        &plist.to_string_lossy(),
                    ],
                );
            }
            println!("Stopped {LAUNCHD_LABEL}");
            Ok(())
        }
        Platform::SystemdUser => {
            run_ok(
                "systemctl",
                &["--user", "stop", SYSTEMD_UNIT],
                "systemctl stop",
            )?;
            println!("Stopped {SYSTEMD_UNIT}");
            Ok(())
        }
    }
}

pub fn restart() -> Result<(), String> {
    match Platform::detect()? {
        // bootout+bootstrap reloads the job so launchd accepts a replaced binary
        // (kill+kickstart keeps the old code-signature cache and can fail with
        // OS_REASON_CODESIGNING after `install.sh` overwrites ~/.klepto/bin/klepto).
        Platform::Launchd => {
            let plist = launchd_plist_path();
            if !plist.exists() {
                return Err("service not installed — run `klepto service install` first".into());
            }
            let domain = format!("gui/{}", uid());
            let _ = run("launchctl", &["bootout", &domain, &plist.to_string_lossy()]);
            run_ok(
                "launchctl",
                &["bootstrap", &domain, &plist.to_string_lossy()],
                "launchctl bootstrap",
            )?;
            let _ = run(
                "launchctl",
                &[
                    "kickstart",
                    "-k",
                    &format!("{domain}/{LAUNCHD_LABEL}"),
                ],
            );
            println!("Restarted {LAUNCHD_LABEL}");
            Ok(())
        }
        Platform::SystemdUser => {
            let _ = stop();
            start()
        }
    }
}

pub fn status() -> Result<(), String> {
    let platform = Platform::detect()?;
    println!("Platform: {}", platform.name());
    println!("Binary:   {}", installed_bin().display());
    println!(
        "Installed: {}",
        match platform {
            Platform::Launchd => launchd_plist_path().exists(),
            Platform::SystemdUser => systemd_unit_path().exists(),
        }
    );

    match platform {
        Platform::Launchd => {
            let target = format!("gui/{}/{}", uid(), LAUNCHD_LABEL);
            let out = run("launchctl", &["print", &target])?;
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                // Print a short summary
                for line in text.lines() {
                    let t = line.trim();
                    if t.starts_with("state =")
                        || t.starts_with("pid =")
                        || t.starts_with("path =")
                        || t.starts_with("runs =")
                    {
                        println!("{t}");
                    }
                }
            } else {
                println!("state = not loaded");
            }
        }
        Platform::SystemdUser => {
            let out = run(
                "systemctl",
                &["--user", "status", SYSTEMD_UNIT, "--no-pager"],
            )?;
            print!("{}", String::from_utf8_lossy(&out.stdout));
            eprint!("{}", String::from_utf8_lossy(&out.stderr));
        }
    }

    // Health probe if listening
    let config = Config::load().unwrap_or_default();
    let url = format!("http://{}/v1/health", config.listen);
    match ureq_get(&url) {
        Ok(body) => println!("health: {body}"),
        Err(e) => println!("health: unreachable ({e})"),
    }
    Ok(())
}

/// Tiny GET without pulling reqwest blocking API into CLI hot path awkwardly.
fn ureq_get(url: &str) -> Result<String, String> {
    // Use std + curl if needed; prefer reqwest blocking via tokio runtime is heavy.
    // Shell out to curl for simplicity in status.
    let out = run("curl", &["-fsS", "--max-time", "2", url])?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn logs(follow: bool, lines: usize) -> Result<(), String> {
    let platform = Platform::detect()?;
    match platform {
        Platform::Launchd => {
            let out_log = log_dir().join("klepto.out.log");
            let err_log = log_dir().join("klepto.err.log");
            if follow {
                println!("==> {} <==", out_log.display());
                let status = Command::new("tail")
                    .args([
                        "-n",
                        &lines.to_string(),
                        "-F",
                        &out_log.to_string_lossy(),
                        &err_log.to_string_lossy(),
                    ])
                    .status()
                    .map_err(|e| format!("tail: {e}"))?;
                if !status.success() {
                    return Err("tail exited with error".into());
                }
            } else {
                print_tail(&out_log, lines)?;
                print_tail(&err_log, lines)?;
            }
            Ok(())
        }
        Platform::SystemdUser => {
            let n = lines.to_string();
            let mut cmd = Command::new("journalctl");
            cmd.args(["--user", "-u", SYSTEMD_UNIT, "-n", &n, "--no-pager"]);
            if follow {
                cmd.arg("-f");
            }
            let status = cmd.status().map_err(|e| format!("journalctl: {e}"))?;
            if !status.success() {
                return Err("journalctl exited with error".into());
            }
            Ok(())
        }
    }
}

fn print_tail(path: &Path, lines: usize) -> Result<(), String> {
    if !path.exists() {
        println!("(no log yet: {})", path.display());
        return Ok(());
    }
    println!("==> {} <==", path.display());
    let file = fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let reader = BufReader::new(file);
    let all: Vec<String> = reader.lines().map_while(Result::ok).collect();
    let start = all.len().saturating_sub(lines);
    for line in &all[start..] {
        println!("{line}");
    }
    Ok(())
}

fn uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".into())
}
