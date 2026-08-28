//! CLI entry point: orchestrates the Klepto daemon via its HTTP API / local managers.

use clap::{Parser, Subcommand};
use klepto::client::ApiClient;
use klepto::config::Config;
use klepto::daemon::server::{create_state, start_server};
use klepto::deps;
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(name = "klepto")]
#[command(about = "Klepto - Local-first Rust harness around pi")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the daemon server
    Serve {
        /// Custom listen address (default: 127.0.0.1:7420; overridden by KLEPTO_LISTEN)
        #[arg(long)]
        listen: Option<String>,
        /// Skip auto-installing missing dependencies
        #[arg(long)]
        no_install: bool,
    },
    /// Check (and optionally install) runtime dependencies
    Doctor {
        /// Install missing deps instead of only reporting
        #[arg(long)]
        install: bool,
        /// Emit machine-readable diagnostics
        #[arg(long)]
        json: bool,
    },
    /// Manage the background daemon service (launchd / systemd)
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
    /// Manage sessions
    Session {
        #[command(subcommand)]
        command: SessionCommands,
    },
    /// Search workspace
    Search {
        /// Workspace path
        workspace: String,
        /// Search query
        query: String,
    },
    /// Index workspace (walk tree, respect .gitignore, generate structure.md)
    Index {
        #[command(subcommand)]
        command: IndexCommands,
    },
    /// Manage memory
    Memory {
        #[command(subcommand)]
        command: MemoryCommands,
    },
    /// Attach to a tmux session
    Attach {
        /// Session ID
        id: String,
    },
    /// Create a timestamped workspace plan artifact
    Plan {
        /// Task title used in the sortable filename
        title: String,
        /// Workspace path
        #[arg(short, long, default_value_t = String::from("."))]
        workspace: String,
        /// Initial markdown content
        #[arg(short, long)]
        content: Option<String>,
    },
    /// Approve and build a workspace plan
    Build {
        /// Plan id (the filename without .md)
        id: String,
        #[arg(short, long, default_value_t = String::from("."))]
        workspace: String,
    },
    /// List available task profiles
    Profiles,
    /// Internal structured runner hosted inside tmux
    #[command(hide = true)]
    Runner {
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        session: String,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum ServiceCommands {
    /// Install and enable the user service
    Install {
        /// Listen address baked into the service unit
        #[arg(long, default_value_t = String::from("127.0.0.1:7420"))]
        listen: String,
    },
    /// Uninstall the user service
    Uninstall,
    /// Start the service
    Start,
    /// Stop the service
    Stop,
    /// Restart the service
    Restart,
    /// Show service + health status
    Status,
    /// Show service logs
    Logs {
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: usize,
    },
}

#[derive(Subcommand, Debug)]
enum SessionCommands {
    /// Create a new session
    Create {
        /// Working directory
        #[arg(short, long, default_value_t = String::from("."))]
        cwd: String,
        /// Provider name (omp --provider)
        #[arg(long)]
        provider: Option<String>,
        /// Model to use (omp --model; accepts provider/id)
        #[arg(long)]
        model: Option<String>,
        /// Spawn profile: agent | plan | debug
        #[arg(long, default_value = "agent")]
        mode: String,
        /// Named task profile (coding, review, research, fact-check, plan, debug)
        #[arg(long)]
        profile: Option<String>,
    },
    /// Prompt a running session
    Prompt { id: String, message: String },
    /// List sessions
    List,
    /// Kill a session
    Kill {
        /// Session ID
        id: String,
    },
}

#[derive(Subcommand, Debug)]
enum IndexCommands {
    /// Index a workspace (walk tree, respect .gitignore, generate structure.md)
    Workspace {
        /// Workspace path
        #[arg(short, long, default_value_t = String::from("."))]
        workspace: String,
    },
    /// Check workspace index status
    Status {
        /// Workspace path
        #[arg(short, long, default_value_t = String::from("."))]
        workspace: String,
    },
}

#[derive(Subcommand, Debug)]
enum MemoryCommands {
    /// Remember a piece of information
    Remember {
        /// Content to remember
        content: String,
        /// Optional workspace
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Recall information by query
    Recall {
        /// Search query
        query: String,
    },
    /// List all memory entries
    List,
    /// Forget a memory entry
    Forget {
        /// Memory entry ID
        id: String,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = Config::load().unwrap_or_default();

    match cli.command {
        Commands::Serve { listen, no_install } => {
            let listen = Config::resolve_listen(listen.as_deref());
            let mut config = Config {
                listen: listen.clone(),
                ..config
            };
            if no_install {
                config.auto_install_deps = false;
            }
            if let Err(error) = config.ensure_data_dir() {
                fail(error);
            }

            if config.auto_install_deps {
                info!("checking runtime dependencies…");
                match deps::ensure(&config).await {
                    Ok(report) => {
                        info!(
                            "deps ready (tmux={}, omp={}, rg={})",
                            report.tmux.path.is_some(),
                            report.omp.path.is_some(),
                            report.rg.path.is_some()
                        );
                    }
                    Err(e) => {
                        error!("dependency setup failed: {e}");
                        eprintln!("klepto: failed to install required dependencies: {e}");
                        eprintln!(
                            "hint: run `klepto doctor --install` or pass `--no-install` to skip"
                        );
                        std::process::exit(1);
                    }
                }
            }

            let state = create_state(config.clone()).await;
            if let Err(error) = start_server(state, config.listen).await {
                fail(error);
            }
        }
        Commands::Doctor { install, json } => {
            if let Err(error) = config.ensure_data_dir() {
                fail(error);
            }
            if !json {
                println!("Klepto dependency check\n");
            }
            let report = if install {
                match deps::ensure(&config).await {
                    Ok(r) => {
                        if !json {
                            println!("Installed missing dependencies where possible.\n");
                        }
                        r
                    }
                    Err(e) => {
                        eprintln!("install failed: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                deps::check(&config)
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": report.all_required_ok(),
                        "klepto_home": Config::home_dir(),
                        "config": Config::path(),
                        "dependencies": {
                            "tmux": report.tmux.path,
                            "omp": report.omp.path,
                            "rg": report.rg.path,
                        },
                        "profiles": klepto::profiles::list_profiles().keys().collect::<Vec<_>>(),
                    })
                );
            } else {
                report.print();
            }
            if !report.all_required_ok() {
                if !json {
                    eprintln!("\nRequired deps missing. Run: klepto doctor --install");
                }
                std::process::exit(1);
            }
            if !json {
                println!("\nAll required dependencies found.");
            }
        }
        Commands::Service { command } => {
            let result = match command {
                ServiceCommands::Install { listen } => klepto::service::install(&listen),
                ServiceCommands::Uninstall => klepto::service::uninstall(),
                ServiceCommands::Start => klepto::service::start(),
                ServiceCommands::Stop => klepto::service::stop(),
                ServiceCommands::Restart => klepto::service::restart(),
                ServiceCommands::Status => klepto::service::status(),
                ServiceCommands::Logs { follow, lines } => klepto::service::logs(follow, lines),
            };
            if let Err(e) = result {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Session { command } => {
            let client = ApiClient::from_config(&config);
            match command {
                SessionCommands::Create {
                    cwd,
                    provider,
                    model,
                    mode,
                    profile,
                } => {
                    let body = serde_json::json!({
                        "cwd": cwd,
                        "provider": provider,
                        "model": model,
                        "agent_mode": mode,
                        "profile": profile,
                    });
                    match client
                        .post::<_, serde_json::Value>("/sessions", &body)
                        .await
                    {
                        Ok(value) => println!("{}", value),
                        Err(error) => fail(error),
                    }
                }
                SessionCommands::Prompt { id, message } => {
                    match client
                        .post::<_, serde_json::Value>(
                            &format!("/sessions/{id}/prompt"),
                            &serde_json::json!({ "message": message }),
                        )
                        .await
                    {
                        Ok(value) => println!("{}", value),
                        Err(error) => fail(error),
                    }
                }
                SessionCommands::List => match client.get::<serde_json::Value>("/sessions").await {
                    Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
                    Err(error) => fail(error),
                },
                SessionCommands::Kill { id } => {
                    match client
                        .delete::<serde_json::Value>(&format!("/sessions/{id}"))
                        .await
                    {
                        Ok(value) => println!("{}", value),
                        Err(error) => fail(error),
                    }
                }
            }
        }
        Commands::Search { workspace, query } => {
            let client = ApiClient::from_config(&config);
            match client
                .post::<_, serde_json::Value>(
                    "/search",
                    &serde_json::json!({ "workspace": workspace, "query": query }),
                )
                .await
            {
                Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
                Err(error) => fail(error),
            }
        }
        Commands::Index { command } => {
            let client = ApiClient::from_config(&config);
            match command {
                IndexCommands::Workspace { workspace } => {
                    match client
                        .post::<_, serde_json::Value>(
                            "/workspace/index",
                            &serde_json::json!({ "workspace": workspace }),
                        )
                        .await
                    {
                        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
                        Err(error) => fail(error),
                    }
                }
                IndexCommands::Status { workspace } => {
                    let path = query_path("/workspace/status", &[("workspace", &workspace)]);
                    match client.get::<serde_json::Value>(&path).await {
                        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
                        Err(error) => fail(error),
                    }
                }
            }
        }
        Commands::Memory { command } => {
            let client = ApiClient::from_config(&config);
            match command {
                MemoryCommands::Remember { content, workspace } => {
                    match client
                        .post::<_, serde_json::Value>(
                            "/memory",
                            &serde_json::json!({ "content": content, "workspace": workspace }),
                        )
                        .await
                    {
                        Ok(value) => println!("{}", value),
                        Err(error) => fail(error),
                    }
                }
                MemoryCommands::Recall { query } => {
                    let path = format!("/memory/search/{}", percent(&query));
                    match client.get::<serde_json::Value>(&path).await {
                        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
                        Err(error) => fail(error),
                    }
                }
                MemoryCommands::List => match client.get::<serde_json::Value>("/memory").await {
                    Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
                    Err(error) => fail(error),
                },
                MemoryCommands::Forget { id } => {
                    match client
                        .delete::<serde_json::Value>(&format!("/memory/{id}"))
                        .await
                    {
                        Ok(value) => println!("{}", value),
                        Err(error) => fail(error),
                    }
                }
            }
        }
        Commands::Attach { id } => {
            let client = ApiClient::from_config(&config);
            match client
                .get::<serde_json::Value>(&format!("/sessions/{id}/resume"))
                .await
            {
                Ok(value) => {
                    if let Some(command) = value.get("command").and_then(|v| v.as_str()) {
                        println!("{command}");
                    } else {
                        println!("{value}");
                    }
                }
                Err(error) => fail(error),
            }
        }
        Commands::Plan {
            title,
            workspace,
            content,
        } => {
            let client = ApiClient::from_config(&config);
            match client
                .post::<_, serde_json::Value>(
                    "/plans",
                    &serde_json::json!({
                        "workspace": workspace,
                        "title": title,
                        "content": content.unwrap_or_default(),
                    }),
                )
                .await
            {
                Ok(value) => {
                    if let Some(path) = value.pointer("/plan/path").and_then(|v| v.as_str()) {
                        println!("{path}");
                    } else {
                        println!("{value}");
                    }
                }
                Err(error) => fail(error),
            }
        }
        Commands::Build { id, workspace } => {
            let client = ApiClient::from_config(&config);
            match client
                .post::<_, serde_json::Value>(
                    &format!("/plans/{id}/build"),
                    &serde_json::json!({ "workspace": workspace }),
                )
                .await
            {
                Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
                Err(error) => fail(error),
            }
        }
        Commands::Profiles => {
            let client = ApiClient::from_config(&config);
            match client.get::<serde_json::Value>("/profiles").await {
                Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
                Err(error) => fail(error),
            }
        }
        Commands::Runner {
            workspace,
            session,
            command,
        } => match klepto::runner::run(&workspace, &session, &command).await {
            Ok(code) => std::process::exit(code),
            Err(error) => fail(error),
        },
    }
}

fn fail(error: impl std::fmt::Display) -> ! {
    eprintln!("error: {error}");
    std::process::exit(1)
}

fn query_path(path: &str, pairs: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse(&format!("http://localhost{path}")).unwrap();
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    match url.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_string(),
    }
}

fn percent(value: &str) -> String {
    let mut url = reqwest::Url::parse("http://localhost/").unwrap();
    url.path_segments_mut().unwrap().push(value);
    url.path().trim_start_matches('/').to_string()
}
