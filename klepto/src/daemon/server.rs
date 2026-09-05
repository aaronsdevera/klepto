//! Daemon HTTP/WebSocket server.

use axum::Router;
use axum::extract::ws::Message;
use axum::extract::{Json, Path, State, WebSocketUpgrade};
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::{delete, get, post};
use tracing::{error, info};

use axum::extract::Query;

use crate::artifacts::{self, PlanAgentReference, PlanAgentRole, PlanStatus};
use crate::config::Config;
use crate::expand_prompt_message;
use crate::index::manager::IndexManager;
use crate::index::workspace::WorkspaceIndexer;
use crate::memory::manager::MemoryManager;
use crate::session::manager::SessionManager;
use crate::{CreateSessionRequest, PromptRequest};
use std::path::PathBuf;

/// Application state shared across all request handlers
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub session_manager: SessionManager,
    pub index_manager: IndexManager,
    pub workspace_indexer: WorkspaceIndexer,
    pub memory_manager: MemoryManager,
    pub started_at: std::sync::Arc<std::time::Instant>,
}

/// Create the application state with all managers
pub async fn create_state(config: Config) -> AppState {
    let session_manager = SessionManager::new(config.clone());
    session_manager.rehydrate().await;
    AppState {
        session_manager,
        index_manager: IndexManager::new(config.clone()),
        workspace_indexer: WorkspaceIndexer::new(config.clone()),
        memory_manager: MemoryManager::new(config.clone()),
        started_at: std::sync::Arc::new(std::time::Instant::now()),
        config,
    }
}

/// Create the axum router with all routes
pub fn create_app(state: AppState) -> Router {
    let auth_state = state.clone();
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/commit-message", post(generate_commit_message))
        .route("/v1/sessions", get(list_sessions).post(create_session))
        .route("/v1/sessions/{id}", get(get_session).delete(kill_session))
        .route("/v1/sessions/{id}/prompt", post(prompt_session))
        .route("/v1/sessions/{id}/interrupt", post(interrupt_session))
        .route("/v1/sessions/{id}/resume", get(resume_session))
        .route("/v1/sessions/{id}/events", get(stream_events))
        .route("/v1/plans", get(list_plans).post(create_plan))
        .route("/v1/plans/{id}", get(get_plan).put(update_plan))
        .route("/v1/plans/{id}/todos/{todo_id}", post(update_plan_todo))
        .route("/v1/plans/{id}/approve", post(approve_plan))
        .route("/v1/plans/{id}/build", post(build_plan))
        .route("/v1/profiles", get(list_profiles))
        .route("/v1/providers", get(list_providers).post(upsert_provider))
        .route("/v1/providers/{id}", delete(delete_provider))
        .route("/v1/config/effective", get(effective_config))
        .route("/v1/search", post(search_workspace))
        .route("/v1/index", post(index_workspace))
        .route("/v1/index/{workspace}", delete(remove_index))
        .route("/v1/index/docs/fetch", post(fetch_index_doc))
        .route("/v1/index/docs", get(list_index_docs))
        .route("/v1/workspace/index", post(index_workspace_code))
        .route("/v1/workspace/status", get(workspace_index_status))
        .route("/v1/memory", get(list_memory).post(remember_memory))
        .route("/v1/memory/{id}", delete(forget_memory))
        .route("/v1/memory/search/{query}", get(recall_memory))
        .layer(middleware::from_fn_with_state(auth_state, require_auth))
        .with_state(state)
}

async fn require_auth(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(expected) = state
        .config
        .token
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return next.run(request).await;
    };
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let query_token = request.uri().query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "access_token").then(|| {
                percent_encoding::percent_decode_str(value)
                    .decode_utf8_lossy()
                    .into_owned()
            })
        })
    });
    if supplied == Some(expected) || query_token.as_deref() == Some(expected) {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "missing or invalid bearer token" })),
        )
            .into_response()
    }
}

