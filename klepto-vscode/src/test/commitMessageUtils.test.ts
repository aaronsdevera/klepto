import { strict as assert } from 'assert';
import { describe, it } from 'node:test';
import {
  MAX_DIFF_BYTES,
  cleanGeneratedCommitMessage,
  truncateDiff,
} from '../commitMessageUtils';

describe('commit message generation', () => {
  it('cleans common model formatting without changing the message', () => {
    assert.equal(
      cleanGeneratedCommitMessage('```text\nfeat: add generated commit messages\n```'),
      'feat: add generated commit messages'
    );
    assert.equal(
      cleanGeneratedCommitMessage('Commit message: Fix commit generation'),
      'Fix commit generation'
    );
  });

  it('rejects empty model output', () => {
    assert.throws(() => cleanGeneratedCommitMessage('  '), /empty commit message/);
  });

  it('caps diffs before sending them to the daemon', () => {
    const result = truncateDiff('x'.repeat(MAX_DIFF_BYTES + 1_000));
    assert.ok(Buffer.byteLength(result) <= MAX_DIFF_BYTES);
    assert.ok(result.endsWith('[diff truncated]\n'));
  });
});
