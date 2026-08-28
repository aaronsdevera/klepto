// Type definitions shared between extension and daemon

export type AgentMode = 'agent' | 'plan' | 'debug';

export interface Session {
  id: string;
  tmux_name: string;
  cwd: string;
  created_at: string;
  status: string;
  pi_mode?: string;
  omp_mode?: string;
  agent_mode?: AgentMode;
  provider?: string;
  model?: string;
  pi_args?: string[];
  parent_id?: string;
  worker_role?: string;
  profile?: string;
  runner?: string;
  network?: string;
}

export interface ModelInfo {
  provider: string;
  id: string;
  label: string;
}

export interface ModelsResponse {
  models: ModelInfo[];
  providers: string[];
  suggested: boolean;
  message?: string;
}

export interface MemoryEntry {
  id: string;
  content: string;
  created_at: string;
  workspace?: string;
}

export interface SearchHit {
  file: string;
  line: number;
  text: string;
  score: number;
  backend: string;
  provenance: string;
  freshness: string;
}

export interface HealthResponse {
  ok: boolean;
  tmux_available: boolean;
  omp_available?: boolean;
  omp_bin?: string;
  /** @deprecated use omp_available */
  pi_available?: boolean;
  /** @deprecated use omp_bin */
  pi_bin?: string;
  uptime_seconds: number;
}

export interface CreateSessionOptions {
  model?: string;
  provider?: string;
  agentMode?: AgentMode;
  profile?: string;
}

export interface Profile {
  name?: string;
  description?: string;
  runner?: string;
  network?: string;
  [key: string]: unknown;
}

export interface ProfilesResponse {
  profiles: Record<string, Profile>;
}

export interface EffectiveConfig {
  [key: string]: unknown;
}

export type PlanStatus =
  | 'draft'
  | 'approved'
  | 'building'
  | 'completed'
  | 'rejected'
  | string;

export type PlanTodoStatus = 'pending' | 'in_progress' | 'completed' | 'cancelled';

export interface PlanTodo {
  id: string;
  content: string;
  status: PlanTodoStatus;
}

export interface PlanAgentReference {
  session_id: string;
  role: 'author' | 'builder';
  label: string;
  todo_ids: string[];
  created_at: string;
}

export interface PlanArtifact {
  schema_version: number;
  id: string;
  title: string;
  slug: string;
  workspace: string;
  path: string;
  status: PlanStatus;
  revision: number;
  created_at: string;
  updated_at: string;
  overview: string;
  todos: PlanTodo[];
  agents: PlanAgentReference[];
  is_project: boolean;
  extra?: Record<string, unknown>;
  content: string;
}

export type MentionKind = 'file' | 'doc';

export interface MentionRef {
  kind: MentionKind;
  path: string;
  label?: string;
}

export interface AttachmentRef {
  path: string;
  mime?: string;
  name?: string;
}

export interface UrlRef {
  url: string;
  doc_path?: string;
  title?: string;
}

export interface PromptContext {
  workspace_root?: string;
  active_file?: string;
  selection?: string;
  open_tabs?: string[];
  mentions?: MentionRef[];
  attachments?: AttachmentRef[];
  urls?: UrlRef[];
}

export interface IndexedDoc {
  path: string;
  title: string;
  url?: string;
  fetched_at?: string;
  content_type?: string;
}

export interface FetchDocResult {
  path: string;
  title: string;
  url: string;
  bytes: number;
}

export interface MentionCandidate {
  kind: MentionKind;
  path: string;
  label: string;
  detail?: string;
}
