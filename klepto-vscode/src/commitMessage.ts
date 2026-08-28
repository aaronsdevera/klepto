import { execFile } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import { MAX_DIFF_BYTES, truncateDiff } from './commitMessageUtils';
export { cleanGeneratedCommitMessage, truncateDiff } from './commitMessageUtils';

const MAX_UNTRACKED_FILE_BYTES = 40_000;

interface GitRepository {
  rootUri: vscode.Uri;
  inputBox: { value: string };
}

interface GitApi {
  repositories: GitRepository[];
  getRepository(uri: vscode.Uri): GitRepository | null;
}

interface GitExtension {
  getAPI(version: 1): GitApi;
}

function runGit(cwd: string, args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      'git',
      args,
      { cwd, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 },
      (error, stdout, stderr) => {
        if (error) {
          reject(new Error(stderr.trim() || error.message));
          return;
        }
        resolve(stdout);
      }
    );
  });
}

export async function pickGitRepository(): Promise<GitRepository | undefined> {
  const extension = vscode.extensions.getExtension<GitExtension>('vscode.git');
  if (!extension) throw new Error('The built-in Git extension is unavailable');
  const exports = extension.isActive ? extension.exports : await extension.activate();
  const api = exports.getAPI(1);
  const activeUri = vscode.window.activeTextEditor?.document.uri;
  const active = activeUri ? api.getRepository(activeUri) : null;
  if (active) return active;
  if (api.repositories.length === 1) return api.repositories[0];
  if (api.repositories.length === 0) throw new Error('No Git repository is open');

  const selected = await vscode.window.showQuickPick(
    api.repositories.map((repository) => ({
      label: path.basename(repository.rootUri.fsPath),
      description: repository.rootUri.fsPath,
      repository,
    })),
    { placeHolder: 'Select a repository for the commit message' }
  );
  return selected?.repository;
}

export async function collectCommitContext(
  repository: GitRepository
): Promise<{ diff: string; previousMessages: string[] }> {
  const cwd = repository.rootUri.fsPath;
  const [staged, log] = await Promise.all([
    runGit(cwd, ['diff', '--cached', '--no-ext-diff', '--no-color', '--']),
    runGit(cwd, ['log', '-10', '--pretty=%s']).catch(() => ''),
  ]);
  const previousMessages = log
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);

  if (staged.trim()) {
    return { diff: truncateDiff(staged), previousMessages };
  }

  const [tracked, untrackedOutput] = await Promise.all([
    runGit(cwd, ['diff', '--no-ext-diff', '--no-color', '--']),
    runGit(cwd, ['ls-files', '--others', '--exclude-standard', '-z']),
  ]);
  const untracked = untrackedOutput.split('\0').filter(Boolean);
  const sections = tracked.trim() ? [tracked] : [];
  let usedBytes = Buffer.byteLength(tracked);

  for (const relativePath of untracked) {
    if (usedBytes >= MAX_DIFF_BYTES) break;
    const absolutePath = path.join(cwd, relativePath);
    let content: Buffer;
    try {
      const stat = await fs.promises.lstat(absolutePath);
      if (!stat.isFile()) continue;
      content = await fs.promises.readFile(absolutePath);
    } catch {
      continue;
    }

    const header = `diff --git a/${relativePath} b/${relativePath}\nnew file\n--- /dev/null\n+++ b/${relativePath}\n`;
    let section: string;
    if (content.includes(0)) {
      section = `${header}Binary file added\n`;
    } else {
      const text = content.subarray(0, MAX_UNTRACKED_FILE_BYTES).toString('utf8');
      const added = text
        .split(/\r?\n/)
        .map((line) => `+${line}`)
        .join('\n');
      const clipped = content.length > MAX_UNTRACKED_FILE_BYTES ? '\n+[file truncated]' : '';
      section = `${header}@@ -0,0 +1 @@\n${added}${clipped}\n`;
    }
    sections.push(section);
    usedBytes += Buffer.byteLength(section);
  }

  const diff = truncateDiff(sections.join('\n'));
  if (!diff.trim()) throw new Error('No changes are available to summarize');
  return { diff, previousMessages };
}

