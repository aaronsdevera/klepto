import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

export class HostExecutableError extends Error {
  constructor(
    message: string,
    readonly kind: 'invalid_override' | 'not_installed'
  ) {
    super(message);
    this.name = 'HostExecutableError';
  }
}

export function hostExecutableCandidates(home = os.homedir()): string[] {
  return [
    path.join(home, '.klepto', 'bin', 'klepto'),
    '/usr/local/bin/klepto',
    path.join(home, '.local', 'bin', 'klepto'),
  ];
}

function expandHome(value: string, home = os.homedir()): string {
  if (value === '~') return home;
  if (value.startsWith(`~${path.sep}`)) return path.join(home, value.slice(2));
  return value;
}

function isExecutableFile(candidate: string): boolean {
  try {
    fs.accessSync(candidate, fs.constants.X_OK);
    return fs.statSync(candidate).isFile();
  } catch {
    return false;
  }
}

export function resolveHostExecutable(
  configured = '',
  home = os.homedir(),
  candidates = hostExecutableCandidates(home)
): string {
  const override = configured.trim();
  if (override) {
    const candidate = path.resolve(expandHome(override, home));
    if (!isExecutableFile(candidate)) {
      throw new HostExecutableError(
        `klepto.daemon.path is not an executable file: ${candidate}`,
        'invalid_override'
      );
    }
    return candidate;
  }

  for (const candidate of candidates) {
    if (isExecutableFile(candidate)) return candidate;
  }
  throw new HostExecutableError(
    `Klepto is not installed. Checked: ${candidates.join(', ')}`,
    'not_installed'
  );
}

export function releaseAssetName(
  platform: NodeJS.Platform = process.platform,
  arch: string = process.arch
): string {
  const assets: Record<string, string> = {
    'darwin:arm64': 'klepto-darwin-arm64',
    'darwin:x64': 'klepto-darwin-amd64',
    'linux:x64': 'klepto-linux-amd64',
    'linux:arm64': 'klepto-linux-arm64',
  };
  const asset = assets[`${platform}:${arch}`];
  if (!asset) throw new Error(`No Klepto release is available for ${platform}/${arch}`);
  return asset;
}

export function verifyReleaseChecksum(binary: Buffer, checksumText: string): boolean {
  const expected = checksumText.trim().match(/^([a-fA-F0-9]{64})(?:\s|$)/)?.[1]?.toLowerCase();
  if (!expected) return false;
  return createHash('sha256').update(binary).digest('hex') === expected;
}
