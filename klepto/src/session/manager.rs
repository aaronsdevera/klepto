/// Session management: registry, tmux spawning, process I/O
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::config::Config;
use crate::deps;
use crate::profiles::{self, Profile};
use crate::session::events::{StreamState, map_omp_line};
use crate::{AgentMode, Session, SessionEvent, SessionStatus, artifacts};

const EVENT_BUS_CAPACITY: usize = 256;

/// Manages the session registry and provides CRUD operations
#[derive(Clone)]
pub struct SessionManager {
    config: Config,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    buses: Arc<Mutex<HashMap<String, broadcast::Sender<artifacts::SequencedSessionEvent>>>>,
    sequences: Arc<Mutex<HashMap<String, Arc<AtomicU64>>>>,
    captures: Arc<Mutex<HashMap<String, tokio::task::AbortHandle>>>,
}

impl SessionManager {
    pub fn new(config: Config) -> Self {
        let sessions = artifacts::load_sessions()
            .into_iter()
            .map(|mut session| {
                if session.status == SessionStatus::Running {
                    session.status = SessionStatus::Waiting;
                }
                (session.id.clone(), session)
            })
            .collect();
        Self {
            config,
            sessions: Arc::new(Mutex::new(sessions)),
            buses: Arc::new(Mutex::new(HashMap::new())),
            sequences: Arc::new(Mutex::new(HashMap::new())),
            captures: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn list(&self) -> Vec<Session> {
        let sessions = self.sessions.lock().unwrap();
        sessions.values().cloned().collect()
    }

    pub async fn get(&self, id: &str) -> Option<Session> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(id).cloned()
    }

    /// Subscribe to mapped omp RPC events for a session.
    pub fn subscribe(
        &self,
        id: &str,
    ) -> Option<broadcast::Receiver<artifacts::SequencedSessionEvent>> {
        let (tmux_name, cwd, status) = {
            let sessions = self.sessions.lock().unwrap();
            let session = sessions.get(id)?;
            (
                session.tmux_name.clone(),
                session.cwd.clone(),
                session.status.clone(),
            )
        };
        let rx = self.ensure_bus(id).subscribe();
        if status == SessionStatus::Running {
            self.ensure_capture(id, &tmux_name, &cwd);
        }
        Some(rx)
    }

    pub async fn create(
        &self,
        cwd: &str,
        provider: Option<String>,
        model: Option<String>,
        agent_mode: AgentMode,
        pi_args: Option<Vec<String>>,
        profile_name: Option<String>,
    ) -> Result<Session, String> {
        let mut session = Session::new(cwd);
        session.agent_mode = agent_mode;
        session.provider = provider.clone();
        let fallback_profile = match agent_mode {
            AgentMode::Plan => "plan",
            AgentMode::Debug => "debug",
            AgentMode::Agent => "coding",
        };
        let requested_profile = profile_name.as_deref().or(Some(fallback_profile));
        let effective = profiles::resolve(
            &self.config,
            Path::new(cwd),
            requested_profile,
            model.as_deref(),
        )?;
        let isolated = std::env::var("KLEPTO_IN_OCI").as_deref() == Ok("1");
        let network_enforced = std::env::var("KLEPTO_NETWORK_ENFORCED").as_deref() == Ok("1");
        if effective.runner == crate::profiles::RunnerKind::Oci && !isolated {
            return Err(
                "profile requires the OCI runner; start the daemon with runtime=oci".into(),
            );
        }
        if (effective.network.mode == crate::profiles::NetworkMode::None
            || effective.network.deny_direct)
            && !network_enforced
        {
            return Err(
                "network policy requires an enforced OCI network; advisory proxy variables are insufficient"
                    .into(),
            );
        }
        session.model = effective.model.clone();
        session.profile = Some(effective.profile.name.clone());
        session.runner = Some(format!("{:?}", effective.runner).to_ascii_lowercase());
        session.network = Some(effective.network_name.clone());
        session.pi_args = pi_args;
        artifacts::persist_session(&session)?;

        if self.config.auto_install_deps {
            if let Err(e) = deps::ensure(&self.config).await {
                warn!("dependency ensure failed before session create: {e}");
            }
        }

        let tmux_bin = deps::resolve_bin("tmux");
        let omp_bin = deps::resolve_bin(&self.config.omp_bin);

        if tmux_bin.is_none() || omp_bin.is_none() {
            if tmux_bin.is_none() {
                warn!(
                    "tmux not found on PATH — session {} registered without a live harness",
                    session.id
                );
            }
            if omp_bin.is_none() {
                warn!(
                    "omp binary '{}' not found on PATH — session {} registered without a live harness",
                    self.config.omp_bin, session.id
                );
            }
            session.status = SessionStatus::Waiting;
            let _ = self.ensure_bus(&session.id);
            let mut sessions = self.sessions.lock().unwrap();
            sessions.insert(session.id.clone(), session.clone());
            artifacts::persist_session(&session)?;
            return Ok(session);
        }

        let tmux_path = tmux_bin.unwrap();
        let omp_path = omp_bin.unwrap();

        let tmux_name = session.tmux_name.clone();
        let cwd_str = cwd.to_string();

        let mut cmd = tmux_command(&tmux_path, Some(&omp_path));
        cmd.args(["new-session", "-d", "-s", &tmux_name, "-c", &cwd_str, "--"]);

        let current_exe =
            std::env::current_exe().map_err(|e| format!("resolve klepto runner binary: {e}"))?;
        cmd.arg(current_exe).args([
            "runner",
            "--workspace",
            &cwd_str,
            "--session",
            &session.id,
            "--",
        ]);
        if let Some(ref args) = session.pi_args {
            cmd.args(args);
        } else {
            cmd.args(&build_omp_argv(
                &omp_path,
                session.provider.as_deref(),
                session.model.as_deref(),
                &effective.profile,
            ));
        }
        for (key, value) in &effective.env {
            cmd.env(key, value);
        }
        let raw_path = artifacts::WorkspacePaths::new(cwd).raw_events(&session.id);
        let _ = std::fs::remove_file(raw_path);

        let result = cmd.output().await;

        match result {
            Ok(output) => {
                if output.status.success() {
                    configure_tmux_session(&tmux_path, &tmux_name).await;
                    // new-session -d returns before the pane command may exit; confirm it lived.
                    let socket = crate::runner::socket_path(cwd, &session.id);
                    if tmux_has_session(&tmux_path, &tmux_name).await
                        && crate::runner::wait_ready(&socket, 40).await
                    {
                        info!(
                            "spawned tmux session {} (mode={}, model={:?}): {}",
                            tmux_name, agent_mode, session.model, cwd_str
                        );
                        session.status = SessionStatus::Running;
                    } else {
                        warn!(
                            "tmux session {} exited immediately after spawn (is omp on PATH?)",
                            tmux_name
                        );
                        session.status = SessionStatus::Waiting;
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(
                        "tmux spawn for session {} returned non-zero status: {}",
                        session.id, stderr
                    );
                    session.status = SessionStatus::Waiting;
                }
            }
            Err(e) => {
                warn!("failed to spawn tmux session {}: {}", session.id, e);
                session.status = SessionStatus::Waiting;
            }
        }

        let _ = self.ensure_bus(&session.id);
        if session.status == SessionStatus::Running {
            self.ensure_capture(&session.id, &session.tmux_name, &session.cwd);
        }

        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(session.id.clone(), session.clone());
        artifacts::persist_session(&session)?;

        Ok(session)
    }

    /// Run one model turn without creating a durable chat session.
    pub async fn generate_once(
        &self,
        cwd: &str,
        profile_name: &str,
        prompt: &str,
    ) -> Result<String, String> {
        let effective = profiles::resolve(&self.config, Path::new(cwd), Some(profile_name), None)?;
        let isolated = std::env::var("KLEPTO_IN_OCI").as_deref() == Ok("1");
        let network_enforced = std::env::var("KLEPTO_NETWORK_ENFORCED").as_deref() == Ok("1");
        if effective.runner == crate::profiles::RunnerKind::Oci && !isolated {
            return Err(
                "profile requires the OCI runner; start the daemon with runtime=oci".into(),
            );
        }
        if (effective.network.mode == crate::profiles::NetworkMode::None
            || effective.network.deny_direct)
            && !network_enforced
        {
            return Err(
                "network policy requires an enforced OCI network; advisory proxy variables are insufficient"
                    .into(),
            );
        }
        if self.config.auto_install_deps {
            deps::ensure(&self.config).await?;
        }

        let omp_path = deps::resolve_bin(&self.config.omp_bin)
            .ok_or_else(|| format!("omp binary '{}' not found", self.config.omp_bin))?;
        let argv = build_omp_argv(
            &omp_path,
            None,
            effective.model.as_deref(),
            &effective.profile,
        );
        let mut command = tokio::process::Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &effective.env {
            command.env(key, value);
        }

        let mut child = command
            .spawn()
            .map_err(|error| format!("spawn one-shot model: {error}"))?;
        let mut stdin = child.stdin.take().ok_or("one-shot model has no stdin")?;
        let stdout = child.stdout.take().ok_or("one-shot model has no stdout")?;
        let mut stderr = child.stderr.take().ok_or("one-shot model has no stderr")?;
        let stderr_task = tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        });

        let mut payload = serde_json::to_vec(&serde_json::json!({
            "id": "klepto-once-1",
            "type": "prompt",
            "message": prompt,
            "streamingBehavior": "followUp",
        }))
        .map_err(|error| format!("serialize one-shot prompt: {error}"))?;
        payload.push(b'\n');
        stdin
            .write_all(&payload)
            .await
            .map_err(|error| format!("write one-shot prompt: {error}"))?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("flush one-shot prompt: {error}"))?;

