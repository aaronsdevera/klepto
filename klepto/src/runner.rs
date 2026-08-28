//! Structured omp runner hosted by tmux.
//!
//! The runner owns omp's pipes and exposes a tiny local Unix-socket control
//! protocol. tmux remains the process supervisor and human attachment surface.
//!
//! Unix domain sockets are unavailable on Windows, so the runner API compiles
//! there but returns explicit errors at runtime (sessions remain Unix/tmux).

use std::path::{Path, PathBuf};

use crate::artifacts::WorkspacePaths;

pub fn socket_path(workspace: &str, session_id: &str) -> PathBuf {
    WorkspacePaths::new(workspace).runner_socket(session_id)
}

#[cfg(unix)]
pub async fn run(workspace: &str, session_id: &str, command: &[String]) -> Result<i32, String> {
    use std::process::Stdio;
    use std::sync::Arc;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::process::Command;
    use tokio::sync::Mutex;
    use tracing::{info, warn};

    use crate::artifacts::ensure_workspace;

    if command.is_empty() {
        return Err("runner requires a command after --".into());
    }
    let paths = ensure_workspace(workspace)?;
    let session_dir = paths.session_dir(session_id);
    std::fs::create_dir_all(&session_dir)
        .map_err(|e| format!("create {}: {e}", session_dir.display()))?;
    let socket = paths.runner_socket(session_id);
    let _ = std::fs::remove_file(&socket);

    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn runner child '{}': {e}", command[0]))?;
    let stdin = Arc::new(Mutex::new(
        child.stdin.take().ok_or("runner child has no stdin")?,
    ));
    let stdout = child.stdout.take().ok_or("runner child has no stdout")?;
    let stderr = child.stderr.take().ok_or("runner child has no stderr")?;

    let raw_path = paths.raw_events(session_id);
    let stdout_task = tokio::spawn(copy_output(stdout, raw_path.clone(), false));
    let stderr_task = tokio::spawn(copy_output(stderr, raw_path, true));
    let listener = UnixListener::bind(&socket)
        .map_err(|e| format!("bind runner socket {}: {e}", socket.display()))?;
    info!("runner {} listening at {}", session_id, socket.display());

    let status = loop {
        tokio::select! {
            status = child.wait() => {
                break status.map_err(|e| format!("wait for runner child: {e}"))?;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let stdin = stdin.clone();
                        tokio::spawn(async move {
                            if let Err(error) = handle_control(stream, stdin).await {
                                warn!("runner control error: {error}");
                            }
                        });
                    }
                    Err(error) => warn!("runner accept failed: {error}"),
                }
            }
        }
    };
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    let _ = std::fs::remove_file(socket);
    Ok(status.code().unwrap_or(1))
}

#[cfg(not(unix))]
pub async fn run(_workspace: &str, _session_id: &str, _command: &[String]) -> Result<i32, String> {
    Err("klepto runner requires Unix (tmux + Unix domain sockets)".into())
}

#[cfg(unix)]
pub async fn send(socket: &Path, payload: &serde_json::Value) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    let mut stream = UnixStream::connect(socket)
        .await
        .map_err(|e| format!("connect runner socket {}: {e}", socket.display()))?;
    let mut line =
        serde_json::to_vec(payload).map_err(|e| format!("serialize runner command: {e}"))?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .await
        .map_err(|e| format!("write runner command: {e}"))?;
    stream
        .shutdown()
        .await
        .map_err(|e| format!("close runner command: {e}"))
}

#[cfg(not(unix))]
pub async fn send(_socket: &Path, _payload: &serde_json::Value) -> Result<(), String> {
    Err("klepto runner requires Unix (tmux + Unix domain sockets)".into())
}

#[cfg(unix)]
pub async fn wait_ready(socket: &Path, attempts: usize) -> bool {
    use tokio::net::UnixStream;

    for _ in 0..attempts {
        if UnixStream::connect(socket).await.is_ok() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(75)).await;
    }
    false
}

#[cfg(not(unix))]
pub async fn wait_ready(_socket: &Path, _attempts: usize) -> bool {
    false
}

#[cfg(unix)]
async fn handle_control(
    stream: tokio::net::UnixStream,
    stdin: std::sync::Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|e| format!("read control command: {e}"))?;
    if line.trim().is_empty() {
        return Ok(());
    }
    // Validate that control input is one JSON value before forwarding it to omp.
    serde_json::from_str::<serde_json::Value>(&line)
        .map_err(|e| format!("invalid runner JSON: {e}"))?;
    let mut child_stdin = stdin.lock().await;
    child_stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("write omp stdin: {e}"))?;
    child_stdin
        .flush()
        .await
        .map_err(|e| format!("flush omp stdin: {e}"))
}

#[cfg(unix)]
async fn copy_output<R>(reader: R, path: PathBuf, is_stderr: bool) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| format!("read runner output: {e}"))?
    {
        if is_stderr {
            eprintln!("{line}");
            continue;
        }
        println!("{line}");
        append_raw(&path, &line).await?;
    }
    Ok(())
}

#[cfg(unix)]
async fn append_raw(path: &Path, line: &str) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|e| format!("open raw event journal: {e}"))?;
    file.write_all(line.as_bytes())
        .await
        .map_err(|e| format!("append raw event: {e}"))?;
    file.write_all(b"\n")
        .await
        .map_err(|e| format!("append raw event newline: {e}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_socket_forwards_json_and_journals_output() {
        let root = std::env::temp_dir().join(format!("klepto-runner-{}", crate::short_id()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = root.to_string_lossy().to_string();
        let session = "runner-test";
        let command = vec![
            "/bin/sh".into(),
            "-c".into(),
            "IFS= read -r line; printf '%s\\n' \"$line\"".into(),
        ];
        let task_workspace = workspace.clone();
        let task =
            tokio::spawn(async move { run(&task_workspace, session, &command).await.unwrap() });
        let socket = socket_path(&workspace, session);
        assert!(wait_ready(&socket, 20).await);
        send(
            &socket,
            &serde_json::json!({ "type": "prompt", "message": "hello" }),
        )
        .await
        .unwrap();
        assert_eq!(task.await.unwrap(), 0);
        let raw =
            std::fs::read_to_string(WorkspacePaths::new(&workspace).raw_events(session)).unwrap();
        assert!(raw.contains("\"message\":\"hello\""));
        let _ = std::fs::remove_dir_all(root);
    }
}
