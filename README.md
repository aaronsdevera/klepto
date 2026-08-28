# Klepto

Klepto is a local coding agent for VS Code and VSCodium.

```text
VS Code extension ──HTTP/WebSocket──> Rust daemon ──> tmux ──> omp ──> model provider
```

- `klepto` is the daemon and command-line program.
- `klepto-vscode` is the editor extension.
- Workspace data stays under `.klepto/`.
- Global configuration stays under `~/.klepto/`.

- [Install and use Klepto](#install-and-use-klepto)
- [Develop and build from source](#develop-and-build-from-source)

## Install and use Klepto

Klepto supports macOS and Linux.

### Requirements

- [VS Code](https://code.visualstudio.com/) or VSCodium
- [Git](https://git-scm.com/)
- [omp](https://omp.sh/) (oh-my-pi) — installed automatically by `klepto doctor --install` if missing

**You do not need Rust, the Rust toolchain, or Node.js to install the released Klepto binary and extension.** Releases include a precompiled daemon for each supported platform and a precompiled VSIX extension.

Klepto runs `omp` as its model harness. If `omp` is not already installed, `klepto doctor --install` uses the official installer from https://omp.sh/install. Rust is never required for the precompiled installation.

### 1. Install

One non-interactive command installs the daemon and the editor extension:

```bash
curl -fsSL https://raw.githubusercontent.com/aaronsdevera/klepto/main/scripts/install.sh | sh -s -- -y
```

Use `sh -s -- -y` so flags reach the script when it is piped. `-y` (`--yes` / `--non-interactive`) never prompts and closes stdin for child commands.

The installer:

- picks the macOS or Linux binary for your CPU from the [latest release](https://github.com/aaronsdevera/klepto/releases/latest),
- verifies SHA-256 checksums,
- installs the daemon at `~/.klepto/bin/klepto` and adds that directory to your shell `PATH`,
- downloads the VSIX and installs it with `code`, `codium`, or `cursor` when available,
- runs `klepto doctor --install` for runtime dependencies (`tmux`, `omp`, `rg`),
- restarts an existing Klepto user service (or leftover `klepto serve`) so the new binary is used immediately.

Common flags:

```bash
# Daemon only (no VSIX)
curl -fsSL https://raw.githubusercontent.com/aaronsdevera/klepto/main/scripts/install.sh | sh -s -- -y --skip-extension

# Skip dependency auto-install
curl -fsSL https://raw.githubusercontent.com/aaronsdevera/klepto/main/scripts/install.sh | sh -s -- -y --skip-doctor

# Pin a release and editor CLI
curl -fsSL https://raw.githubusercontent.com/aaronsdevera/klepto/main/scripts/install.sh | sh -s -- -y --version v0.5.3 --editor code

# Fail if no editor CLI is found (default: save VSIX under ~/.cache/klepto/)
curl -fsSL https://raw.githubusercontent.com/aaronsdevera/klepto/main/scripts/install.sh | sh -s -- -y --require-editor
```

Also supported: `--dir ~/.klepto/bin`, and the env vars `KLEPTO_VERSION`, `KLEPTO_INSTALL_DIR`, `KLEPTO_EDITOR`, `KLEPTO_SKIP_EXTENSION`, `KLEPTO_SKIP_DOCTOR`, `KLEPTO_YES`.

### 2. Open a project

Open a folder or Git repository. The extension starts the daemon, creates `.klepto/`, and indexes the workspace.

Open chat with `Cmd+L` on macOS or `Ctrl+L` on Linux.

## Configure a model

Run **Klepto: Manage Providers** from the Command Palette.

### Hosted provider

Select **Add built-in provider**, choose the provider, and enter its API key.

### Ollama

Start Ollama and download a model:

```bash
ollama serve
ollama pull qwen3:8b
```

Select **Add Ollama endpoint** in Klepto. Keep `http://127.0.0.1:11434` for local Ollama and leave the API key empty.

Klepto discovers models from `/api/tags` and configures omp (built-in keyless discovery for local Ollama).

### vLLM or another OpenAI-compatible server

Select **Add OpenAI-compatible endpoint** and enter its `/v1` base URL:

```text
http://127.0.0.1:8000/v1
```

Klepto refreshes models from `/models`.

To limit the model picker, run **Klepto: Manage Included Models**.

## Use

### Modes

| Mode | Behavior |
|---|---|
| Agent | Reads and changes files, runs commands, and verifies work |
| Plan | Explores with read-only tools and creates an editable plan |
| Debug | Investigates failures using runtime evidence |

### Shortcuts

| Shortcut | Action |
|---|---|
| `Cmd+L` / `Ctrl+L` | Open chat |
| `Cmd+N` / `Ctrl+N` | Create a chat tab while Klepto is focused |
| `Escape` | Stop the active response |

Chat can include the active file, selected text, open tabs, `@` file references, indexed documents, and attachments.

### Commit messages

Open Source Control and select the sparkle action in its toolbar. Klepto summarizes the staged changes, or all working-tree changes when nothing is staged, and fills the Git commit input. The latest commit subjects are included so the generated message can follow the repository's style.

### Plans

Plan mode saves Markdown under `.klepto/plans/`. The plan editor shows the plan, task status, referenced sessions, and a **Build** button.

Run **Klepto: Open Latest Plan** to reopen the newest plan.

### Sessions

Each chat tab has its own session. Sessions run inside `tmux`:

```bash
klepto session list
klepto attach <session-id>
klepto session kill <session-id>
```

## CLI

Add Klepto to your shell path:

```bash
export PATH="$HOME/.klepto/bin:$PATH"
```

Add that line to `~/.zshrc`, `~/.bashrc`, or the equivalent file for your shell.

```bash
# Daemon
klepto serve
klepto doctor --install

# User service
klepto service install
klepto service status
klepto service logs -f

# Sessions
klepto session create --cwd .
klepto session create --cwd . --mode plan
klepto session create --cwd . --profile review
klepto session prompt <session-id> "Review the authentication code"

# Plans
klepto plan "add workspace profiles" --workspace .
klepto build <plan-id> --workspace .

# Search and memory
klepto search . "function name"
klepto memory remember "Important detail" --workspace .
klepto memory recall "Important detail"
```

## Configuration

Global configuration:

```text
~/.klepto/config.toml
```

Workspace overrides:

```text
<workspace>/.klepto/config.toml
```

Example:

```toml
listen = "127.0.0.1:7420"
omp_bin = "omp"
auto_install_deps = true
default_profile = "coding"
default_model = "provider/model-id"

[networks.direct]
mode = "direct"

[networks.proxy]
mode = "socks5h"
proxy_url = "socks5h://127.0.0.1:1080"
no_proxy = ["127.0.0.1", "localhost"]

# Required before exposing the daemon beyond localhost.
# token = "replace-with-a-random-secret"
```

Built-in profiles are `coding`, `commit`, `review`, `research`, `fact-check`, `plan`, and `debug`. Custom profiles are stored under `~/.klepto/profiles/`.

### Main editor settings

| Setting | Default | Purpose |
|---|---|---|
| `klepto.daemon.listen` | `127.0.0.1:7420` | Daemon address |
| `klepto.daemon.autoStart` | `true` | Start on extension activation |
| `klepto.daemon.path` | empty | Explicit daemon path |
| `klepto.daemon.runtime` | `host` | `host`, `oci`, or `nix` |
| `klepto.defaultMode` | `agent` | Default chat mode |
| `klepto.defaultProvider` | empty | Default provider |
| `klepto.defaultModel` | empty | Default model |
| `klepto.includedModels` | `[]` | Models shown in chat; empty shows all |

Without an explicit daemon path, the extension checks:

1. `~/.klepto/bin/klepto`
2. `/usr/local/bin/klepto`
3. `~/.local/bin/klepto`

Proxy URLs must use `socks5h://` so DNS queries use the proxy. Plain `socks5://` URLs are rejected.

## Data locations

```text
~/.klepto/
├── bin/             Installed daemon
├── config.toml      Global configuration
├── models.toml      Provider catalog
└── profiles/        Custom profiles

.klepto/
├── config.toml      Workspace overrides
├── sessions/        Session metadata and events
├── plans/           Editable plans
├── index/           Repository map, symbols, and indexed documents
├── memory/          Workspace memory
└── artifacts/       Session artifacts
```

Generated workspace data is ignored by `.klepto/.gitignore`.

omp credentials and generated model configuration are stored under `~/.omp/agent/`.

## Troubleshooting

### Daemon does not start

```bash
~/.klepto/bin/klepto doctor
~/.klepto/bin/klepto serve
```

### Extension cannot connect

```bash
curl http://127.0.0.1:7420/v1/health
```

Check `klepto.daemon.listen` and `klepto.daemon.token`.

### No models appear

1. Run **Klepto: Manage Providers**.
2. Confirm that the provider is reachable.
3. Clear **Klepto: Manage Included Models** to show every discovered model.

```bash
# Ollama
curl http://127.0.0.1:11434/api/tags

# OpenAI-compatible server
curl http://127.0.0.1:8000/v1/models
```

### Session is stuck

Press `Escape`. If it does not recover:

```bash
klepto session list
klepto session kill <session-id>
```

### Rebuild workspace data

This deletes workspace sessions, plans, memory, and indexes:

```bash
rm -rf .klepto
```

Reopen the workspace to rebuild it.

## Develop and build from source

This section is for contributors and anyone who wants to modify or build Klepto. Regular users should follow [Install and use Klepto](#install-and-use-klepto) instead.

### Development requirements

- [Git](https://git-scm.com/)
- [Rust](https://rustup.rs/)
- [Node.js](https://nodejs.org/) 20 or newer, including npm
- `make`

Clone the repository and install the locked extension dependencies:

```bash
git clone https://github.com/aaronsdevera/klepto.git
cd klepto
cd klepto-vscode && npm ci && cd ..
```

### Build and test

```bash
# Rust
make build
make serve
(cd klepto && cargo test)

# Extension
(cd klepto-vscode && npm run test:unit)
(cd klepto-vscode && npm run check-types)
(cd klepto-vscode && npm run package)
```

Tag the current version and push it so CI builds binaries and the VSIX, then attaches them to the GitHub Release:

```bash
make bump          # optional: increment version first, then commit
make release
```

Build all release artifacts locally (no tag, no GitHub Release):

```bash
make release-local
```

### Build and run the OCI image

```bash
make release-linux-arm64   # Apple Silicon
# make release-linux-amd64 # Intel/AMD
make image
make container-run KLEPTO_MOUNT="$PWD"
make container-status
```

Set `klepto.daemon.runtime` to `oci`.

The container drops capabilities, enables `no-new-privileges`, applies resource limits, and mounts only approved paths.

Useful commands:

```bash
make container-logs FOLLOW=1
make container-restart
make container-stop
```

## Security

- The host runtime has the current user's file access.
- The OCI runtime limits mounted paths but does not remove the need to review agent actions.
- The daemon listens on localhost by default.
- Set a bearer token before exposing it to another machine.
- Klepto has no hosted service or telemetry.
- Code leaves the machine only when sent to a configured model provider.
