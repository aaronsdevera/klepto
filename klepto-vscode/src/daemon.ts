import * as vscode from 'vscode';
import { spawn, ChildProcess, execFile } from 'child_process';
import { promisify } from 'util';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  HostExecutableError,
  releaseAssetName,
  resolveHostExecutable,
  verifyReleaseChecksum,
} from './bootstrap';
export { HostExecutableError } from './bootstrap';
import type {
  CreateSessionOptions,
  EffectiveConfig,
  ModelsResponse,
  PlanArtifact,
  PlanTodoStatus,
  ProfilesResponse,
  Session,
} from './types';
import { ShareManager, defaultDataMounts, normalizePath } from './shares';

const execFileAsync = promisify(execFile);

interface MemoryEntry {
  id: string;
  content: string;
  created_at: string;
  workspace?: string;
}

export type DaemonRuntime = 'host' | 'oci' | 'nix';

const RELEASE_REPOSITORY = 'aaronsdevera/klepto';

/**
 * Normalize `klepto.daemon.listen` to `host:port`.
 * Accepts `host:port` or `http(s)://host:port[/path]` (scheme is stripped for the bind/connect host).
 */
export function normalizeListenAddr(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) return '127.0.0.1:7420';

  if (/^https?:\/\//i.test(trimmed)) {
    try {
      const u = new URL(trimmed);
      if (!u.hostname) return '127.0.0.1:7420';
      const port = u.port || (u.protocol === 'https:' ? '443' : '80');
      const host = u.hostname.includes(':') ? `[${u.hostname}]` : u.hostname;
      return `${host}:${port}`;
    } catch {
      // fall through to host:port cleanup
    }
  }

  return trimmed.replace(/\/+$/, '').replace(/\/v1$/i, '');
}

function listenHost(listenAddr: string): string {
  if (listenAddr.startsWith('[')) {
    const end = listenAddr.indexOf(']');
    return end > 0 ? listenAddr.slice(1, end) : listenAddr;
  }
  const idx = listenAddr.lastIndexOf(':');
  return idx > 0 ? listenAddr.slice(0, idx) : listenAddr;
}

/** True when the extension may spawn/manage a local daemon for this address. */
export function isLocalListenAddr(listenAddr: string): boolean {
  const host = listenHost(listenAddr).toLowerCase();
  return (
    host === '127.0.0.1' ||
    host === 'localhost' ||
    host === '::1' ||
    host === '0.0.0.0' ||
    host === '[::]' ||
    host === '::'
  );
}

export class KleptoDaemon {
  private baseURL!: string;
  private listenAddr!: string;
  private useTls = false;
  private ws: WebSocket | null = null;
  private process: ChildProcess | null = null;
  private reconnectTimer: NodeJS.Timeout | null = null;
  private lastEventSeq = new Map<string, number>();
  private ociName = 'klepto';
  private lastOciMountKey = '';
  private shares?: ShareManager;
  private startupPromise: Promise<boolean> | null = null;
  private lastStartError: Error | null = null;
  private processExitError: Error | null = null;

  constructor(listenAddr: string = '127.0.0.1:7420') {
    this.applyListen(listenAddr);
  }

  setShareManager(shares: ShareManager): void {
    this.shares = shares;
  }

  setListenAddr(listenAddr: string): void {
    this.applyListen(listenAddr);
  }

  /** HTTP API base, e.g. `http://127.0.0.1:7420/v1`. */
  getBaseURL(): string {
    return this.baseURL;
  }

  isLocalDaemon(): boolean {
    return isLocalListenAddr(this.listenAddr);
  }

  getLastStartError(): Error | null {
    return this.lastStartError;
  }

  private applyListen(raw: string): void {
    this.useTls = /^https:\/\//i.test(raw.trim());
    this.listenAddr = normalizeListenAddr(raw);
    const scheme = this.useTls ? 'https' : 'http';
    this.baseURL = `${scheme}://${this.listenAddr}/v1`;
  }