/// Start the HTTP/WS server on the configured address
pub async fn start_server(state: AppState, listen: String) -> Result<(), String> {
    let app = create_app(state);

    info!("Starting Klepto daemon on {}", listen);

    let listener = tokio::net::TcpListener::bind(&listen).await.map_err(|e| {
        format!(
            "failed to bind to {listen}: {e}. Is another klepto already listening? Try `klepto service status` or free the port."
        )
    })?;

    axum::serve(listener, app).await.map_err(|e| {
        error!("Server error: {}", e);
        format!("server error: {e}")
    })
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let (tmux_available, omp_available) = state.session_manager.check_dependencies();
    Json(serde_json::json!({
        "ok": true,
        "tmux_available": tmux_available,
        "omp_available": omp_available,
        "omp_bin": state.config.omp_bin,
        // Legacy aliases for older extension builds.
        "pi_available": omp_available,
        "pi_bin": state.config.omp_bin,
        "uptime_seconds": state.started_at.elapsed().as_secs(),
        "message": "Klepto daemon is running"
    }))
}

#[derive(Debug, serde::Deserialize, Default)]
struct ModelsQuery {
    refresh: Option<String>,
}

async fn list_models(
    State(state): State<AppState>,
    Query(query): Query<ModelsQuery>,
) -> impl IntoResponse {
    Json(
        crate::models::list_models_with_refresh(
            &state.config,
            crate::models::refresh_requested(query.refresh.as_deref()),
        )
        .await,
    )
}

async fn generate_commit_message(
    State(state): State<AppState>,
    Json(payload): Json<CommitMessageRequest>,
) -> Response {
    if payload.diff.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "no Git changes to summarize" })),
        )
            .into_response();
    }
    if payload.diff.len() > 120_000 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": "Git diff exceeds the 120,000 byte limit" })),
        )
            .into_response();
    }

    let prompt = commit_message_prompt(&payload.diff, &payload.previous_messages);
    match state
        .session_manager
        .generate_once(&payload.workspace, "commit", &prompt)
        .await
    {
        Ok(message) => (
            StatusCode::OK,
            Json(serde_json::json!({ "message": message.trim() })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response(),
    }
}

fn commit_message_prompt(diff: &str, previous_messages: &[String]) -> String {
    let style = if previous_messages.is_empty() {
        "No previous commit subjects were available.".to_string()
    } else {
        previous_messages
            .iter()
            .take(10)
            .map(|message| format!("- {}", message.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "Generate the commit message for the Git diff below. Describe the intent of the change, not a file-by-file inventory. Match the repository's existing style when useful. Return only the commit message with no Markdown fences, labels, or commentary.\n\nPrevious commit subjects:\n{style}\n\n<git_diff>\n{diff}\n</git_diff>"
    )
}

async fn list_sessions(State(state): State<AppState>) -> impl IntoResponse {
    let sessions = state.session_manager.list().await;
    Json(serde_json::json!({ "sessions": sessions }))
}

async fn get_session(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match state.session_manager.get(&id).await {
        Some(s) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "session": s })),
        ),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("session {} not found", id)
            })),
        ),
    }
}

