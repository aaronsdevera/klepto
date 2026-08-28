import * as vscode from 'vscode';
import * as path from 'path';
import { KleptoDaemon } from './daemon';
import type {
  AgentMode,
  AttachmentRef,
  CreateSessionOptions,
  MentionCandidate,
  MentionRef,
  ModelsResponse,
  PromptContext,
  UrlRef,
} from './types';
import type { ShareManager } from './shares';
import { filterModelsByAllowlist } from './providers';

type WebviewMessage = {
  type: string;
  message?: string;
  sessionId?: string;
  thenSend?: string;
  text?: string;
  format?: 'md' | 'json';
  title?: string;
  content?: string;
  agentMode?: AgentMode;
  profile?: string;
  provider?: string;
  model?: string;
  url?: string;
  requestId?: string;
  query?: string;
  mentions?: MentionRef[];
  attachments?: AttachmentRef[];
  urls?: UrlRef[];
  name?: string;
  mime?: string;
  dataBase64?: string;
  path?: string;
  target?: string;
  planId?: string;
  messageId?: string;
  generating?: boolean;
};

export class ChatViewProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = 'klepto.chat';

  private view?: vscode.WebviewView;
  private currentSessionId: string | null = null;
  private prefs: CreateSessionOptions = { agentMode: 'agent' };
  private connPollTimer?: NodeJS.Timeout;
  private modelRefreshTimer?: NodeJS.Timeout;
  private sessionRefreshTimer?: NodeJS.Timeout;
  private lastConnected?: boolean;
  private availableModels = new Set<string>();

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly daemon: KleptoDaemon,
    private readonly shares: ShareManager
  ) {
    const cfg = vscode.workspace.getConfiguration('klepto');
    const agentMode = (cfg.get<string>('defaultMode') as AgentMode) || 'agent';
    this.prefs = {
      agentMode,
      profile: this.profileForMode(agentMode),
      provider: cfg.get<string>('defaultProvider') || undefined,
      model: cfg.get<string>('defaultModel') || undefined,
    };
    if (!this.prefs.provider) delete this.prefs.provider;
    if (!this.prefs.model) delete this.prefs.model;
  }

  private profileForMode(mode: AgentMode): string {
    return mode === 'agent' ? 'coding' : mode;
  }

  resolveWebviewView(
    webviewView: vscode.WebviewView,
    _context: vscode.WebviewViewResolveContext,
    _token: vscode.CancellationToken
  ): void {
    this.view = webviewView;
    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, 'media')],
    };
    webviewView.webview.html = this.getHtmlForWebview(webviewView.webview);
    webviewView.webview.onDidReceiveMessage(async (message: WebviewMessage) => {
      await this.handleWebviewMessage(message);
    });
    webviewView.onDidDispose(() => {
      if (this.connPollTimer) {
        clearInterval(this.connPollTimer);
        this.connPollTimer = undefined;
      }
      if (this.modelRefreshTimer) {
        clearInterval(this.modelRefreshTimer);
        this.modelRefreshTimer = undefined;
      }
      if (this.sessionRefreshTimer) {
        clearInterval(this.sessionRefreshTimer);
        this.sessionRefreshTimer = undefined;
      }
      void vscode.commands.executeCommand('setContext', 'klepto.generating', false);
      if (this.view === webviewView) this.view = undefined;
    });
    this.startConnectionPolling();
    this.modelRefreshTimer = setInterval(() => {
      void this.daemon.ping().then((connected) => {
        if (connected) void this.pushModels();
      });
    }, 30_000);
    this.sessionRefreshTimer = setInterval(() => {
      void this.refreshSessions();
    }, 15_000);
  }

  private startConnectionPolling(): void {
    if (this.connPollTimer) clearInterval(this.connPollTimer);
    this.lastConnected = undefined;
    void this.pushConnectionStatus();
    this.connPollTimer = setInterval(() => {
      void this.pushConnectionStatus();
    }, 4000);
  }

  private async pushConnectionStatus(): Promise<void> {
    if (!this.view) return;
    const connected = await this.daemon.ping();
    if (this.lastConnected === connected) return;
    this.lastConnected = connected;
    this.view.webview.postMessage({ type: 'connectionStatus', connected });
  }

  async focus(): Promise<void> {
    await vscode.commands.executeCommand('workbench.view.extension.klepto');
    await vscode.commands.executeCommand(`${ChatViewProvider.viewType}.focus`);
    this.view?.show?.(true);
    this.view?.webview.postMessage({ type: 'focusComposer' });
  }

  /** Create a new chat tab (same as the + control in the webview). */
  async newChatTab(): Promise<void> {
    await this.focus();
    const cwd = this.workspaceCwd();
    const session = await this.ensureSession(cwd);
    if (!session) return;
    this.currentSessionId = session.id;
    this.view?.webview.postMessage({ type: 'newSession', session });
  }

  async stopCurrentSession(sessionId?: string): Promise<void> {
    const id = sessionId || this.currentSessionId;
    if (!id) {
      this.view?.webview.postMessage({
        type: 'stopResult',
        ok: false,
        error: 'No active Klepto session',
      });
      return;
    }
    const result = await this.daemon.interruptSession(id);
    this.view?.webview.postMessage({
      type: 'stopResult',
      sessionId: id,
      ...result,
    });
  }

  async requestStopCurrentSession(): Promise<void> {
    if (this.view) {
      await this.view.webview.postMessage({ type: 'requestStop' });
      return;
    }
    await this.stopCurrentSession();
  }

  async refreshSessions(): Promise<void> {
    if (!this.view) return;
    try {
      const sessions = await this.daemon.listSessions();
      this.view.webview.postMessage({ type: 'sessionsUpdate', sessions });
    } catch (e) {
      console.error('Failed to refresh sessions:', e);
    }
  }

  /** Refresh provider/model pickers from the daemon (applies includedModels filter). */
  async refreshModels(): Promise<void> {
    await this.pushModels();
  }

  async createPlanFromChat(): Promise<void> {
    await this.focus();
    this.view?.webview.postMessage({ type: 'requestCreatePlan' });
  }

  async openLatestPlan(): Promise<void> {
    const plans = await this.daemon.listPlans(this.workspaceCwd());
    const latest = plans.sort((a, b) => b.updated_at.localeCompare(a.updated_at))[0];
    if (!latest) {
      vscode.window.showInformationMessage('No saved Klepto plans in this workspace');
      return;
    }
    await this.openPlan(latest.path);
  }

  async openPlan(planPath: string): Promise<void> {
    await vscode.commands.executeCommand(
      'vscode.openWith',
      vscode.Uri.file(planPath),
      'klepto.planPreview'
    );
  }

  async buildPlan(planId?: string, workspace = this.workspaceCwd()): Promise<void> {
    let id = planId;
    if (!id) {
      const plans = await this.daemon.listPlans(workspace);
      const selected = await vscode.window.showQuickPick(
        plans
          .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
          .map((plan) => ({
            label: plan.title,
            description: `${plan.status} · revision ${plan.revision}`,
            detail: plan.path,
            plan,
          })),
        { title: 'Build Klepto Plan', placeHolder: 'Select a saved plan' }
      );
      if (!selected) return;
      id = selected.plan.id;
    }
    const result = await this.daemon.buildPlan(id, workspace);
    await this.activateBuiltSession(result.session);
    this.view?.webview.postMessage({ type: 'planUpdated', plan: result.plan });
  }

  private async activateBuiltSession(session: import('./types').Session): Promise<void> {
    this.currentSessionId = session.id;
    await this.daemon.subscribeToSession(session.id, (event) => {
      this.forwardSessionEvent(session.id, event);
    });
    await this.focus();
    this.view?.webview.postMessage({ type: 'newSession', session });
  }

  /** Full model catalog from the daemon (unfiltered) for Manage Included Models. */
  async listModelsCatalog(): Promise<ModelsResponse> {
    return this.daemon.listModels();
  }

  async createAndPromptSession(cwd: string, message: string): Promise<void> {
    await this.focus();
    const session = await this.ensureSession(cwd);
    if (!session) return;
    this.view?.webview.postMessage({ type: 'userMessage', message });
    await this.prompt(session.id, message, cwd);
  }

  private workspaceCwd(): string {
    return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath || process.cwd();
  }

  private async ensureFolderAccess(cwd: string): Promise<boolean> {
    const allowed = await this.shares.ensureAccess(cwd);
    if (!allowed) {
      vscode.window.showErrorMessage(
        `Klepto access denied for ${cwd}. Session not created.`
      );
      this.view?.webview.postMessage({
        type: 'systemMessage',
        message: `Access denied for ${cwd}`,
      });
      return false;
    }
    await this.daemon.ensureOciMounts();
    return true;
  }

  private async ensureSession(cwd: string): Promise<{ id: string } | null> {
    if (!(await this.ensureFolderAccess(cwd))) {
      return null;
    }
    const session = await this.daemon.createSession(cwd, this.prefs);
    if (!session) return null;
    if (session.status && session.status !== 'Running') {
      vscode.window.showErrorMessage(
        `Klepto session ${session.id} failed to start a live tmux harness (status=${session.status}). Check that omp and tmux are on PATH, then restart the Klepto daemon.`
      );
      return null;
    }
    this.currentSessionId = session.id;
    await this.daemon.subscribeToSession(session.id, (event) => {
      this.forwardSessionEvent(session.id, event);
    });
    return session;
  }

  private forwardSessionEvent(sessionId: string, event: unknown): void {
    if (
      event &&
      typeof event === 'object' &&
      (event as { type?: string }).type === 'connected'
    ) {
      return;
    }
    this.view?.webview.postMessage({
      type: 'sessionEvent',
      sessionId,
      event,
    });
  }

  /** Reuse a live session id, or create a new one if missing after daemon restart. */
  private async ensureLiveSession(
    cwd: string,
    preferredId?: string | null
  ): Promise<{ id: string } | null> {
    if (preferredId) {
      const existing = await this.daemon.getSession(preferredId);
      const modelAvailable =
        !existing?.model ||
        this.availableModels.size === 0 ||
        this.availableModels.has(existing.model);
      if (existing?.status === 'Running' && modelAvailable) {
        this.currentSessionId = existing.id;
        await this.daemon.subscribeToSession(existing.id, (event) => {
          this.forwardSessionEvent(existing.id, event);
        });
        return existing;
      }
      if (existing?.model && !modelAvailable) {
        this.view?.webview.postMessage({
          type: 'systemMessage',
          message: `Starting a new session because ${existing.model} is no longer available.`,
        });
      }
      this.currentSessionId = null;
    }
    return this.ensureSession(cwd);
  }

  private buildEditorContext(
    cwd: string,
    extra?: {
      mentions?: MentionRef[];
      attachments?: AttachmentRef[];
      urls?: UrlRef[];
    }
  ): PromptContext {
    const editor = vscode.window.activeTextEditor;
    return {
      workspace_root: cwd,
      active_file: editor?.document.uri.fsPath,
      selection: editor?.document.getText(editor.selection) || undefined,
      open_tabs: vscode.window.tabGroups.all
        .flatMap((g) => g.tabs)
        .map((t) => (t.input as { uri?: vscode.Uri })?.uri?.fsPath)
        .filter(Boolean)
        .slice(0, 12) as string[],
      mentions: extra?.mentions,
      attachments: extra?.attachments,
      urls: extra?.urls,
    };
  }

  private async prompt(
    sessionId: string,
    message: string,
    cwd: string,
    extra?: {
      mentions?: MentionRef[];
      attachments?: AttachmentRef[];
      urls?: UrlRef[];
    }
  ): Promise<void> {
    this.view?.webview.postMessage({
      type: 'thinkingDelta',
      text: 'Planning response…\n',
    });

    const context = this.buildEditorContext(cwd, extra);
    const result = await this.daemon.promptSession(
      sessionId,
      message,
      context as unknown as Record<string, unknown>
    );
    if (result && typeof result === 'object' && (result as { ok?: boolean }).ok) {
      this.view?.webview.postMessage({
        type: 'promptAccepted',
        sessionId,
      });
      return;
    }
    const error =
      result && typeof result === 'object' && 'error' in result
        ? String((result as { error?: unknown }).error || 'Prompt failed')
        : undefined;
    if (error) {
      this.view?.webview.postMessage({
        type: 'promptError',
        sessionId,
        message: error,
      });
      return;
    }
    this.view?.webview.postMessage({
      type: 'promptResponse',
      sessionId,
      response: result,
    });
  }

  private async pushModels(): Promise<void> {
    const models = filterModelsByAllowlist(await this.daemon.listModels());
    this.availableModels = new Set(models.models.map((model) => model.label));
    this.view?.webview.postMessage({
      type: 'modelsUpdate',
      ...models,
      prefs: this.prefs,
    });
    try {
      const { profiles } = await this.daemon.listProfiles();
      this.view?.webview.postMessage({
        type: 'profilesUpdate',
        profiles,
        selected: this.prefs.profile,
      });
    } catch (e) {
      console.error('Failed to list profiles:', e);
    }
  }

  private async ensureKleptoDirs(cwd: string): Promise<vscode.Uri> {
    const root = vscode.Uri.file(cwd);
    const klepto = vscode.Uri.joinPath(root, '.klepto');
    const uploads = vscode.Uri.joinPath(klepto, 'uploads');
    const docs = vscode.Uri.joinPath(klepto, 'index', 'docs');
    await vscode.workspace.fs.createDirectory(docs);
    await vscode.workspace.fs.createDirectory(uploads);
    const gitignore = vscode.Uri.joinPath(klepto, '.gitignore');
    try {
      await vscode.workspace.fs.stat(gitignore);
    } catch {
      await vscode.workspace.fs.writeFile(gitignore, Buffer.from('*\n!.gitignore\n', 'utf8'));
    }
    return uploads;
  }

  private async handleOpenFile(rawPath?: string): Promise<void> {
    const p = (rawPath || '').trim();
    if (!p) return;
    try {
      const uri = path.isAbsolute(p)
        ? vscode.Uri.file(p)
        : vscode.Uri.joinPath(vscode.Uri.file(this.workspaceCwd()), p);
      const doc = await vscode.workspace.openTextDocument(uri);
      await vscode.window.showTextDocument(doc, { preview: true, preserveFocus: false });
    } catch (e) {
      vscode.window.showErrorMessage(
        `Could not open ${p}: ${e instanceof Error ? e.message : e}`
      );
    }
  }

  private async handleFetchUrl(message: WebviewMessage): Promise<void> {
    const url = message.url?.trim();
    const requestId = message.requestId;
    if (!url || !requestId) return;
    const cwd = this.workspaceCwd();
    const doc = await this.daemon.fetchIndexDoc(cwd, url);
    if (doc) {
      this.view?.webview.postMessage({
        type: 'fetchUrlResult',
        requestId,
        ok: true,
        url: doc.url,
        path: doc.path,
        title: doc.title,
        bytes: doc.bytes,
      });
    } else {
      this.view?.webview.postMessage({
        type: 'fetchUrlResult',
        requestId,
        ok: false,
        url,
        error: 'Failed to fetch URL into workspace index',
      });
    }
  }

  private async handleSearchMentions(message: WebviewMessage): Promise<void> {
    const requestId = message.requestId;
    if (!requestId) return;
    const query = (message.query || '').trim().toLowerCase();
    const cwd = this.workspaceCwd();
    const candidates: MentionCandidate[] = [];
    const seen = new Set<string>();

    const push = (c: MentionCandidate) => {
      if (seen.has(c.path)) return;
      seen.add(c.path);
      candidates.push(c);
    };

    const editor = vscode.window.activeTextEditor;
    if (editor) {
      const p = editor.document.uri.fsPath;
      push({
        kind: 'file',
        path: p,
        label: path.basename(p),
        detail: 'Active file',
      });
    }

    for (const tab of vscode.window.tabGroups.all.flatMap((g) => g.tabs)) {
      const uri = (tab.input as { uri?: vscode.Uri })?.uri;
      if (!uri || uri.scheme !== 'file') continue;
      push({
        kind: 'file',
        path: uri.fsPath,
        label: path.basename(uri.fsPath),
        detail: 'Open tab',
      });
    }

    const docs = await this.daemon.listIndexDocs(cwd);
    for (const d of docs) {
      push({
        kind: 'doc',
        path: d.path,
        label: d.title,
        detail: d.url || 'Indexed doc',
      });
    }

    const glob = query
      ? `**/*${query}*`
      : '**/*.{ts,tsx,js,jsx,rs,py,md,json,toml,go,java,css,html}';
    try {
      const files = await vscode.workspace.findFiles(
        glob,
        '**/{node_modules,target,.git,.klepto}/**',
        40
      );
      for (const f of files) {
        push({
          kind: 'file',
          path: f.fsPath,
          label: path.basename(f.fsPath),
          detail: vscode.workspace.asRelativePath(f),
        });
      }
    } catch (e) {
      console.error('findFiles failed:', e);
    }

    let filtered = candidates;
    if (query) {
      filtered = candidates.filter(
        (c) =>
          c.label.toLowerCase().includes(query) ||
          c.path.toLowerCase().includes(query) ||
          (c.detail || '').toLowerCase().includes(query)
      );
    }

    this.view?.webview.postMessage({
      type: 'searchMentionsResult',
      requestId,
      candidates: filtered.slice(0, 30),
    });
  }

  private async handleAttachFiles(message: WebviewMessage): Promise<void> {
    const requestId = message.requestId;
    const cwd = this.workspaceCwd();
    const sessionId = message.sessionId || this.currentSessionId || 'pending';
    const uploadsRoot = await this.ensureKleptoDirs(cwd);
    const sessionDir = vscode.Uri.joinPath(uploadsRoot, sessionId);
    await vscode.workspace.fs.createDirectory(sessionDir);

    const attachments: AttachmentRef[] = [];

    if (message.dataBase64 && message.name) {
      const buf = Buffer.from(message.dataBase64, 'base64');
      const safeName = path.basename(message.name).replace(/[^\w.\-]+/g, '_');
      const dest = vscode.Uri.joinPath(sessionDir, safeName);
      await vscode.workspace.fs.writeFile(dest, buf);
      attachments.push({
        path: dest.fsPath,
        name: safeName,
        mime: message.mime,
      });
    } else {
      const uris = await vscode.window.showOpenDialog({
        canSelectMany: true,
        openLabel: 'Attach',
        filters: {
          'All files': ['*'],
          Images: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg'],
        },
      });
      if (!uris?.length) {
        this.view?.webview.postMessage({
          type: 'attachFilesResult',
          requestId,
          attachments: [],
        });
        return;
      }
      for (const uri of uris) {
        const base = path.basename(uri.fsPath);
        const dest = vscode.Uri.joinPath(sessionDir, base);
        await vscode.workspace.fs.copy(uri, dest, { overwrite: true });
        attachments.push({ path: dest.fsPath, name: base });
      }
    }

    this.view?.webview.postMessage({
      type: 'attachFilesResult',
      requestId,
      attachments,
    });
  }

  private async handleWebviewMessage(message: WebviewMessage): Promise<void> {
    switch (message.type) {
      case 'ready':
        this.lastConnected = undefined;
        await this.pushConnectionStatus();
        this.view?.webview.postMessage({
          type: 'workspaceInfo',
          root: this.workspaceCwd(),
        });
        await this.refreshSessions();
        await this.pushModels();
        break;

      case 'openFile':
        await this.handleOpenFile(message.path);
        break;

      case 'openMarkdownLink': {
        const target = message.target?.trim();
        if (!target) break;
        if (/^https?:\/\//i.test(target)) {
          await vscode.env.openExternal(vscode.Uri.parse(target));
        } else if (target.startsWith('file://')) {
          await this.handleOpenFile(vscode.Uri.parse(target).fsPath);
        } else {
          await this.handleOpenFile(target);
        }
        break;
      }

      case 'setPrefs': {
        const agentMode = message.agentMode || this.prefs.agentMode || 'agent';
        this.prefs = {
          agentMode,
          profile: message.profile || this.prefs.profile || this.profileForMode(agentMode),
          provider: message.provider || undefined,
          model: message.model || undefined,
        };
        if (!this.prefs.provider) delete this.prefs.provider;
        if (!this.prefs.model) delete this.prefs.model;
        break;
      }

      case 'savePlan': {
        const content = message.content?.trim();
        if (!content) return;
        try {
          const title =
            message.title?.trim() ||
            `Plan ${new Date().toISOString().replace('T', ' ').slice(0, 19)}`;
          const plan = await this.daemon.createPlan(
            this.workspaceCwd(),
            title,
            content,
            message.sessionId
          );
          this.view?.webview.postMessage({
            type: 'planSaved',
            plan,
            messageId: message.messageId,
          });
          await this.openPlan(plan.path);
        } catch (e) {
          this.view?.webview.postMessage({
            type: 'planSaveFailed',
            messageId: message.messageId,
            error: `Failed to save plan: ${e}`,
          });
          vscode.window.showErrorMessage(`Failed to save plan: ${e}`);
        }
        break;
      }

      case 'openPlan':
        if (message.path) await this.openPlan(message.path);
        break;

      case 'approvePlan':
        if (message.planId) {
          try {
            const plan = await this.daemon.approvePlan(
              message.planId,
              this.workspaceCwd()
            );
            this.view?.webview.postMessage({ type: 'planUpdated', plan });
          } catch (e) {
            vscode.window.showErrorMessage(`Failed to approve plan: ${e}`);
          }
        }
        break;

      case 'buildPlan':
        if (message.planId) {
          try {
            await this.buildPlan(message.planId);
          } catch (e) {
            vscode.window.showErrorMessage(`Failed to build plan: ${e}`);
          }
        }
        break;

      case 'revisePlan':
        if (message.planId) {
          try {
            const plan = await this.daemon.updatePlan(
              message.planId,
              this.workspaceCwd(),
              { status: 'draft' }
            );
            this.view?.webview.postMessage({ type: 'planUpdated', plan });
            await vscode.commands.executeCommand(
              'vscode.openWith',
              vscode.Uri.file(plan.path),
              'default'
            );
          } catch (e) {
            vscode.window.showErrorMessage(`Failed to revise plan: ${e}`);
          }
        }
        break;

      case 'refreshModels':
        await this.pushModels();
        break;

      case 'fetchUrl':
        await this.handleFetchUrl(message);
        break;

      case 'searchMentions':
        await this.handleSearchMentions(message);
        break;

      case 'attachFiles':
        await this.handleAttachFiles(message);
        break;

      case 'sendMessage': {
        const text = message.message?.trim();
        if (!text) return;
        const cwd = this.workspaceCwd();
        const preferredId = message.sessionId || this.currentSessionId;
        const session = await this.ensureLiveSession(cwd, preferredId);
        if (!session) return;
        if (session.id !== preferredId) {
          this.view?.webview.postMessage({ type: 'newSession', session });
        }
        this.currentSessionId = session.id;
        await this.prompt(session.id, text, cwd, {
          mentions: message.mentions,
          attachments: message.attachments,
          urls: message.urls,
        });
        break;
      }

      case 'createSession': {
        const cwd = this.workspaceCwd();
        const session = await this.ensureSession(cwd);
        if (!session) return;
        this.view?.webview.postMessage({
          type: 'newSession',
          session,
          thenSend: message.thenSend,
        });
        break;
      }

      case 'switchSession':
        this.currentSessionId = message.sessionId ?? null;
        if (this.currentSessionId) {
          const sessionId = this.currentSessionId;
          await this.daemon.subscribeToSession(sessionId, (event) => {
            this.forwardSessionEvent(sessionId, event);
          });
        }
        break;

      case 'closeSession':
        if (message.sessionId) {
          await this.daemon.killSession(message.sessionId);
          if (this.currentSessionId === message.sessionId) {
            this.currentSessionId = null;
          }
        }
        break;

      case 'stopSession':
        if (message.sessionId || this.currentSessionId) {
          await this.stopCurrentSession(message.sessionId || this.currentSessionId!);
        }
        break;

      case 'generationState':
        await vscode.commands.executeCommand(
          'setContext',
          'klepto.generating',
          !!message.generating
        );
        break;

      case 'copyText':
        if (message.text) {
          await vscode.env.clipboard.writeText(message.text);
          vscode.window.setStatusBarMessage('Copied to clipboard', 1500);
        }
        break;

      case 'exportTranscript': {
        if (!message.content) return;
        const ext = message.format === 'json' ? 'json' : 'md';
        const defaultName = `klepto-${(message.title || message.sessionId || 'chat')
          .replace(/[^\w.-]+/g, '-')
          .slice(0, 40)}.${ext}`;
        const uri = await vscode.window.showSaveDialog({
          defaultUri: vscode.Uri.joinPath(
            vscode.workspace.workspaceFolders?.[0]?.uri ?? this.extensionUri,
            defaultName
          ),
          filters:
            ext === 'json'
              ? { JSON: ['json'] }
              : { Markdown: ['md'], Text: ['txt'] },
        });
        if (!uri) return;
        await vscode.workspace.fs.writeFile(uri, Buffer.from(message.content, 'utf8'));
        vscode.window.showInformationMessage(`Exported transcript to ${uri.fsPath}`);
        break;
      }
    }
  }

  private getHtmlForWebview(webview: vscode.Webview): string {
    const styleUri = webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, 'media', 'chat.css')
    );
    const scriptUri = webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, 'media', 'chat.js')
    );
    const markdownUri = webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, 'media', 'markdown.js')
    );
    const nonce = getNonce();

    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource}; script-src 'nonce-${nonce}';">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <link href="${styleUri}" rel="stylesheet">
  <title>Klepto</title>