  private runtime(): DaemonRuntime {
    const v = vscode.workspace.getConfiguration('klepto').get<string>('daemon.runtime') || 'host';
    if (v === 'oci' || v === 'nix') return v;
    return 'host';
  }

  /** Lightweight reachability check (does not spawn the daemon). */
  async ping(): Promise<boolean> {
    try {
      const response = await this.request('/health', { method: 'GET' });
      return response.ok;
    } catch {
      return false;
    }
  }

  async startOrCheck(opts?: { forceRestart?: boolean }): Promise<boolean> {
    if (this.startupPromise) return this.startupPromise;
    const startup = this.performStartOrCheck(opts).finally(() => {
      if (this.startupPromise === startup) this.startupPromise = null;
    });
    this.startupPromise = startup;
    return startup;
  }

  private async performStartOrCheck(opts?: { forceRestart?: boolean }): Promise<boolean> {
    this.lastStartError = null;
    if (!opts?.forceRestart) {
      try {
        const response = await this.request('/health', { method: 'GET' });
        if (response.ok) {
          // If OCI and shares changed, recreate
          if (
            this.isLocalDaemon() &&
            this.runtime() === 'oci' &&
            this.shares &&
            this.ociMountsChanged()
          ) {
            await this.stop();
          } else {
            return true;
          }
        }
      } catch {
        // not running
      }
    } else if (this.isLocalDaemon()) {
      await this.stop();
    }

    // Remote host: connect only — never spawn a local process with that address.
    if (!this.isLocalDaemon()) {
      try {
        const response = await this.request('/health', { method: 'GET' });
        return response.ok;
      } catch (e) {
        vscode.window.showErrorMessage(
          `Cannot reach Klepto daemon at ${this.baseURL} (${e}). ` +
            `Ensure it is running and listening on 0.0.0.0 (not only 127.0.0.1).`
        );
        return false;
      }
    }

    const runtime = this.runtime();
    try {
      this.processExitError = null;
      if (runtime === 'oci') {
        await this.startOci();
      } else if (runtime === 'nix') {
        await this.startNix();
      } else {
        await this.startHost();
      }
      await this.waitForHealthCheck();
      return true;
    } catch (e) {
      this.lastStartError = e instanceof Error ? e : new Error(String(e));
      if (!(e instanceof HostExecutableError && e.kind === 'not_installed')) {
        vscode.window.showErrorMessage(`Failed to start Klepto daemon (${runtime}): ${e}`);
      }
      return false;
    }
  }

  /** After granting a new folder under OCI, recreate container with mounts. */
  async ensureOciMounts(): Promise<void> {
    if (this.runtime() !== 'oci') return;
    if (!this.ociMountsChanged()) return;
    await this.startOrCheck({ forceRestart: true });
  }

  private ociMountKey(): string {
    const paths = (this.shares?.mountPaths() || []).map(normalizePath).sort();
    return paths.join('\0');
  }

  private ociMountsChanged(): boolean {
    return this.ociMountKey() !== this.lastOciMountKey;
  }

  private async startHost(): Promise<void> {
    const config = vscode.workspace.getConfiguration('klepto');
    const configured = config.get<string>('daemon.path') || '';
    const kleptoPath = resolveHostExecutable(configured);
    await this.spawnDaemon(kleptoPath, ['serve', '--listen', this.listenAddr], 'klepto');
  }

  private async spawnDaemon(bin: string, args: string[], label: string, cwd?: string): Promise<void> {
    const child = spawn(bin, args, {
      cwd,
      detached: false,
      stdio: ['ignore', 'pipe', 'pipe'],
      shell: false,
    });
    this.process = child;
    let stderr = '';
    child.stdout?.on('data', (data) => console.log(`${label}: ${data}`));
    child.stderr?.on('data', (data) => {
      stderr = `${stderr}${String(data)}`.slice(-4000);
      console.error(`${label} error: ${data}`);
    });

    await new Promise<void>((resolve, reject) => {
      let spawned = false;
      child.once('spawn', () => {
        spawned = true;
        resolve();
      });
      child.once('error', (error) => {
        this.process = null;
        reject(new Error(`could not spawn ${bin}: ${error.message}`));
      });
      child.once('exit', (code, signal) => {
        const detail = stderr.trim();
        this.processExitError = new Error(
          `${label} exited before becoming healthy (${signal || `code ${code}`})${
            detail ? `: ${detail}` : ''
          }`
        );
        if (!spawned) reject(this.processExitError);
      });
    });

    child.on('exit', () => {
      this.process = null;
    });
  }