async fn create_session(
    State(state): State<AppState>,
    Json(payload): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let agent_mode = payload.agent_mode.unwrap_or_default();
    let _cwd = payload.cwd.clone();

    // Auto-index workspace in background (fire-and-forget)
    let ws_indexer = state.workspace_indexer.clone();
    let cwd_clone = payload.cwd.clone();
    tokio::spawn(async move {
        let ws_path = PathBuf::from(&cwd_clone);
        if !ws_indexer.is_indexed(&ws_path) {
            match ws_indexer.index_workspace(&ws_path).await {
                Ok(idx) => {
                    info!(
                        "auto-indexed workspace {} ({} files, {} bytes)",
                        cwd_clone,
                        idx.files.len(),
                        idx.total_bytes
                    );
                }
                Err(e) => {
                    tracing::warn!("auto-index workspace {} failed: {}", cwd_clone, e);
                }
            }
        }
    });

    match state
        .session_manager
        .create(
            &payload.cwd,
            payload.provider,
            payload.model,
            agent_mode,
            payload.pi_args,
            payload.profile,
        )
        .await
    {
        Ok(s) => (
            axum::http::StatusCode::CREATED,
            Json(serde_json::json!({ "session": s })),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn kill_session(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match state.session_manager.kill(&id).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "message": format!("session {} killed", id)
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn prompt_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<PromptRequest>,
) -> impl IntoResponse {
    let expanded = expand_prompt_message(&payload.message, payload.context.as_ref());
    match state.session_manager.prompt(&id, &expanded).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "ok": true,
                "session_id": id,
                "message": "Prompt accepted by omp RPC harness. Streaming events will follow on the WebSocket.",
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn interrupt_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.session_manager.abort(&id).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "ok": true,
                "session_id": id,
                "message": "Abort sent to omp RPC harness",
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn resume_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.session_manager.resume(&id).await {
        Ok(command) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "command": command })),
        ),
        Err(error) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}

async fn stream_events(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<EventCursor>,
) -> impl IntoResponse {
    ws.on_upgrade(move |mut socket| async move {
        use futures::StreamExt;
        use tokio::sync::broadcast;

        info!("WebSocket connected for session: {}", id);
        let payload = serde_json::json!({
            "type": "connected",
            "session_id": id
        })
        .to_string();
        if socket.send(Message::Text(payload.into())).await.is_err() {
            return;
        }

        let session = state.session_manager.get(&id).await;
        let mut rx = match state.session_manager.subscribe(&id) {
            Some(rx) => rx,
            None => {
                tracing::debug!("WebSocket for unknown session {}; keeping alive", id);
                while let Some(msg) = socket.next().await {
                    match msg {
                        Ok(Message::Close(_)) => break,
                        Ok(Message::Ping(data)) => {
                            if socket.send(Message::Pong(data)).await.is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::debug!("WebSocket error for session {}: {}", id, e);
                            break;
                        }
                    }
                }
                info!("WebSocket closed for session: {}", id);
                return;
            }
        };

        if let Some(session) = session {
            for event in artifacts::read_events(&session.cwd, &id, query.after.unwrap_or(0)) {
                let Ok(payload) = serde_json::to_string(&event) else {
                    continue;
                };
                if socket.send(Message::Text(payload.into())).await.is_err() {
                    return;
                }
            }
        }

        loop {
            tokio::select! {
                event = rx.recv() => {
                    match event {
                        Ok(event) => {
                            let payload = match serde_json::to_string(&event) {
                                Ok(s) => s,
                                Err(_) => continue,
                            };
                            if socket.send(Message::Text(payload.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                msg = socket.next() => {
                    match msg {
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(Message::Ping(data))) => {
                            if socket.send(Message::Pong(data)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            tracing::debug!("WebSocket error for session {}: {}", id, e);
                            break;
                        }
                    }
                }
            }
        }
        info!("WebSocket closed for session: {}", id);
    })
}

async fn create_plan(Json(payload): Json<CreatePlanRequest>) -> impl IntoResponse {
    match artifacts::create_plan_with_author(
        &payload.workspace,
        &payload.title,
        &payload.content.unwrap_or_default(),
        payload.author_session_id.as_deref(),
    ) {
        Ok(plan) => (
            axum::http::StatusCode::CREATED,
            Json(serde_json::json!({ "plan": plan })),
        ),
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}

async fn list_plans(Query(query): Query<WorkspaceQuery>) -> impl IntoResponse {
    match artifacts::list_plans(&query.workspace) {
        Ok(plans) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "plans": plans })),
        ),
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}

async fn get_plan(
    Path(id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    match artifacts::load_plan(&query.workspace, &id) {
        Ok(plan) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "plan": plan })),
        ),
        Err(error) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}

async fn update_plan(
    Path(id): Path<String>,
    Json(payload): Json<UpdatePlanRequest>,
) -> impl IntoResponse {
    match artifacts::update_plan(&payload.workspace, &id, payload.content, payload.status) {
        Ok(plan) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "plan": plan })),
        ),
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}