</head>
<body>
  <div class="shell">
    <div class="tabs-bar">
      <div class="tabs" id="tabs" role="tablist"></div>
      <div class="tabs-actions">
        <button class="icon-btn" id="newSession" title="New chat (⌘N / Ctrl+N)">+</button>
        <button class="icon-btn" id="moreBtn" title="More">⋯</button>
      </div>
      <div class="menu" id="moreMenu">
        <button data-action="export-md">Export transcript (.md)</button>
        <button data-action="export-json">Export transcript (.json)</button>
        <button data-action="copy-all">Copy all messages</button>
        <button data-action="clear">Clear chat</button>
        <button data-action="refresh-models">Refresh models</button>
      </div>
    </div>

    <div id="messages" class="messages">
      <div class="empty" id="emptyState">
        <h2>Klepto</h2>
        <p>Ask anything about this workspace. ⌘L opens chat. New tabs keep sessions separate.</p>
      </div>
    </div>

    <div class="composer">
      <div class="composer-box">
        <div class="composer-toolbar">
          <div class="mode-toggle" id="modeToggle" role="group" aria-label="Mode">
            <button type="button" class="mode-btn active" data-mode="agent" title="Agent — full tools" aria-label="Agent">
              <svg viewBox="0 0 16 16" fill="none" aria-hidden="true">
                <path d="M8 1.5l1.2 3.3L12.5 6 9.2 7.2 8 10.5 6.8 7.2 3.5 6l3.3-1.2L8 1.5z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
                <path d="M12.5 10.2l.6 1.6 1.6.6-1.6.6-.6 1.6-.6-1.6-1.6-.6 1.6-.6.6-1.6z" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"/>
              </svg>
            </button>
            <button type="button" class="mode-btn" data-mode="plan" title="Plan — read-only planning" aria-label="Plan">
              <svg viewBox="0 0 16 16" fill="none" aria-hidden="true">
                <path d="M3.5 3.5h9M3.5 8h9M3.5 12.5h6" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
              </svg>
            </button>
            <button type="button" class="mode-btn" data-mode="debug" title="Debug — focused debugging" aria-label="Debug">
              <svg viewBox="0 0 16 16" fill="none" aria-hidden="true">
                <path d="M5.5 6.5h5v5.5a2.5 2.5 0 01-5 0V6.5z" stroke="currentColor" stroke-width="1.2"/>
                <path d="M8 3.5v2M4 8H2.5M13.5 8H12M4.2 4.2L3 3M11.8 4.2L13 3M4.2 12.8L3 14M11.8 12.8L13 14" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
              </svg>
            </button>
          </div>
          <div class="picker-row">
            <label class="picker picker-profile" title="Profile">
              <span class="picker-label">Profile</span>
              <select id="profileSelect" title="Profile" aria-label="Profile">
                <option value="coding">coding</option>
                <option value="plan">plan</option>
                <option value="debug">debug</option>
              </select>
            </label>
            <label class="picker" title="Provider">
              <span class="picker-label">Provider</span>
              <select id="providerSelect" title="Provider" aria-label="Provider">
                <option value="">Provider</option>
              </select>
            </label>
            <label class="picker picker-model" title="Model">
              <span class="picker-label">Model</span>
              <select id="modelSelect" title="Model" aria-label="Model">
                <option value="">Model</option>
              </select>
            </label>
          </div>
        </div>
        <div class="attach-row" id="attachRow" hidden></div>
        <div
          id="composerInput"
          class="composer-input"
          contenteditable="true"
          role="textbox"
          aria-multiline="true"
          aria-label="Message"
          data-placeholder="Ask Klepto… (@ for context, drag & drop files)"
        ></div>
        <div id="dropZone" class="drop-zone" aria-hidden="true">
          <svg width="22" height="22" viewBox="0 0 16 16" fill="none" aria-hidden="true">
            <path d="M8 2v8m0-8L5 5M8 2l3 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
            <path d="M2.5 9.5v2a2 2 0 002 2h7a2 2 0 002-2v-2" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
          <span>Drop files or images here</span>
        </div>
        <div class="mention-menu" id="mentionMenu" hidden></div>
        <div class="composer-footer">
          <span class="hint" id="prefsHint" aria-hidden="true"></span>
          <div class="composer-btns">
            <span
              class="conn-light is-unknown"
              id="connLight"
              role="status"
              aria-live="polite"
              title="Checking daemon…"
              aria-label="Daemon connection"
            ><span class="conn-dot" aria-hidden="true"></span></span>
            <button class="icon-btn attach-btn" id="attachBtn" type="button" title="Attach files">
              <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
                <path d="M14.5 7.5l-6.07 6.07a3.5 3.5 0 01-4.95-4.95L9.55 2.55a2.25 2.25 0 013.18 3.18L6.3 12.16a1 1 0 11-1.41-1.41l5.65-5.66" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
            </button>
            <button class="btn btn-primary" id="sendMessage" title="Send (Enter)" aria-label="Send">
              <svg class="icon-send" viewBox="0 0 16 16" fill="none" aria-hidden="true">
                <path d="M8 12.5V3.5M8 3.5L4.5 7M8 3.5L11.5 7" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
              <svg class="icon-stop" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
                <rect x="4.5" y="4.5" width="7" height="7" rx="1"/>
              </svg>
            </button>
          </div>
        </div>
      </div>
      <div class="status-pill" id="status"></div>
    </div>
  </div>
  <script nonce="${nonce}" src="${markdownUri}"></script>
  <script nonce="${nonce}" src="${scriptUri}"></script>
</body>
</html>`;
  }
}

function getNonce(): string {
  const possible = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let text = '';
  for (let i = 0; i < 32; i++) {
    text += possible.charAt(Math.floor(Math.random() * possible.length));
  }
  return text;
}
