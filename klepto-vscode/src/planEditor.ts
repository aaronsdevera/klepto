import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import { randomBytes } from 'crypto';
import { KleptoDaemon } from './daemon';
import type { PlanArtifact, PlanTodoStatus, Session } from './types';

export class PlanEditorProvider implements vscode.CustomTextEditorProvider {
  static readonly viewType = 'klepto.planPreview';

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly daemon: KleptoDaemon,
    private readonly buildPlan: (id: string, workspace: string) => Promise<void>
  ) {}

  async resolveCustomTextEditor(
    document: vscode.TextDocument,
    panel: vscode.WebviewPanel,
    _token: vscode.CancellationToken
  ): Promise<void> {
    const workspace =
      vscode.workspace.getWorkspaceFolder(document.uri)?.uri.fsPath ||
      this.workspaceFromPlanPath(document.uri.fsPath);
    const id = path.basename(document.uri.fsPath, path.extname(document.uri.fsPath));
    panel.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, 'media')],
    };
    panel.webview.html = await this.html(panel.webview);

    const refresh = async (): Promise<void> => {
      try {
        const [plan, sessions] = await Promise.all([
          this.daemon.getPlan(id, workspace),
          this.daemon.listSessions().catch(() => [] as Session[]),
        ]);
        await panel.webview.postMessage({ type: 'plan', plan, sessions });
      } catch (error) {
        await panel.webview.postMessage({ type: 'error', error: String(error) });
      }
    };

    const watcher = vscode.workspace.createFileSystemWatcher(
      new vscode.RelativePattern(path.dirname(document.uri.fsPath), path.basename(document.uri.fsPath))
    );
    const disposables = [
      watcher,
      watcher.onDidChange(() => void refresh()),
      watcher.onDidCreate(() => void refresh()),
      vscode.workspace.onDidChangeTextDocument((event) => {
        if (event.document.uri.toString() === document.uri.toString()) void refresh();
      }),
      panel.webview.onDidReceiveMessage(async (message) => {
        try {
          switch (message.type) {
            case 'ready':
            case 'refresh':
              await refresh();
              break;
            case 'updateTodo': {
              const plan = await this.daemon.updatePlanTodo(
                id,
                String(message.todoId),
                workspace,
                message.status as PlanTodoStatus
              );
              await this.postPlan(panel.webview, plan);
              break;
            }
            case 'build':
              await panel.webview.postMessage({ type: 'building', value: true });
              await this.buildPlan(id, workspace);
              await refresh();
              break;
            case 'openSource':
              await vscode.commands.executeCommand('vscode.openWith', document.uri, 'default');
              break;
            case 'openFile':
              await this.openWorkspaceFile(String(message.path || ''));
              break;
          }
        } catch (error) {
          await panel.webview.postMessage({ type: 'error', error: String(error) });
        } finally {
          if (message.type === 'build') {
            await panel.webview.postMessage({ type: 'building', value: false });
          }
        }
      }),
    ];
    panel.onDidDispose(() => disposables.forEach((disposable) => disposable.dispose()));
  }

  private async postPlan(webview: vscode.Webview, plan: PlanArtifact): Promise<void> {
    const sessions = await this.daemon.listSessions().catch(() => [] as Session[]);
    await webview.postMessage({ type: 'plan', plan, sessions });
  }

  private workspaceFromPlanPath(planPath: string): string {
    const marker = `${path.sep}.klepto${path.sep}plans${path.sep}`;
    const index = planPath.lastIndexOf(marker);
    if (index < 0) throw new Error(`Plan is not inside a workspace: ${planPath}`);
    return planPath.slice(0, index);
  }

  private async openWorkspaceFile(raw: string): Promise<void> {
    const candidate = path.resolve(raw);
    const allowed = (vscode.workspace.workspaceFolders || []).some((folder) => {
      const root = path.resolve(folder.uri.fsPath);
      return candidate === root || candidate.startsWith(`${root}${path.sep}`);
    });
    if (!allowed || !fs.existsSync(candidate)) {
      throw new Error(`Cannot open plan reference outside the workspace: ${raw}`);
    }
    await vscode.window.showTextDocument(vscode.Uri.file(candidate), { preview: true });
  }

  private async html(webview: vscode.Webview): Promise<string> {
    const media = vscode.Uri.joinPath(this.extensionUri, 'media');
    const template = await fs.promises.readFile(
      vscode.Uri.joinPath(media, 'planPreview.html').fsPath,
      'utf8'
    );
    const nonce = randomBytes(16).toString('base64');
    return template
      .split('{{cspSource}}')
      .join(webview.cspSource)
      .split('{{nonce}}')
      .join(nonce)
      .split('{{styleUri}}')
      .join(
        webview.asWebviewUri(vscode.Uri.joinPath(media, 'planPreview.css')).toString()
      )
      .split('{{scriptUri}}')
      .join(
        webview.asWebviewUri(vscode.Uri.joinPath(media, 'planPreview.js')).toString()
      );
  }
}
