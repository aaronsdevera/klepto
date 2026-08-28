export const MAX_DIFF_BYTES = 110_000;

export function truncateDiff(diff: string): string {
  if (Buffer.byteLength(diff) <= MAX_DIFF_BYTES) return diff;
  const marker = '\n\n[diff truncated]\n';
  const bytes = Buffer.from(diff);
  const available = MAX_DIFF_BYTES - Buffer.byteLength(marker);
  return `${bytes.subarray(0, available).toString('utf8')}${marker}`;
}

export function cleanGeneratedCommitMessage(message: string): string {
  let cleaned = message.trim().replace(/\r\n/g, '\n');
  const fenced = cleaned.match(/^```(?:text)?\s*\n([\s\S]*?)\n```$/i);
  if (fenced) cleaned = fenced[1].trim();
  cleaned = cleaned.replace(/^(?:commit message|message):\s*/i, '').trim();
  if (/^["'][^"'\n]+["']$/.test(cleaned)) cleaned = cleaned.slice(1, -1).trim();
  if (!cleaned) throw new Error('The model returned an empty commit message');
  return cleaned;
}