async fn update_plan_todo(
    Path((id, todo_id)): Path<(String, String)>,
    Json(payload): Json<UpdatePlanTodoRequest>,
) -> impl IntoResponse {
    match artifacts::update_plan_todo(&payload.workspace, &id, &todo_id, payload.status) {
        Ok(plan) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "plan": plan })),
        ),
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}

async fn approve_plan(
    Path(id): Path<String>,
    Json(payload): Json<WorkspaceRequest>,
) -> impl IntoResponse {
    match artifacts::update_plan(&payload.workspace, &id, None, Some(PlanStatus::Approved)) {
        Ok(plan) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "plan": plan })),
        ),
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}

async fn build_plan(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<WorkspaceRequest>,
) -> impl IntoResponse {
    let current = match artifacts::load_plan(&payload.workspace, &id) {
        Ok(plan) => plan,
        Err(error) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            );
        }
    };
    if !matches!(current.status, PlanStatus::Draft | PlanStatus::Approved) {
        return (
            axum::http::StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": format!("plan cannot be built from status {:?}", current.status)
            })),
        );
    }
    let rollback_status = current.status.clone();
    let plan =
        match artifacts::update_plan(&payload.workspace, &id, None, Some(PlanStatus::Building)) {
            Ok(plan) => plan,
            Err(error) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": error })),
                );
            }
        };
    let session = match state
        .session_manager
        .create(
            &payload.workspace,
            None,
            None,
            crate::AgentMode::Agent,
            None,
            Some("coding".into()),
        )
        .await
    {
        Ok(session) => session,
        Err(error) => {
            let _ = artifacts::update_plan(
                &payload.workspace,
                &id,
                None,
                Some(rollback_status.clone()),
            );
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            );
        }
    };
    let prompt = format!(
        "Build the plan at {} (revision {}). Follow it exactly and verify the result. Update each todo status in the plan frontmatter as work progresses, and set the plan status to completed only after verification succeeds.",
        plan.path, plan.revision
    );
    if let Err(error) = state.session_manager.prompt(&session.id, &prompt).await {
        let _ = state.session_manager.kill(&session.id).await;
        let _ =
            artifacts::update_plan(&payload.workspace, &id, None, Some(rollback_status.clone()));
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        );
    }
    let agent = PlanAgentReference {
        session_id: session.id.clone(),
        role: PlanAgentRole::Builder,
        label: format!("Build {}", plan.title),
        todo_ids: plan.todos.iter().map(|todo| todo.id.clone()).collect(),
        created_at: chrono::Utc::now(),
    };
    match artifacts::add_plan_agent(&payload.workspace, &id, agent) {
        Ok(plan) => (
            axum::http::StatusCode::CREATED,
            Json(serde_json::json!({ "plan": plan, "session": session })),
        ),
        Err(error) => {
            let _ = state.session_manager.kill(&session.id).await;
            let _ = artifacts::update_plan(&payload.workspace, &id, None, Some(rollback_status));
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
        }
    }
}

async fn list_profiles() -> impl IntoResponse {
    Json(serde_json::json!({ "profiles": crate::profiles::list_profiles() }))
}

async fn list_providers() -> impl IntoResponse {
    Json(serde_json::json!({ "catalog": crate::providers::load_catalog() }))
}

async fn upsert_provider(Json(payload): Json<ProviderRequest>) -> impl IntoResponse {
    let definition = crate::providers::ProviderDefinition {
        id: payload.id,
        kind: payload.kind,
        base_url: payload.base_url,
        api: payload.api,
        models: payload.models,
    };
    match crate::providers::upsert(definition, payload.api_key.as_deref()) {
        Ok(catalog) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "catalog": catalog })),
        ),
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}

async fn delete_provider(Path(id): Path<String>) -> impl IntoResponse {
    match crate::providers::remove(&id) {
        Ok(catalog) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "catalog": catalog })),
        ),
        Err(error) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}