        let response = tokio::time::timeout(Duration::from_secs(90), async {
            let mut lines = BufReader::new(stdout).lines();
            let mut stream = StreamState::default();
            let mut text = String::new();
            while let Some(line) = lines
                .next_line()
                .await
                .map_err(|error| format!("read one-shot response: {error}"))?
            {
                for event in map_omp_line(&line, &mut stream) {
                    match event {
                        SessionEvent::TextDelta { text: delta } => text.push_str(&delta),
                        SessionEvent::Error { message } => return Err(message),
                        SessionEvent::Status { status } if status == "agent_end" => {
                            return Ok(text);
                        }
                        _ => {}
                    }
                }
            }
            Err("one-shot model exited before completing a response".into())
        })
        .await;

        drop(stdin);
        let _ = child.kill().await;
        let stderr = stderr_task.await.unwrap_or_default();
        match response {
            Ok(Ok(text)) if !text.trim().is_empty() => Ok(text),
            Ok(Ok(_)) => Err("model returned an empty commit message".into()),
            Ok(Err(error)) if !stderr.trim().is_empty() => {
                Err(format!("{error}: {}", stderr.trim()))
            }
            Ok(Err(error)) => Err(error),
            Err(_) if !stderr.trim().is_empty() => Err(format!(
                "commit message generation timed out: {}",
                stderr.trim()
            )),
            Err(_) => Err("commit message generation timed out".into()),
        }
    }

    pub async fn kill(&self, id: &str) -> Result<(), String> {
        let tmux_name = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(id).map(|s| s.tmux_name.clone())
        };

        let Some(tmux_name) = tmux_name else {
            return Err(format!("session {} not found", id));
        };

        self.stop_capture(id);

        let tmux = deps::resolve_bin("tmux").unwrap_or_else(|| "tmux".into());
        let _ = tmux_command(&tmux, None)
            .args(["pipe-pane", "-t", &tmux_name])
            .output()
            .await;

        let kill_result = tmux_command(&tmux, None)
            .args(["kill-session", "-t", &tmux_name])
            .output()
            .await;

        match kill_result {
            Ok(output) if output.status.success() => {
                info!("killed tmux session {}", tmux_name);
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("failed to kill tmux session {}: {}", tmux_name, stderr);
            }
            Err(e) => {
                warn!("failed to kill tmux session {}: {}", tmux_name, e);
            }
        }

        {
            let mut buses = self.buses.lock().unwrap();
            buses.remove(id);
        }

        let mut sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get_mut(id) {
            session.status = SessionStatus::Killed;
            artifacts::persist_session(session)?;
        }
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> Result<String, String> {
        let sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get(id) {
            Ok(format!("tmux attach -t {}", session.tmux_name))
        } else {
            Err(format!("session {} not found", id))
        }
    }

    /// Send a JSON-RPC command to a live omp session (JSONL on stdin via runner).
    pub async fn send_rpc(&self, id: &str, command: serde_json::Value) -> Result<(), String> {
        let session = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(id).map(|s| (s.cwd.clone(), s.status.clone()))
        };
        let Some((cwd, status)) = session else {
            return Err(format!("session {} not found", id));
        };
        if status != SessionStatus::Running {
            return Err(format!("session {} is not running (status={})", id, status));
        }

        let socket = crate::runner::socket_path(&cwd, id);
        crate::runner::send(&socket, &command).await?;

        info!("sent rpc to runner {}: {}", id, command["type"]);
        Ok(())
    }

    pub async fn prompt(&self, id: &str, message: &str) -> Result<(), String> {
        let req_id = format!("klepto-{}", uuid::Uuid::new_v4());
        self.send_rpc(
            id,
            serde_json::json!({
                "id": req_id,
                "type": "prompt",
                "message": message,
                "streamingBehavior": "followUp",
            }),
        )
        .await
    }

    pub async fn abort(&self, id: &str) -> Result<(), String> {
        self.send_rpc(
            id,
            serde_json::json!({
                "id": format!("klepto-abort-{}", uuid::Uuid::new_v4()),
                "type": "abort",
            }),
        )
        .await
    }

    pub async fn rehydrate(&self) {
        let tmux = match deps::resolve_bin("tmux") {
            Some(p) => p,
            None => {
                warn!("tmux not available for rehydration");
                return;
            }
        };
        let output = tmux_command(&tmux, None)
            .arg("list-sessions")
            .output()
            .await;

        match output {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout);
                let mut restored = Vec::new();
                {
                    let mut sessions = self.sessions.lock().unwrap();
                    for line in text.lines() {
                        if let Some(name) = line.strip_prefix("klepto-") {
                            let id = name.split(':').next().unwrap_or(name);
                            if !sessions.contains_key(id) {
                                warn!(
                                    "found unregistered tmux session klepto-{id}; leaving it untouched"
                                );
                            } else if let Some(session) = sessions.get_mut(id) {
                                session.status = SessionStatus::Running;
                                let _ = artifacts::persist_session(session);
                                restored.push(session.clone());
                                info!("rehydrated session {} from durable metadata", id);
                            }
                        }
                    }
                }
                for session in restored {
                    let _ = self.ensure_bus(&session.id);
                    self.ensure_capture(&session.id, &session.tmux_name, &session.cwd);
                }
            }
            Err(e) => {
                warn!("failed to list tmux sessions for rehydration: {}", e);
            }
        }
    }

    pub fn check_dependencies(&self) -> (bool, bool) {
        (
            deps::resolve_bin("tmux").is_some(),
            deps::resolve_bin(&self.config.omp_bin).is_some(),
        )
    }

    fn ensure_bus(&self, id: &str) -> broadcast::Sender<artifacts::SequencedSessionEvent> {
        let mut buses = self.buses.lock().unwrap();
        buses
            .entry(id.to_string())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(EVENT_BUS_CAPACITY);
                tx
            })
            .clone()
    }

    fn ensure_capture(&self, id: &str, tmux_name: &str, cwd: &str) {
        let mut captures = self.captures.lock().unwrap();
        if captures.contains_key(id) {
            return;
        }
        let tx = self.ensure_bus(id);
        let seq = self.ensure_sequence(id, cwd);
        let tmux = deps::resolve_bin("tmux").unwrap_or_else(|| PathBuf::from("tmux"));
        let handle = tokio::spawn(capture_runner_events(
            tmux,
            tmux_name.to_string(),
            id.to_string(),
            cwd.to_string(),
            tx,
            seq,
            self.sessions.clone(),
        ));
        captures.insert(id.to_string(), handle.abort_handle());
    }

    fn stop_capture(&self, id: &str) {
        let mut captures = self.captures.lock().unwrap();
        if let Some(handle) = captures.remove(id) {
            handle.abort();
        }
    }

    fn ensure_sequence(&self, id: &str, cwd: &str) -> Arc<AtomicU64> {
        let mut sequences = self.sequences.lock().unwrap();
        sequences
            .entry(id.to_string())
            .or_insert_with(|| {
                let last = artifacts::read_events(cwd, id, 0)
                    .last()
                    .map(|event| event.seq)
                    .unwrap_or(0);
                Arc::new(AtomicU64::new(last))
            })
            .clone()
    }
}