  private async startNix(): Promise<void> {
    const config = vscode.workspace.getConfiguration('klepto');
    const nixCmd =
      config.get<string>('daemon.nix.command') || 'nix run .#klepto --';
    const parts = nixCmd.trim().split(/\s+/).filter(Boolean);
    const bin = parts[0] || 'nix';
    const args = [...parts.slice(1), 'serve', '--listen', this.listenAddr];
    const cwd =
      config.get<string>('daemon.nix.cwd') ||
      vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ||
      undefined;
    await this.spawnDaemon(bin, args, 'klepto-nix', cwd);
  }

  private resolveOciCmd(): string {
    const pref =
      vscode.workspace.getConfiguration('klepto').get<string>('daemon.oci.command') || 'auto';
    if (pref === 'docker' || pref === 'container') return pref;
    // auto: macOS prefers `container`, Linux prefers `docker`
    const isDarwin = process.platform === 'darwin';
    const tryCmd = (bin: string): boolean => {
      try {
        require('child_process').execFileSync(bin, bin === 'docker' ? ['version'] : ['--help'], {
          stdio: 'ignore',
        });
        return true;
      } catch {
        return false;
      }
    };
    if (isDarwin) {
      if (tryCmd('container')) return 'container';
      if (tryCmd('docker')) return 'docker';
    } else {
      if (tryCmd('docker')) return 'docker';
      if (tryCmd('container')) return 'container';
    }
    throw new Error('Neither docker nor macOS container CLI found');
  }

  private async startOci(): Promise<void> {
    const config = vscode.workspace.getConfiguration('klepto');
    const image = config.get<string>('daemon.oci.image') || 'klepto:local';
    const name = config.get<string>('daemon.oci.containerName') || 'klepto';
    this.ociName = name;
    const cmd = this.resolveOciCmd();

    // Remove existing container
    try {
      await execFileAsync(cmd, ['rm', '-f', name]);
    } catch {
      /* ignore */
    }

    const hostPort = this.listenAddr.includes(':')
      ? this.listenAddr.split(':').pop()!
      : '7420';
    const publishHost = '127.0.0.1';
    const networkMode = config.get<string>('network.mode') || 'direct';
    const proxyUrl = (config.get<string>('network.proxyUrl') || '').trim();
    const denyDirect = config.get<boolean>('network.denyDirect', false);
    if (proxyUrl && !proxyUrl.startsWith('socks5h://')) {
      throw new Error('klepto.network.proxyUrl must start with socks5h://');
    }
    if (denyDirect && networkMode !== 'none') {
      throw new Error(
        'denyDirect requires an externally enforced proxy network; use network.mode=none or configure the container runtime network'
      );
    }

    const args: string[] = [];
    if (cmd === 'docker') {
      args.push('run', '-d', '--name', name, '-p', `${publishHost}:${hostPort}:7420`);
      args.push(
        '--cap-drop',
        'ALL',
        '--security-opt',
        'no-new-privileges',
        '--pids-limit',
        '512',
        '--memory',
        config.get<string>('daemon.oci.memoryLimit') || '4g',
        '--cpus',
        config.get<string>('daemon.oci.cpuLimit') || '4',
        '--tmpfs',
        '/tmp:rw,noexec,nosuid,size=512m'
      );
    } else {
      args.push('run', '--name', name, '--detach', '--publish', `${publishHost}:${hostPort}:7420`);
    }
    if (networkMode === 'none') {
      args.push('--network', 'none');
    }
    args.push('-e', 'KLEPTO_LISTEN=0.0.0.0:7420', '-e', 'KLEPTO_IN_OCI=1');
    if (networkMode === 'none') {
      args.push('-e', 'KLEPTO_NETWORK_ENFORCED=1');
    }
    if (proxyUrl) {
      args.push('-e', `ALL_PROXY=${proxyUrl}`, '-e', `all_proxy=${proxyUrl}`);
    }

    // Data / pi mounts
    let mountedKleptoHome = false;
    for (const m of defaultDataMounts()) {
      if (fs.existsSync(m.host)) {
        args.push('-v', `${m.host}:${m.container}`);
        if (m.container === '/home/klepto/.klepto') mountedKleptoHome = true;
      }
    }
    if (!mountedKleptoHome) {
      args.push('-v', `${name}-data:/home/klepto/.klepto`);
    }

    // Same-path workspace shares
    for (const p of this.shares?.mountPaths() || []) {
      if (fs.existsSync(p)) {
        args.push('-v', `${p}:${p}`);
      }
    }

    args.push(image, 'serve', '--listen', '0.0.0.0:7420');

    await execFileAsync(cmd, args);
    this.lastOciMountKey = this.ociMountKey();
  }

