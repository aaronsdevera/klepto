import * as vscode from 'vscode';
import { HostExecutableError, KleptoDaemon } from './daemon';
import { ChatViewProvider } from './chatView';
import { SessionTreeProvider } from './sessionTree';
import { ShareManager } from './shares';
import { manageIncludedModels, manageProviders } from './providers';
import { PlanEditorProvider } from './planEditor';
import {
  cleanGeneratedCommitMessage,
  collectCommitContext,
  pickGitRepository,
} from './commitMessage';

let daemon: KleptoDaemon | undefined;
let chatViewProvider: ChatViewProvider | undefined;
let shares: ShareManager | undefined;

export function activate(context: vscode.ExtensionContext) {
  const config = vscode.workspace.getConfiguration('klepto');
  const listenAddr = config.get<string>('daemon.listen') || '127.0.0.1:7420';
  const autoStart = config.get<boolean>('daemon.autoStart', true);

  shares = new ShareManager(context);
  daemon = new KleptoDaemon(listenAddr);
  daemon.setShareManager(shares);
  chatViewProvider = new ChatViewProvider(context.extensionUri, daemon, shares);
  const planEditorProvider = new PlanEditorProvider(
    context.extensionUri,
    daemon,
    (id, workspace) => chatViewProvider!.buildPlan(id, workspace)
  );
  const sessionTree = new SessionTreeProvider(daemon);
  let ensureDaemonPromise: Promise<boolean> | undefined;
  let indexQueue = Promise.resolve();
  const indexing = new Map<string, Promise<void>>();

  const showManualInstallInstructions = async (): Promise<void> => {
    const command =
      'curl -fsSL https://raw.githubusercontent.com/aaronsdevera/klepto/main/scripts/install.sh | sh';
    const choice = await vscode.window.showInformationMessage(
      `Install Klepto manually, then retry startup:\n\n${command}\n\n` +
        'The default binary location is ~/.klepto/bin/klepto.',
      { modal: true },
      'Copy Command',
      'Open Installation Guide'
    );
    if (choice === 'Copy Command') {
      await vscode.env.clipboard.writeText(command);
      vscode.window.showInformationMessage('Klepto install command copied');
    } else if (choice === 'Open Installation Guide') {
      await vscode.env.openExternal(
        vscode.Uri.parse('https://github.com/aaronsdevera/klepto#quick-start')
      );
    }
  };

  const ensureDaemonAvailable = async (): Promise<boolean> => {
    if (ensureDaemonPromise) return ensureDaemonPromise;
    const operation = (async () => {
      if (!daemon) return false;
      if (await daemon.startOrCheck()) return true;
      if (!daemon.isLocalDaemon()) return false;

      const error = daemon.getLastStartError();
      if (!(error instanceof HostExecutableError) || error.kind !== 'not_installed') {
        return false;
      }

      const choice = await vscode.window.showWarningMessage(
        'Klepto is not installed. Install the latest verified release?',
        { modal: true },
        'Install Latest',
        'Manual Instructions'
      );
      if (choice === 'Manual Instructions') {
        await showManualInstallInstructions();
        return false;
      }
      if (choice !== 'Install Latest') return false;

      try {
        const installed = await vscode.window.withProgress(
          {
            location: vscode.ProgressLocation.Notification,
            title: 'Installing the latest Klepto release',
            cancellable: false,
          },
          () => daemon!.installLatestRelease()
        );
        vscode.window.showInformationMessage(`Installed verified Klepto binary at ${installed}`);
        return await daemon.startOrCheck();
      } catch (installError) {
        vscode.window.showErrorMessage(`Klepto installation failed: ${installError}`);
        await showManualInstallInstructions();
        return false;
      }
    })().finally(() => {
      if (ensureDaemonPromise === operation) ensureDaemonPromise = undefined;
    });
    ensureDaemonPromise = operation;
    return operation;
  };

  const queueWorkspaceIndex = (folder: vscode.WorkspaceFolder): Promise<void> => {
    const workspace = folder.uri.fsPath;
    const existing = indexing.get(workspace);
    if (existing) return existing;
    const job = indexQueue
      .then(async () => {
        if (!daemon || !(await ensureDaemonAvailable())) return;
        if (vscode.workspace.getConfiguration('klepto').get('daemon.runtime') === 'oci') {
          if (!(await shares!.ensureAccess(workspace))) return;
          await daemon.ensureOciMounts();
        }
        await daemon.indexWorkspace(workspace);
      })
      .catch((error) => {
        vscode.window.showWarningMessage(`Klepto could not index ${workspace}: ${error}`);
      })
      .finally(() => indexing.delete(workspace));
    indexing.set(workspace, job);
    indexQueue = job.then(
      () => undefined,
      () => undefined
    );
    return job;
  };

  const bootstrapWorkspaces = async (): Promise<void> => {
    for (const folder of vscode.workspace.workspaceFolders || []) {
      await queueWorkspaceIndex(folder);
    }
  };

  context.subscriptions.push(
    sessionTree,
    vscode.window.registerWebviewViewProvider(ChatViewProvider.viewType, chatViewProvider, {
      webviewOptions: { retainContextWhenHidden: true },
    }),
    vscode.window.registerTreeDataProvider('klepto.sessions', sessionTree),
    vscode.window.registerCustomEditorProvider(PlanEditorProvider.viewType, planEditorProvider, {
      supportsMultipleEditorsPerDocument: false,
      webviewOptions: { retainContextWhenHidden: true },
    }),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('klepto.includedModels')) {
        void chatViewProvider?.refreshModels();
      }
    }),
    vscode.workspace.onDidChangeWorkspaceFolders((event) => {
      for (const folder of event.added) void queueWorkspaceIndex(folder);
    })
  );

  const openChatCmd = vscode.commands.registerCommand('klepto.openChat', async () => {
    await chatViewProvider?.focus();
  });

  const startDaemonCmd = vscode.commands.registerCommand('klepto.startDaemon', async () => {
    const ok = await ensureDaemonAvailable();
    vscode.window.showInformationMessage(
      ok ? 'Klepto daemon is running' : 'Could not start Klepto daemon'
    );
    if (ok) void bootstrapWorkspaces();
  });

  const stopDaemonCmd = vscode.commands.registerCommand('klepto.stopDaemon', async () => {
    await daemon?.stop();
    vscode.window.showInformationMessage('Klepto daemon stop requested');
  });

  const restartDaemonCmd = vscode.commands.registerCommand('klepto.restartDaemon', async () => {
    const ok = await daemon?.startOrCheck({ forceRestart: true });
    vscode.window.showInformationMessage(
      ok ? 'Klepto daemon restarted' : 'Could not restart Klepto daemon'
    );
    sessionTree.refresh();
  });

  const manageSharesCmd = vscode.commands.registerCommand(
    'klepto.manageSharedFolders',
    async () => {
      await shares?.manageSharedFolders();
      // Revoke may require OCI remount without the path
      if (vscode.workspace.getConfiguration('klepto').get('daemon.runtime') === 'oci') {
        await daemon?.startOrCheck({ forceRestart: true });
      }
    }
  );

  const manageProvidersCmd = vscode.commands.registerCommand(
    'klepto.manageProviders',
    async () => {
      if (!daemon) return;
      const changed = await manageProviders(daemon);
      if (changed) {
        await chatViewProvider?.refreshModels();
      }
    }
  );

  const runOnboarding = async (): Promise<boolean> => {
    if (!daemon) return false;
    const running = await ensureDaemonAvailable();
    if (!running) return false;
    const catalog = await daemon.listModels({ refresh: true });
    if (catalog.suggested) {
      const choice = await vscode.window.showInformationMessage(
        'Klepto is running. Configure a hosted or self-hosted model to start working.',
        'Configure Model'
      );
      if (choice === 'Configure Model') {
        await manageProviders(daemon);
        await chatViewProvider?.refreshModels();
      }
    }
    return true;
  };

  const onboardingCmd = vscode.commands.registerCommand(
    'klepto.runOnboarding',
    runOnboarding
  );

  const manageIncludedModelsCmd = vscode.commands.registerCommand(
    'klepto.manageIncludedModels',
    async () => {
      const catalog = chatViewProvider
        ? await chatViewProvider.listModelsCatalog()
        : (await daemon?.listModels()) || {
            models: [],
            providers: [],
            suggested: true,
          };
      const changed = await manageIncludedModels(catalog);
      if (changed) {
        await chatViewProvider?.refreshModels();
      }
    }
  );

  const createSessionCmd = vscode.commands.registerCommand('klepto.createNewSession', async () => {
    await chatViewProvider?.focus();
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    const cwd = workspaceFolder?.uri.fsPath || process.cwd();
    const message = await vscode.window.showInputBox({
      prompt: 'Message',
      placeHolder: 'Ask Klepto something…',
      ignoreFocusOut: true,
    });
    if (message && chatViewProvider) {
      await chatViewProvider.createAndPromptSession(cwd, message);
      sessionTree.refresh();
    }
  });

  const newChatTabCmd = vscode.commands.registerCommand('klepto.newChatTab', async () => {
    await chatViewProvider?.newChatTab();
    sessionTree.refresh();
  });

  const stopSessionCmd = vscode.commands.registerCommand('klepto.stopSession', async () => {
    await chatViewProvider?.requestStopCurrentSession();
  });

  const createPlanCmd = vscode.commands.registerCommand('klepto.createPlan', async () => {
    await chatViewProvider?.createPlanFromChat();
  });

  const openLatestPlanCmd = vscode.commands.registerCommand(
    'klepto.openLatestPlan',
    async () => {
      try {
        await chatViewProvider?.openLatestPlan();
      } catch (e) {
        vscode.window.showErrorMessage(`Failed to open latest plan: ${e}`);
      }
    }
  );

  const buildPlanCmd = vscode.commands.registerCommand('klepto.buildPlan', async () => {
    try {
      await chatViewProvider?.buildPlan();
      sessionTree.refresh();
    } catch (e) {
      vscode.window.showErrorMessage(`Failed to build plan: ${e}`);
    }
  });

  const generateCommitMessageCmd = vscode.commands.registerCommand(
    'klepto.generateCommitMessage',
    async () => {
      try {
        const repository = await pickGitRepository();
        if (!repository) return;
        await vscode.window.withProgress(
          {
            location: vscode.ProgressLocation.Notification,
            title: 'Klepto is generating a commit message',
            cancellable: false,
          },
          async () => {
            const { diff, previousMessages } = await collectCommitContext(repository);
            if (!daemon || !(await ensureDaemonAvailable())) {
              throw new Error('Klepto daemon is unavailable');
            }
            const message = await daemon.generateCommitMessage(
              repository.rootUri.fsPath,
              diff,
              previousMessages
            );
            repository.inputBox.value = cleanGeneratedCommitMessage(message);
          }
        );
      } catch (error) {
        vscode.window.showErrorMessage(`Could not generate commit message: ${error}`);
      }
    }
  );

  const openInTerminalCmd = vscode.commands.registerCommand(
    'klepto.openInTerminal',
    async (sessionId?: string) => {
      if (!sessionId) {
        const sessions = await daemon?.listSessions();
        if (!sessions?.length) {
          vscode.window.showErrorMessage('No active sessions');
          return;
        }
        const selected = await vscode.window.showQuickPick(
          sessions.map((s) => ({
            label: s.id,
            description: `${s.cwd} - ${s.status}`,
          })),
          { placeHolder: 'Select a session' }
        );
        if (!selected) return;
        sessionId = selected.label;
      }

      if (sessionId && daemon) {
        const resumeInfo = await daemon.resumeSession(sessionId);
        if (resumeInfo?.command) {
          const terminal = vscode.window.createTerminal(`Klepto: ${sessionId}`);
          terminal.sendText(resumeInfo.command);
          terminal.show();
        }
      }
    }
  );

  const recallMemoryCmd = vscode.commands.registerCommand('klepto.recallMemory', async () => {
    if (!daemon) {
      vscode.window.showErrorMessage('Klepto daemon not running');
      return;
    }
    const query = await vscode.window.showInputBox({
      prompt: 'What do you want to recall?',
      placeHolder: 'Search your memory…',
    });
    if (!query) return;
    const entries = await daemon.recallMemory(query);
    if (!entries?.length) {
      vscode.window.showInformationMessage('No memories found');
      return;
    }
    const selected = await vscode.window.showQuickPick(
      entries.map((e) => ({
        label: e.id,
        detail: `${e.created_at}${e.workspace ? ` (${e.workspace})` : ''}`,
        description: e.content,
      })),
      { placeHolder: 'Select a memory entry' }
    );
    if (selected) {
      vscode.window.showInformationMessage(`Memory: ${selected.description}`);
    }
  });

  context.subscriptions.push(
    openChatCmd,
    startDaemonCmd,
    stopDaemonCmd,
    restartDaemonCmd,
    manageSharesCmd,
    manageProvidersCmd,
    onboardingCmd,
    manageIncludedModelsCmd,
    createSessionCmd,
    newChatTabCmd,
    stopSessionCmd,
    createPlanCmd,
    openLatestPlanCmd,
    buildPlanCmd,
    generateCommitMessageCmd,
    openInTerminalCmd,
    recallMemoryCmd
  );

  if (autoStart) {
    ensureDaemonAvailable().then(async (available) => {
      if (!available) {
        if (!daemon?.isLocalDaemon()) {
          await vscode.window.showWarningMessage(
            `Klepto daemon is not reachable at ${daemon?.getBaseURL()}. ` +
              `Start it on the remote host with --listen 0.0.0.0:7420.`
          );
        }
      } else {
        await bootstrapWorkspaces();
        await chatViewProvider?.refreshSessions();
        sessionTree.refresh();
        const models = await daemon?.listModels({ refresh: true });
        if (
          models?.suggested &&
          !context.globalState.get<boolean>('klepto.onboarding.modelPrompted')
        ) {
          await context.globalState.update('klepto.onboarding.modelPrompted', true);
          await runOnboarding();
        }
      }
    });
  }
}

export function deactivate() {
  // Leave the daemon running so tmux sessions survive extension reload.
}