/// Tail the runner-owned raw JSONL journal, persist sequenced events, and fan out.
async fn capture_runner_events(
    tmux: PathBuf,
    tmux_name: String,
    session_id: String,
    cwd: String,
    tx: broadcast::Sender<artifacts::SequencedSessionEvent>,
    seq: Arc<AtomicU64>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
) {
    let log_path = artifacts::WorkspacePaths::new(&cwd).raw_events(&session_id);
    info!("capturing structured runner events for {}", tmux_name);

    let capture_offset = artifacts::WorkspacePaths::new(&cwd).capture_offset(&session_id);
    let mut offset = std::fs::read_to_string(&capture_offset)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    let mut leftover = String::new();
    let mut state = StreamState::default();

    loop {
        if !tmux_session_alive(&tmux, &tmux_name).await {
            break;
        }

        match read_new_bytes(&log_path, offset).await {
            Ok((bytes, new_offset)) => {
                offset = new_offset;
                let _ = std::fs::write(&capture_offset, offset.to_string());
                if !bytes.is_empty() {
                    leftover.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(idx) = leftover.find('\n') {
                        let line = leftover[..idx].trim_end_matches('\r').to_string();
                        leftover.drain(..=idx);
                        for event in map_omp_line(&line, &mut state) {
                            let next = seq.fetch_add(1, Ordering::SeqCst) + 1;
                            let event = artifacts::SequencedSessionEvent::new(next, event);
                            if let Err(error) = artifacts::append_event(&cwd, &session_id, &event) {
                                warn!("persist session event failed: {error}");
                            }
                            let _ = tx.send(event);
                        }
                    }
                }
            }
            Err(e) => {
                if log_path.exists() {
                    warn!("runner journal read failed for {}: {}", tmux_name, e);
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    let next = seq.fetch_add(1, Ordering::SeqCst) + 1;
    let terminal = artifacts::SequencedSessionEvent::new(
        next,
        SessionEvent::Status {
            status: "exited".into(),
        },
    );
    let _ = artifacts::append_event(&cwd, &session_id, &terminal);
    let _ = tx.send(terminal);
    if let Some(session) = sessions.lock().unwrap().get_mut(&session_id) {
        if session.status == SessionStatus::Running {
            session.status = SessionStatus::Exited;
            let _ = artifacts::persist_session(session);
        }
    }
    info!("stopped runner event capture for {}", tmux_name);
}

async fn read_new_bytes(path: &Path, offset: u64) -> std::io::Result<(Vec<u8>, u64)> {
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buf = Vec::new();
    let n = file.read_to_end(&mut buf).await?;
    Ok((buf, offset + n as u64))
}

async fn configure_tmux_session(tmux_path: &Path, tmux_name: &str) {
    for (key, value) in [("history-limit", "50000"), ("mouse", "on")] {
        let _ = tmux_command(tmux_path, None)
            .args(["set-option", "-t", tmux_name, key, value])
            .output()
            .await;
    }
}

async fn tmux_session_alive(tmux_path: &Path, tmux_name: &str) -> bool {
    tmux_command(tmux_path, None)
        .args(["has-session", "-t", tmux_name])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Pin tmux to the conventional `/tmp` socket so daemon + IDE terminals share sessions.
/// Also inject PATH for omp when spawning harness panes.
fn tmux_command(tmux_path: &Path, omp_path: Option<&Path>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(tmux_path);
    cmd.env("TMPDIR", "/tmp");
    if let Some(omp) = omp_path {
        cmd.env("PATH", harness_path_env(omp));
    }
    cmd
}

/// PATH for tmux-spawned omp: prepend the binary's directory.
fn harness_path_env(omp_path: &Path) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(parent) = omp_path.parent() {
        dirs.push(parent.to_path_buf());
    }
    let current = std::env::var_os("PATH").unwrap_or_default();
    for p in std::env::split_paths(&current) {
        if !dirs.iter().any(|d| d == &p) {
            dirs.push(p);
        }
    }
    std::env::join_paths(&dirs).unwrap_or(current)
}

async fn tmux_has_session(tmux_path: &Path, tmux_name: &str) -> bool {
    // Brief grace for omp startup before the pane exits on failure.
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(75)).await;
        if tmux_session_alive(tmux_path, tmux_name).await {
            return true;
        }
    }
    false
}

/// Build argv for the omp RPC harness pane.
fn build_omp_argv(
    omp_path: &Path,
    provider: Option<&str>,
    model: Option<&str>,
    profile: &Profile,
) -> Vec<String> {
    build_omp_args(&omp_path.to_string_lossy(), provider, model, profile)
}

/// Build argv for `omp` based on provider/model and a declarative profile.
fn build_omp_args(
    omp_bin: &str,
    provider: Option<&str>,
    model: Option<&str>,
    profile: &Profile,
) -> Vec<String> {
    let mut args = vec![omp_bin.to_string()];

    if let Some(p) = provider.filter(|s| !s.is_empty()) {
        // Skip --provider when model already embeds provider/
        let model_has_provider = model.map(|m| m.contains('/')).unwrap_or(false);
        if !model_has_provider {
            args.push("--provider".into());
            args.push(p.to_string());
        }
    }

    if let Some(m) = model.filter(|s| !s.is_empty()) {
        let model_arg = match provider.filter(|s| !s.is_empty()) {
            Some(p) if !m.contains('/') => format!("{p}/{m}"),
            _ => m.to_string(),
        };
        args.push("--model".into());
        args.push(model_arg);
    }

    args.push("--mode".into());
    args.push("rpc".into());
    args.push("--auto-approve".into());

    if !profile.tools.is_empty() {
        args.push("--tools".into());
        args.push(profile.tools.join(","));
    }
    if let Some(thinking) = profile.thinking.as_deref() {
        args.push("--thinking".into());
        args.push(thinking.into());
    }
    if !profile.system_prompt.trim().is_empty() {
        args.push("--append-system-prompt".into());
        args.push(profile.system_prompt.clone());
    }

    args
}
