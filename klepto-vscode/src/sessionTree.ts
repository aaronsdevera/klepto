import * as vscode from 'vscode';
import { KleptoDaemon } from './daemon';
import { Session } from './types';

export class SessionTreeProvider
  implements vscode.TreeDataProvider<SessionTreeItem>, vscode.Disposable
{
  private _onDidChangeTreeData = new vscode.EventEmitter<SessionTreeItem | undefined | null | void>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  private sessions: Session[] = [];
  private readonly refreshTimer: NodeJS.Timeout;

  constructor(private readonly daemon: KleptoDaemon) {
    this.refreshTimer = setInterval(() => this.refresh(), 15000);
  }

  dispose(): void {
    clearInterval(this.refreshTimer);
    this._onDidChangeTreeData.dispose();
  }

  getTreeItem(element: SessionTreeItem): vscode.TreeItem {
    return element;
  }

  async getChildren(): Promise<SessionTreeItem[]> {
    try {
      this.sessions = (await this.daemon.listSessions()).filter(
        (session) => session.status === 'Running'
      );
    } catch {
      this.sessions = [];
    }
    return this.sessions.map((s) => new SessionTreeItem(s));
  }

  refresh(): void {
    this._onDidChangeTreeData.fire();
  }
}

export class SessionTreeItem extends vscode.TreeItem {
  constructor(public readonly session: Session) {
    super(session.id, vscode.TreeItemCollapsibleState.None);
    this.description = session.status;
    this.tooltip = `${session.cwd} · ${session.agent_mode || 'agent'} · ${session.model || session.omp_mode || session.pi_mode}`;
    this.command = {
      command: 'klepto.openInTerminal',
      title: 'Open in Terminal',
      arguments: [session.id],
    };
    this.contextValue = session.status === 'Running' ? 'running' : 'stopped';
    this.iconPath =
      session.status === 'Running'
        ? new vscode.ThemeIcon('terminal')
        : new vscode.ThemeIcon('debug-disconnect');
  }
}
