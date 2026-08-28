import { strict as assert } from 'assert';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { afterEach, describe, it } from 'node:test';
import {
  HostExecutableError,
  releaseAssetName,
  resolveHostExecutable,
  verifyReleaseChecksum,
} from '../bootstrap';

const temporary: string[] = [];

function tempHome(): string {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'klepto-bootstrap-'));
  temporary.push(home);
  return home;
}

function executable(file: string): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, '#!/bin/sh\n');
  fs.chmodSync(file, 0o755);
}

afterEach(() => {
  while (temporary.length) fs.rmSync(temporary.pop()!, { recursive: true, force: true });
});

describe('host executable discovery', () => {
  it('prefers the canonical ~/.klepto binary', () => {
    const home = tempHome();
    const canonical = path.join(home, '.klepto', 'bin', 'klepto');
    const fallback = path.join(home, '.local', 'bin', 'klepto');
    executable(canonical);
    executable(fallback);
    assert.equal(resolveHostExecutable('', home, [canonical, fallback]), canonical);
  });

  it('distinguishes invalid overrides from a missing installation', () => {
    const home = tempHome();
    assert.throws(
      () => resolveHostExecutable('~/missing', home, []),
      (error: unknown) =>
        error instanceof HostExecutableError && error.kind === 'invalid_override'
    );
    assert.throws(
      () => resolveHostExecutable('', home, [path.join(home, 'missing')]),
      (error: unknown) => error instanceof HostExecutableError && error.kind === 'not_installed'
    );
  });
});

describe('release verification', () => {
  it('selects supported release asset names', () => {
    assert.equal(releaseAssetName('darwin', 'arm64'), 'klepto-darwin-arm64');
    assert.equal(releaseAssetName('linux', 'x64'), 'klepto-linux-amd64');
    assert.throws(() => releaseAssetName('win32', 'x64'));
  });

  it('rejects missing or mismatched checksum sidecars', () => {
    const binary = Buffer.from('verified release');
    const checksum = createHash('sha256').update(binary).digest('hex');
    assert.equal(verifyReleaseChecksum(binary, `${checksum}  klepto\n`), true);
    assert.equal(verifyReleaseChecksum(binary, `${'0'.repeat(64)}  klepto\n`), false);
    assert.equal(verifyReleaseChecksum(binary, 'not a checksum'), false);
  });
});
