import * as vscode from 'vscode';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { ModelInfo, ModelsResponse } from './types';

const BUILTIN_PROVIDERS = [
  'anthropic',
  'openai',
  'google',
  'openrouter',
  'groq',
  'xai',
  'mistral',
  'deepseek',
  'ollama',
] as const;

type OmpModelDef = {
  id: string;
  name?: string;
  [k: string]: unknown;
};

type OmpProviderConfig = {
  name?: string;
  baseUrl?: string;
  api?: string;
  apiKey?: string;
  auth?: string;
  models?: OmpModelDef[];
  [k: string]: unknown;
};

type ModelsYml = {
  providers?: Record<string, OmpProviderConfig>;
  [k: string]: unknown;
};

export interface ProviderManagerApi {
  upsertProvider(input: {
    id: string;
    kind?: 'openai_compatible' | 'ollama';
    base_url?: string;
    api?: string;
    models?: string[];
    api_key?: string;
  }): Promise<void>;
  deleteProvider(id: string): Promise<void>;
}

function agentDir(): string {
  return path.join(os.homedir(), '.omp', 'agent');
}

function modelsYmlPath(): string {
  return path.join(agentDir(), 'models.yml');
}

/** Best-effort parse of omp models.yml provider entries (no full YAML dependency). */
function readModelsYml(): ModelsYml {
  const filePath = modelsYmlPath();
  try {
    if (!fs.existsSync(filePath)) return { providers: {} };
    const raw = fs.readFileSync(filePath, 'utf8');
    if (!raw.trim()) return { providers: {} };
    return parseModelsYmlProviders(raw);
  } catch (e) {
    throw new Error(`Failed to read ${filePath}: ${e instanceof Error ? e.message : e}`);
  }
}