  private async waitForHealthCheck(maxRetries = 40): Promise<void> {
    for (let i = 0; i < maxRetries; i++) {
      if (this.processExitError) throw this.processExitError;
      try {
        const response = await this.request('/health', { method: 'GET' });
        if (response.ok) return;
      } catch {
        // continue
      }
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
    throw new Error('Daemon failed to start after retries');
  }

  async installLatestRelease(): Promise<string> {
    const asset = releaseAssetName();
    const base = `https://github.com/${RELEASE_REPOSITORY}/releases/latest/download`;
    const [binaryResponse, checksumResponse] = await Promise.all([
      fetch(`${base}/${asset}`),
      fetch(`${base}/${asset}.sha256`),
    ]);
    if (!binaryResponse.ok) {
      throw new Error(`download ${asset} failed with HTTP ${binaryResponse.status}`);
    }
    if (!checksumResponse.ok) {
      throw new Error(`download ${asset}.sha256 failed with HTTP ${checksumResponse.status}`);
    }

    const binary = Buffer.from(await binaryResponse.arrayBuffer());
    const checksum = await checksumResponse.text();
    if (!verifyReleaseChecksum(binary, checksum)) {
      throw new Error(`checksum verification failed for ${asset}`);
    }

    const installDir = path.join(os.homedir(), '.klepto', 'bin');
    const destination = path.join(installDir, 'klepto');
    const temporary = path.join(
      installDir,
      `.klepto.${process.pid}.${Date.now().toString(36)}.download`
    );
    await fs.promises.mkdir(installDir, { recursive: true, mode: 0o755 });
    try {
      await fs.promises.writeFile(temporary, binary, { mode: 0o755 });
      await fs.promises.chmod(temporary, 0o755);
      await fs.promises.rename(temporary, destination);
    } finally {
      await fs.promises.rm(temporary, { force: true });
    }
    return destination;
  }

  async stop(): Promise<void> {
    if (this.process) {
      this.process.kill();
      this.process = null;
    }
    if (this.runtime() === 'oci') {
      try {
        const cmd = this.resolveOciCmd();
        await execFileAsync(cmd, ['rm', '-f', this.ociName]);
      } catch {
        /* ignore */
      }
      this.lastOciMountKey = '';
    }
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  private async request(endpoint: string, options: RequestInit = {}): Promise<Response> {
    const url = `${this.baseURL}${endpoint}`;
    const token =
      vscode.workspace.getConfiguration('klepto').get<string>('daemon.token')?.trim() || '';
    const defaultOptions: RequestInit = {
      ...options,
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
        ...(options.headers || {}),
      },
    };

    let response: Response;
    try {
      response = await fetch(url, defaultOptions);
    } catch (e) {
      throw new Error(`fetch failed for ${url}: ${e instanceof Error ? e.message : e}`);
    }
    if (!response.ok) {
      let detail = response.statusText;
      try {
        const body = (await response.json()) as { error?: string; message?: string };
        detail = body.error || body.message || detail;
      } catch {
        // ignore parse errors
      }
      throw new Error(`HTTP ${response.status}: ${detail}`);
    }
    return response;
  }

  private async requestJson<T>(endpoint: string, options: RequestInit = {}): Promise<T> {
    const response = await this.request(endpoint, options);
    return (await response.json()) as T;
  }

  async listSessions(): Promise<Session[]> {
    const response = await this.request('/sessions');
    const data = (await response.json()) as { sessions?: Session[] };
    return data.sessions || [];
  }

  async getSession(sessionId: string): Promise<Session | null> {
    try {
      const response = await this.request(`/sessions/${sessionId}`);
      const data = (await response.json()) as { session?: Session };
      return data.session || null;
    } catch {
      return null;
    }
  }

  async createSession(cwd: string, opts: CreateSessionOptions = {}): Promise<Session | null> {
    try {
      const response = await this.request('/sessions', {
        method: 'POST',
        body: JSON.stringify({
          cwd,
          model: opts.model,
          provider: opts.provider,
          agent_mode: opts.agentMode || 'agent',
          profile: opts.profile,
        }),
      });
      const data = (await response.json()) as { session?: Session };
      return data.session || null;
    } catch (e) {
      vscode.window.showErrorMessage(`Failed to create session: ${e}`);
      return null;
    }
  }

  async listModels(options?: { refresh?: boolean }): Promise<ModelsResponse> {
    try {
      const query = options?.refresh ? '?refresh=true' : '';
      return await this.requestJson<ModelsResponse>(`/models${query}`);
    } catch (e) {
      console.error('Failed to list models:', e);
      return { models: [], providers: [], suggested: true, message: String(e) };
    }
  }

  async generateCommitMessage(
    workspace: string,
    diff: string,
    previousMessages: string[]
  ): Promise<string> {
    const data = await this.requestJson<{ message: string }>('/commit-message', {
      method: 'POST',
      body: JSON.stringify({
        workspace,
        diff,
        previous_messages: previousMessages,
      }),
    });
    return data.message;
  }

  async listProfiles(): Promise<ProfilesResponse> {
    return this.requestJson<ProfilesResponse>('/profiles');
  }

  async upsertProvider(input: {
    id: string;
    kind?: 'openai_compatible' | 'ollama';
    base_url?: string;
    api?: string;
    models?: string[];
    api_key?: string;
  }): Promise<void> {
    await this.request('/providers', {
      method: 'POST',
      body: JSON.stringify(input),
    });
  }

  async deleteProvider(id: string): Promise<void> {
    await this.request(`/providers/${encodeURIComponent(id)}`, { method: 'DELETE' });
  }

  async getEffectiveConfig(workspace: string): Promise<EffectiveConfig> {
    const data = await this.requestJson<{ config: EffectiveConfig }>(
      `/config/effective?workspace=${encodeURIComponent(workspace)}`
    );
    return data.config;
  }

  async createPlan(
    workspace: string,
    title: string,
    content?: string,
    authorSessionId?: string
  ): Promise<PlanArtifact> {
    const data = await this.requestJson<{ plan: PlanArtifact }>('/plans', {
      method: 'POST',
      body: JSON.stringify({
        workspace,
        title,
        content,
        author_session_id: authorSessionId,
      }),
    });
    return data.plan;
  }

  async listPlans(workspace: string): Promise<PlanArtifact[]> {
    const data = await this.requestJson<{ plans?: PlanArtifact[] }>(
      `/plans?workspace=${encodeURIComponent(workspace)}`
    );
    return data.plans || [];
  }

  async getPlan(id: string, workspace: string): Promise<PlanArtifact> {
    const data = await this.requestJson<{ plan: PlanArtifact }>(
      `/plans/${encodeURIComponent(id)}?workspace=${encodeURIComponent(workspace)}`
    );
    return data.plan;
  }

  async updatePlan(
    id: string,
    workspace: string,
    update: { content?: string; status?: string }
  ): Promise<PlanArtifact> {
    const data = await this.requestJson<{ plan: PlanArtifact }>(
      `/plans/${encodeURIComponent(id)}`,
      {
        method: 'PUT',
        body: JSON.stringify({ workspace, ...update }),
      }
    );
    return data.plan;
  }

  async updatePlanTodo(
    id: string,
    todoId: string,
    workspace: string,
    status: PlanTodoStatus
  ): Promise<PlanArtifact> {
    const data = await this.requestJson<{ plan: PlanArtifact }>(
      `/plans/${encodeURIComponent(id)}/todos/${encodeURIComponent(todoId)}`,
      {
        method: 'POST',
        body: JSON.stringify({ workspace, status }),
      }
    );
    return data.plan;
  }

  async approvePlan(id: string, workspace: string): Promise<PlanArtifact> {
    const data = await this.requestJson<{ plan: PlanArtifact }>(
      `/plans/${encodeURIComponent(id)}/approve`,
      {
        method: 'POST',
        body: JSON.stringify({ workspace }),
      }
    );
    return data.plan;
  }

  async buildPlan(
    id: string,
    workspace: string
  ): Promise<{ plan: PlanArtifact; session: Session }> {
    return this.requestJson<{ plan: PlanArtifact; session: Session }>(
      `/plans/${encodeURIComponent(id)}/build`,
      {
        method: 'POST',
        body: JSON.stringify({ workspace }),
      }
    );
  }

  async killSession(sessionId: string): Promise<boolean> {
    try {
      await this.request(`/sessions/${sessionId}`, { method: 'DELETE' });
      return true;
    } catch (e) {
      vscode.window.showErrorMessage(`Failed to kill session: ${e}`);
      return false;
    }
  }

  async interruptSession(sessionId: string): Promise<{ ok: boolean; error?: string }> {
    try {
      await this.request(`/sessions/${sessionId}/interrupt`, { method: 'POST' });
      return { ok: true };
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      console.error('Interrupt failed:', error);
      return { ok: false, error };
    }
  }

  async promptSession(
    sessionId: string,
    message: string,
    context?: Record<string, unknown>
  ): Promise<unknown> {
    try {
      const response = await this.request(`/sessions/${sessionId}/prompt`, {
        method: 'POST',
        body: JSON.stringify({
          message,
          context: context || {
            workspace_root: vscode.workspace.workspaceFolders?.[0]?.uri.fsPath,
          },
        }),
      });
      return await response.json();
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      return { ok: false, error };
    }
  }

  async fetchIndexDoc(
    workspace: string,
    url: string
  ): Promise<{ path: string; title: string; url: string; bytes: number } | null> {
    try {
      const data = await this.requestJson<{
        doc?: { path: string; title: string; url: string; bytes: number };
        error?: string;
      }>('/index/docs/fetch', {
        method: 'POST',
        body: JSON.stringify({ workspace, url }),
      });
      return data.doc || null;
    } catch (e) {
      console.error('fetchIndexDoc failed:', e);
      return null;
    }
  }

  async listIndexDocs(
    workspace: string
  ): Promise<Array<{ path: string; title: string; url?: string }>> {
    try {
      const q = encodeURIComponent(workspace);
      const data = await this.requestJson<{
        docs?: Array<{ path: string; title: string; url?: string }>;
      }>(`/index/docs?workspace=${q}`);
      return data.docs || [];
    } catch (e) {
      console.error('listIndexDocs failed:', e);
      return [];
    }
  }

  async subscribeToSession(sessionId: string, onEvent: (event: unknown) => void): Promise<void> {
    if (this.ws) {
      const prev = this.ws;
      this.ws = null;
      prev.onclose = null;
      prev.close();
    }
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }

    const wsScheme = this.useTls ? 'wss' : 'ws';
    const after = this.lastEventSeq.get(sessionId) || 0;
    const token =
      vscode.workspace.getConfiguration('klepto').get<string>('daemon.token')?.trim() || '';
    const auth = token ? `&access_token=${encodeURIComponent(token)}` : '';
    const wsUrl = `${wsScheme}://${this.listenAddr}/v1/sessions/${sessionId}/events?after=${after}${auth}`;
    const ws = new WebSocket(wsUrl);
    this.ws = ws;

    ws.onopen = () => {
      console.log(`WebSocket connected for session ${sessionId}`);
    };

    ws.onmessage = (event) => {
      try {
        const data = JSON.parse(String(event.data));
        if (typeof data?.seq === 'number') {
          const previous = this.lastEventSeq.get(sessionId) || 0;
          if (data.seq <= previous) return;
          this.lastEventSeq.set(sessionId, data.seq);
        }
        onEvent(data);
      } catch (e) {
        console.error('Failed to parse WebSocket message:', e);
      }
    };

    ws.onerror = (error) => {
      console.error(`WebSocket error for session ${sessionId}:`, error);
    };

    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      console.log(`WebSocket closed for session ${sessionId}; reconnecting in 5s`);
      this.reconnectTimer = setTimeout(() => {
        this.reconnectTimer = null;
        this.subscribeToSession(sessionId, onEvent);
      }, 5000);
    };
  }

  async resumeSession(sessionId: string): Promise<{ command?: string } | null> {
    try {
      const response = await fetch(`${this.baseURL}/sessions/${sessionId}/resume`);
      return (await response.json()) as { command?: string };
    } catch {
      return {
        command: `printf '\\n[klepto] RPC session klepto-${sessionId} — type JSON prompts here, or use the Chat panel.\\n\\n'; tmux attach -t klepto-${sessionId}`,
      };
    }
  }

  async searchWorkspace(workspace: string, query: string): Promise<unknown[]> {
    try {
      const response = await this.request('/search', {
        method: 'POST',
        body: JSON.stringify({ workspace, query }),
      });
      const data = (await response.json()) as { hits?: unknown[] };
      return data.hits || [];
    } catch (e) {
      console.error('Search failed:', e);
      return [];
    }
  }

  async indexWorkspace(workspace: string): Promise<unknown> {
    const response = await this.request('/index', {
      method: 'POST',
      body: JSON.stringify({ workspace }),
    });
    const data = (await response.json()) as { index_state?: unknown };
    return data.index_state;
  }

  async removeIndex(workspace: string): Promise<boolean> {
    try {
      await this.request(`/index/${workspace}`, { method: 'DELETE' });
      return true;
    } catch {
      return false;
    }
  }

  async rememberMemory(content: string, workspace?: string): Promise<MemoryEntry | null> {
    try {
      const response = await this.request('/memory', {
        method: 'POST',
        body: JSON.stringify({ content, workspace }),
      });
      const data = (await response.json()) as { entry?: MemoryEntry };
      return data.entry || null;
    } catch (e) {
      vscode.window.showErrorMessage(`Failed to remember: ${e}`);
      return null;
    }
  }

  async recallMemory(query: string): Promise<MemoryEntry[]> {
    try {
      const response = await this.request(`/memory/search/${encodeURIComponent(query)}`);
      const data = (await response.json()) as { entries?: MemoryEntry[] };
      return data.entries || [];
    } catch {
      return [];
    }
  }

  async listMemory(): Promise<MemoryEntry[]> {
    try {
      const response = await this.request('/memory');
      const data = (await response.json()) as { entries?: MemoryEntry[] };
      return data.entries || [];
    } catch {
      return [];
    }
  }

  async forgetMemory(id: string): Promise<boolean> {
    try {
      await this.request(`/memory/${id}`, { method: 'DELETE' });
      return true;
    } catch {
      return false;
    }
  }
}