async fn effective_config(
    State(state): State<AppState>,
    Query(query): Query<EffectiveConfigQuery>,
) -> impl IntoResponse {
    match crate::profiles::resolve(
        &state.config,
        std::path::Path::new(&query.workspace),
        query.profile.as_deref(),
        query.model.as_deref(),
    ) {
        Ok(config) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "config": config })),
        ),
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}

async fn index_workspace(
    State(state): State<AppState>,
    Json(payload): Json<IndexWorkspaceRequest>,
) -> impl IntoResponse {
    match state
        .index_manager
        .index_workspace(&payload.workspace)
        .await
    {
        Ok(index_state) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "index_state": index_state })),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn search_workspace(
    State(state): State<AppState>,
    Json(payload): Json<SearchRequest>,
) -> impl IntoResponse {
    match state
        .index_manager
        .search(&payload.workspace, &payload.query)
        .await
    {
        Ok(hits) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "hits": hits })),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn remove_index(
    State(state): State<AppState>,
    Path(workspace): Path<String>,
) -> impl IntoResponse {
    match state.index_manager.delete_workspace(&workspace).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "message": format!("index removed for {}", workspace)
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn fetch_index_doc(
    State(state): State<AppState>,
    Json(payload): Json<FetchDocRequest>,
) -> impl IntoResponse {
    match state
        .index_manager
        .fetch_and_store(&payload.workspace, &payload.url)
        .await
    {
        Ok(doc) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "doc": doc })),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn list_index_docs(
    State(state): State<AppState>,
    Query(query): Query<ListDocsQuery>,
) -> impl IntoResponse {
    match state.index_manager.list_docs(&query.workspace).await {
        Ok(docs) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "docs": docs })),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn list_memory(State(state): State<AppState>) -> impl IntoResponse {
    match state.memory_manager.list().await {
        Ok(entries) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "entries": entries })),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn remember_memory(
    State(state): State<AppState>,
    Json(payload): Json<RememberRequest>,
) -> impl IntoResponse {
    match state
        .memory_manager
        .remember(&payload.content, payload.workspace.as_deref())
        .await
    {
        Ok(e) => (
            axum::http::StatusCode::CREATED,
            Json(serde_json::json!({ "entry": e })),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn recall_memory(
    State(state): State<AppState>,
    Path(query): Path<String>,
    Query(scope): Query<MemoryScopeQuery>,
) -> impl IntoResponse {
    match state
        .memory_manager
        .recall_scoped(&query, scope.workspace.as_deref())
        .await
    {
        Ok(entries) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "entries": entries })),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn forget_memory(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match state.memory_manager.forget(&id).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "message": format!("memory {} forgotten", id)
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn index_workspace_code(
    State(state): State<AppState>,
    Json(payload): Json<IndexWorkspaceCodeRequest>,
) -> impl IntoResponse {
    let ws_path = PathBuf::from(&payload.workspace);
    match state.workspace_indexer.index_workspace(&ws_path).await {
        Ok(idx) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "workspace_index": idx,
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
    }
}

async fn workspace_index_status(
    State(state): State<AppState>,
    Query(query): Query<WorkspaceStatusQuery>,
) -> impl IntoResponse {
    let ws_path = PathBuf::from(&query.workspace);
    let is_indexed = state.workspace_indexer.is_indexed(&ws_path);
    let has_changes = if is_indexed {
        match state.workspace_indexer.has_changes(&ws_path).await {
            Ok(changes) => Some(changes),
            Err(_e) => Some(false), // on error, don't flag changes
        }
    } else {
        None
    };

    Json(serde_json::json!({
        "workspace": query.workspace,
        "is_indexed": is_indexed,
        "has_changes": has_changes,
    }))
}

#[derive(Debug, serde::Deserialize)]
struct IndexWorkspaceRequest {
    workspace: String,
}

#[derive(Debug, serde::Deserialize)]
struct SearchRequest {
    workspace: String,
    query: String,
}

#[derive(Debug, serde::Deserialize)]
struct CommitMessageRequest {
    workspace: String,
    diff: String,
    #[serde(default)]
    previous_messages: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct FetchDocRequest {
    workspace: String,
    url: String,
}

#[derive(Debug, serde::Deserialize)]
struct ListDocsQuery {
    workspace: String,
}

#[derive(Debug, serde::Deserialize)]
struct RememberRequest {
    content: String,
    workspace: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct IndexWorkspaceCodeRequest {
    workspace: String,
}

#[derive(Debug, serde::Deserialize)]
struct WorkspaceStatusQuery {
    workspace: String,
}

#[derive(Debug, serde::Deserialize)]
struct EventCursor {
    after: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
struct WorkspaceQuery {
    workspace: String,
}

#[derive(Debug, serde::Deserialize)]
struct WorkspaceRequest {
    workspace: String,
}

#[derive(Debug, serde::Deserialize)]
struct CreatePlanRequest {
    workspace: String,
    title: String,
    content: Option<String>,
    author_session_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct UpdatePlanRequest {
    workspace: String,
    content: Option<String>,
    status: Option<PlanStatus>,
}

#[derive(Debug, serde::Deserialize)]
struct UpdatePlanTodoRequest {
    workspace: String,
    status: artifacts::PlanTodoStatus,
}

#[derive(Debug, serde::Deserialize)]
struct EffectiveConfigQuery {
    workspace: String,
    profile: Option<String>,
    model: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct MemoryScopeQuery {
    workspace: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ProviderRequest {
    id: String,
    #[serde(default)]
    kind: crate::providers::ProviderKind,
    base_url: Option<String>,
    api: Option<String>,
    #[serde(default)]
    models: Vec<String>,
    api_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state(config: Config) -> AppState {
        AppState {
            session_manager: SessionManager::new(config.clone()),
            index_manager: IndexManager::new(config.clone()),
            workspace_indexer: WorkspaceIndexer::new(config.clone()),
            memory_manager: MemoryManager::new(config.clone()),
            started_at: std::sync::Arc::new(std::time::Instant::now()),
            config,
        }
    }

    #[tokio::test]
    async fn plan_api_creates_sortable_workspace_artifact() {
        let root = std::env::temp_dir().join(format!("klepto-api-{}", crate::short_id()));
        std::fs::create_dir_all(&root).unwrap();
        let app = create_app(test_state(Config::default()));
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/plans")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "workspace": root,
                            "title": "Add plan workflow",
                            "author_session_id": "plan-author",
                            "content": "---\nname: Add plan workflow\noverview: Test plan APIs\ntodos:\n  - id: artifact\n    content: Add artifact\n    status: pending\nisProject: false\n---\n\n# Add plan workflow"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let id = value["plan"]["id"].as_str().unwrap();
        assert!(id.contains("add-plan-workflow"));
        assert!(root.join(format!(".klepto/plans/{id}.md")).exists());
        assert_eq!(value["plan"]["agents"][0]["session_id"], "plan-author");

        let response = app
            .oneshot(
                Request::post(format!("/v1/plans/{id}/todos/artifact"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "workspace": root,
                            "status": "completed"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["plan"]["todos"][0]["status"], "completed");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn configured_token_is_enforced() {
        let app = create_app(test_state(Config {
            token: Some("test-token".into()),
            ..Config::default()
        }));
        let denied = app
            .clone()
            .oneshot(Request::get("/v1/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let allowed = app
            .oneshot(
                Request::get("/v1/health")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[test]
    fn commit_prompt_includes_diff_and_repository_style() {
        let prompt = commit_message_prompt(
            "diff --git a/file b/file\n+new line",
            &["feat: add existing behavior".into()],
        );
        assert!(prompt.contains("feat: add existing behavior"));
        assert!(prompt.contains("+new line"));
        assert!(prompt.contains("Return only the commit message"));
    }
}