function parseModelsYmlProviders(raw: string): ModelsYml {
  const providers: Record<string, OmpProviderConfig> = {};
  const lines = raw.split(/\r?\n/);
  let inProviders = false;
  let currentId: string | null = null;
  let current: OmpProviderConfig | null = null;
  let inModels = false;

  for (const line of lines) {
    if (!inProviders) {
      if (/^providers:\s*$/.test(line) || /^providers:\s*\{\s*\}\s*$/.test(line)) {
        inProviders = true;
      }
      continue;
    }
    if (/^[^\s#]/.test(line) && !line.startsWith('providers:')) {
      break;
    }
    const providerMatch = line.match(/^  ([A-Za-z0-9_-]+):\s*$/);
    if (providerMatch) {
      if (currentId && current) providers[currentId] = current;
      currentId = providerMatch[1];
      current = {};
      inModels = false;
      continue;
    }
    if (!currentId || !current) continue;

    if (/^    models:\s*$/.test(line)) {
      inModels = true;
      current.models = current.models || [];
      continue;
    }
    if (inModels) {
      const modelId = line.match(/^      - id:\s*["']?([^"'\s]+)["']?\s*$/);
      if (modelId) {
        current.models = current.models || [];
        current.models.push({ id: modelId[1] });
        continue;
      }
      if (/^    [A-Za-z]/.test(line)) {
        inModels = false;
      } else {
        continue;
      }
    }

    const kv = line.match(/^    (baseUrl|api|apiKey|auth):\s*(.+?)\s*$/);
    if (kv) {
      let value = kv[2].trim();
      if (
        (value.startsWith('"') && value.endsWith('"')) ||
        (value.startsWith("'") && value.endsWith("'"))
      ) {
        value = value.slice(1, -1);
      }
      if (kv[1] === 'baseUrl') current.baseUrl = value;
      else if (kv[1] === 'api') current.api = value;
      else if (kv[1] === 'apiKey') current.apiKey = value;
      else if (kv[1] === 'auth') current.auth = value;
    }
  }
  if (currentId && current) providers[currentId] = current;
  return { providers };
}

function listConnectionStatus(): Array<{
  provider: string;
  hasKey: boolean;
  baseUrl?: string;
  custom: boolean;
  modelCount: number;
}> {
  const models = readModelsYml();
  return Object.keys(models.providers || {})
    .sort()
    .map((provider) => {
      const cfg = models.providers?.[provider];
      const hasKey =
        typeof cfg?.apiKey === 'string' &&
        cfg.apiKey.length > 0 &&
        cfg.apiKey !== 'none' &&
        cfg.auth !== 'none';
      return {
        provider,
        hasKey: hasKey || cfg?.auth === 'none',
        baseUrl: typeof cfg?.baseUrl === 'string' ? cfg.baseUrl : undefined,
        custom: !!cfg?.baseUrl,
        modelCount: Array.isArray(cfg?.models) ? cfg.models.length : 0,
      };
    });
}

async function addBuiltinConnection(api: ProviderManagerApi): Promise<boolean> {
  const picked = await vscode.window.showQuickPick(
    BUILTIN_PROVIDERS.map((p) => ({
      label: p,
      description: p === 'ollama' ? 'API key optional (local)' : 'API key required',
    })),
    { placeHolder: 'Select a built-in provider', title: 'Add provider connection' }
  );
  if (!picked) return false;
  if (picked.label === 'ollama') return addOllamaConnection(api);

  const key = await vscode.window.showInputBox({
    title: `API key for ${picked.label}`,
    prompt: `Enter API key for ${picked.label}`,
    password: true,
    ignoreFocusOut: true,
    placeHolder: 'sk-…',
  });
  if (key === undefined) return false;
  if (!key.trim()) {
    vscode.window.showErrorMessage(`API key is required for ${picked.label}`);
    return false;
  }
  await api.upsertProvider({
    id: picked.label,
    models: [],
    api_key: key.trim() || undefined,
  });
  vscode.window.showInformationMessage(
    `Saved ${picked.label} through the Klepto provider catalog`
  );
  return true;
}

async function addOpenAiCompatConnection(api: ProviderManagerApi): Promise<boolean> {
  const provider = await vscode.window.showInputBox({
    title: 'OpenAI-compatible provider id',
    prompt: 'Short id used as the provider name (e.g. vllm, vllm-proxy, local)',
    placeHolder: 'vllm',
    ignoreFocusOut: true,
    validateInput: (v) => {
      const t = v.trim();
      if (!t) return 'Provider id is required';
      if (!/^[a-z0-9][a-z0-9_-]*$/i.test(t)) {
        return 'Use letters, numbers, hyphen, or underscore';
      }
      return undefined;
    },
  });
  if (!provider) return false;
  const providerId = provider.trim();

  const baseUrl = await vscode.window.showInputBox({
    title: `Base URL for ${providerId}`,
    prompt: 'OpenAI-compatible API base URL',
    placeHolder: 'http://127.0.0.1:8000/v1',
    ignoreFocusOut: true,
    validateInput: (v) => {
      const t = v.trim();
      if (!t) return 'Base URL is required';
      try {
        // URL constructor validates absolute URLs
        new URL(t);
      } catch {
        return 'Enter a valid URL';
      }
      return undefined;
    },
  });
  if (!baseUrl) return false;

  const key = await vscode.window.showInputBox({
    title: `API key for ${providerId} (optional)`,
    prompt: 'Leave empty if the endpoint does not require auth',
    password: true,
    ignoreFocusOut: true,
  });
  if (key === undefined) return false;

  await api.upsertProvider({
    id: providerId,
    kind: 'openai_compatible',
    base_url: baseUrl.trim(),
    api: 'openai-completions',
    models: [],
    api_key: key.trim() || 'no-key',
  });
  vscode.window.showInformationMessage(
    `Saved ${providerId}; available models will refresh from ${baseUrl.trim().replace(/\/$/, '')}/models`
  );
  return true;
}

async function addOllamaConnection(api: ProviderManagerApi): Promise<boolean> {
  const provider = await vscode.window.showInputBox({
    title: 'Ollama provider id',
    prompt: 'Short id used in model labels and profiles',
    value: 'ollama',
    ignoreFocusOut: true,
    validateInput: (value) =>
      /^[a-z0-9][a-z0-9_-]*$/i.test(value.trim())
        ? undefined
        : 'Use letters, numbers, hyphen, or underscore',
  });
  if (!provider) return false;

  const baseUrl = await vscode.window.showInputBox({
    title: `Ollama base URL for ${provider.trim()}`,
    prompt: 'Ollama server root; /api and /v1 suffixes are accepted and normalized',
    value: 'http://127.0.0.1:11434',
    ignoreFocusOut: true,
    validateInput: (value) => {
      try {
        const url = new URL(value.trim());
        return url.protocol === 'http:' || url.protocol === 'https:'
          ? undefined
          : 'Use an http:// or https:// URL';
      } catch {
        return 'Enter a valid absolute URL';
      }
    },
  });
  if (!baseUrl) return false;

  const key = await vscode.window.showInputBox({
    title: `API key for ${provider.trim()} (optional)`,
    prompt: 'Local Ollama does not require a key; provide one for an authenticated remote endpoint',
    password: true,
    ignoreFocusOut: true,
  });
  if (key === undefined) return false;

  await api.upsertProvider({
    id: provider.trim(),
    kind: 'ollama',
    base_url: baseUrl.trim(),
    api: 'openai-completions',
    models: [],
    api_key: key.trim() || 'no-key',
  });
  vscode.window.showInformationMessage(
    `Saved ${provider.trim()}; models will refresh from the Ollama /api/tags endpoint`
  );
  return true;
}

async function removeConnection(api: ProviderManagerApi): Promise<boolean> {
  const status = listConnectionStatus();
  if (!status.length) {
    vscode.window.showInformationMessage('No provider connections configured.');
    return false;
  }
  const picked = await vscode.window.showQuickPick(
    status.map((s) => ({
      label: s.provider,
      description: [
        s.hasKey ? 'key' : 'no key',
        s.custom ? `custom ${s.baseUrl}` : 'built-in',
        s.modelCount ? `${s.modelCount} models` : undefined,
      ]
        .filter(Boolean)
        .join(' · '),
    })),
    { placeHolder: 'Select a connection to remove', title: 'Remove provider connection' }
  );
  if (!picked) return false;

  await api.deleteProvider(picked.label);
  vscode.window.showInformationMessage(`Removed connection for ${picked.label}`);
  return true;
}

async function showConnectionStatus(): Promise<void> {
  const status = listConnectionStatus();
  if (!status.length) {
    vscode.window.showInformationMessage(
      'No provider connections yet. Use “Add built-in” or “Add OpenAI-compatible”.'
    );
    return;
  }
  await vscode.window.showQuickPick(
    status.map((s) => ({
      label: s.provider,
      description: s.hasKey ? 'API key configured' : 'No API key',
      detail: s.custom
        ? `${s.baseUrl || ''} · ${s.modelCount} model(s) in models.yml`
        : 'Built-in / omp provider (models.yml)',
    })),
    { placeHolder: 'Configured providers', title: 'Provider connections' }
  );
}

/**
 * QuickPick hub for managing omp provider credentials and custom OpenAI-compatible endpoints.
 * Writes through the Klepto daemon into ~/.omp/agent/models.yml. Returns true if files changed.
 */
export async function manageProviders(api: ProviderManagerApi): Promise<boolean> {
  const action = await vscode.window.showQuickPick(
    [
      {
        label: 'Add built-in provider',
        description: 'Anthropic, OpenAI, Google, …',
        action: 'builtin' as const,
      },
      {
        label: 'Add Ollama endpoint',
        description: 'Local or remote Ollama with live model discovery',
        action: 'ollama' as const,
      },
      {
        label: 'Add OpenAI-compatible endpoint',
        description: 'Custom base URL + models (e.g. vLLM)',
        action: 'compat' as const,
      },
      {
        label: 'Refresh available models',
        description: 'Re-query configured providers and the omp catalog',
        action: 'refresh' as const,
      },
      {
        label: 'Remove connection',
        description: 'Delete from Klepto catalog / models.yml',
        action: 'remove' as const,
      },
      {
        label: 'Show connections',
        description: 'List configured providers',
        action: 'status' as const,
      },
    ],
    { placeHolder: 'Manage provider connections', title: 'Klepto providers' }
  );
  if (!action) return false;

  switch (action.action) {
    case 'builtin':
      return addBuiltinConnection(api);
    case 'compat':
      return addOpenAiCompatConnection(api);
    case 'ollama':
      return addOllamaConnection(api);
    case 'refresh':
      return true;
    case 'remove':
      return removeConnection(api);
    case 'status':
      await showConnectionStatus();
      return false;
    default:
      return false;
  }
}

/**
 * Multi-select models to include in the chat picker. Empty allowlist = show all.
 */
export async function manageIncludedModels(
  catalog: ModelsResponse | ModelInfo[]
): Promise<boolean> {
  const models = Array.isArray(catalog) ? catalog : catalog.models || [];
  if (!models.length) {
    vscode.window.showWarningMessage(
      'No models available. Configure a provider first (Klepto: Manage Providers), then refresh.'
    );
    return false;
  }

  const cfg = vscode.workspace.getConfiguration('klepto');
  const current = new Set(
    (cfg.get<string[]>('includedModels') || []).map((s) => s.trim()).filter(Boolean)
  );

  const picked = await vscode.window.showQuickPick(
    models.map((m) => ({
      label: m.label,
      description: m.provider,
      picked: current.size === 0 ? true : current.has(m.label),
    })),
    {
      title: 'Included models',
      placeHolder: 'Select models to show in the chat picker (none selected = show all)',
      canPickMany: true,
      matchOnDescription: true,
    }
  );
  if (!picked) return false;

  // If user selected everything, store empty (= show all) for less config noise
  const labels = picked.map((p) => p.label);
  const next =
    labels.length === 0 || labels.length === models.length
      ? []
      : labels.sort();

  await cfg.update('includedModels', next, vscode.ConfigurationTarget.Global);
  vscode.window.showInformationMessage(
    next.length
      ? `Chat picker limited to ${next.length} model${next.length === 1 ? '' : 's'}`
      : 'Chat picker will show all discovered models'
  );
  return true;
}

/** Apply klepto.includedModels allowlist (empty = no filter). */
export function filterModelsByAllowlist(response: ModelsResponse): ModelsResponse {
  const allow = (vscode.workspace.getConfiguration('klepto').get<string[]>('includedModels') || [])
    .map((s) => s.trim())
    .filter(Boolean);
  if (!allow.length) return response;

  const allowSet = new Set(allow);
  const models = response.models.filter((m) => allowSet.has(m.label));
  const providers = [...new Set(models.map((m) => m.provider))].sort();
  return {
    ...response,
    models,
    providers,
    message:
      models.length === 0
        ? `No models match klepto.includedModels (${allow.length} entries). Use “Klepto: Manage Included Models”.`
        : response.message,
  };
}
