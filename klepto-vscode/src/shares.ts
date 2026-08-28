import * as vscode from 'vscode';
import * as os from 'os';
import * as path from 'path';
import * as fs from 'fs';

const SHARES_KEY = 'klepto.sharedFolders';
const SESSION_GRANTS_KEY = 'klepto.sessionGrants';

export interface SharedFolder {
  path: string;
  remember: boolean;
  grantedAt: number;
}

/**
 * Manages workspace folder consent for OCI mounts:
 * Grant (session) / Deny / Grant and don't ask again (persistent).
 */
export class ShareManager {
  private sessionGrants = new Set<string>();

  constructor(private readonly context: vscode.ExtensionContext) {
    const session = context.workspaceState.get<string[]>(SESSION_GRANTS_KEY) || [];
    for (const p of session) this.sessionGrants.add(normalizePath(p));
  }

  listRemembered(): SharedFolder[] {
    return this.context.globalState.get<SharedFolder[]>(SHARES_KEY) || [];
  }

  rememberedPaths(): string[] {
    return this.listRemembered().map((s) => normalizePath(s.path));
  }

  /** Paths that should be bind-mounted into the OCI container. */
  mountPaths(): string[] {
    const remembered = this.rememberedPaths();
    const session = [...this.sessionGrants];
    return [...new Set([...remembered, ...session])];
  }

  isAllowed(folderPath: string): boolean {
    const p = normalizePath(folderPath);
    if (this.sessionGrants.has(p)) return true;
    return this.rememberedPaths().some((r) => p === r || p.startsWith(r + path.sep));
  }

  async ensureAccess(folderPath: string): Promise<boolean> {
    const p = normalizePath(folderPath);
    if (this.isAllowed(p)) {
      return true;
    }

    const choice = await vscode.window.showWarningMessage(
      `Klepto needs access to this folder to run sessions:\n${p}`,
      { modal: true },
      'Grant',
      "Grant and don't ask again",
      'Deny'
    );

    if (choice === 'Deny' || choice === undefined) {
      return false;
    }

    if (choice === "Grant and don't ask again") {
      await this.remember(p);
      return true;
    }

    // Grant for this window / until reload
    this.sessionGrants.add(p);
    await this.context.workspaceState.update(SESSION_GRANTS_KEY, [...this.sessionGrants]);
    return true;
  }

  async remember(folderPath: string): Promise<void> {
    const p = normalizePath(folderPath);
    const list = this.listRemembered().filter((s) => normalizePath(s.path) !== p);
    list.push({ path: p, remember: true, grantedAt: Date.now() });
    await this.context.globalState.update(SHARES_KEY, list);
    this.sessionGrants.add(p);
  }

  async forget(folderPath: string): Promise<void> {
    const p = normalizePath(folderPath);
    const list = this.listRemembered().filter((s) => normalizePath(s.path) !== p);
    await this.context.globalState.update(SHARES_KEY, list);
    this.sessionGrants.delete(p);
    await this.context.workspaceState.update(SESSION_GRANTS_KEY, [...this.sessionGrants]);
  }

  async manageSharedFolders(): Promise<void> {
    const remembered = this.listRemembered();
    if (!remembered.length) {
      vscode.window.showInformationMessage('No remembered Klepto folder grants.');
      return;
    }
    const picked = await vscode.window.showQuickPick(
      remembered.map((s) => ({
        label: s.path,
        description: new Date(s.grantedAt).toLocaleString(),
        detail: 'Select to revoke access',
      })),
      { placeHolder: 'Shared folders (select to revoke)' }
    );
    if (!picked) return;
    await this.forget(picked.label);
    vscode.window.showInformationMessage(`Revoked Klepto access to ${picked.label}`);
  }
}

export function normalizePath(p: string): string {
  return path.resolve(p);
}

/** Host paths to mount for daemon data / omp auth (same path or mapped). */
export function defaultDataMounts(): { host: string; container: string }[] {
  const home = os.homedir();
  const mounts: { host: string; container: string }[] = [];
  const omp = path.join(home, '.omp');
  mounts.push({ host: omp, container: '/home/klepto/.omp' });

  const klepto = path.join(home, '.klepto');
  mounts.push({ host: klepto, container: '/home/klepto/.klepto' });
  return mounts;
}
