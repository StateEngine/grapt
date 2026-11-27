// Read existing ordinary artifacts only; never open the writable Rust store or its key.
import { constants } from 'node:fs';
import { lstat, open } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';

async function bounded(file, limit) {
  if (!(await lstat(file)).isFile()) throw new Error('not-regular');
  const handle = await open(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > limit) throw new Error('size-bound');
    const bytes = Buffer.alloc(limit + 1);
    let used = 0;
    while (used < bytes.length) {
      const { bytesRead } = await handle.read(bytes, used, bytes.length - used, null);
      if (!bytesRead) break;
      used += bytesRead;
    }
    if (used > limit) throw new Error('size-bound');
    return bytes.subarray(0, used);
  } finally { await handle.close(); }
}

export function artifactReader(service) {
  let remaining = 32 * 1024 * 1024;
  return async reference => {
    try {
      if (!reference || !/^[a-f0-9]{64}$/.test(reference.id)
          || !/^[a-f0-9]{64}$/.test(reference.digest)
          || !Number.isSafeInteger(reference.length) || reference.length < 0) {
        throw new Error('invalid-reference');
      }
      if (reference.length > 1024 * 1024) throw new Error('artifact-size-bound');
      const charge = 16 * 1024 + reference.length + 1;
      if (remaining < charge) throw new Error('artifact-total-byte-bound');
      remaining -= charge; // Charge failed reads too; corrupt evidence cannot evade the I/O bound.
      const root = path.join(service, 'dogfood-artifacts');
      // Refuse linked directory components as well as linked payload files.
      for (const component of [root, path.join(root, 'meta'), path.join(root, 'objects'),
        path.join(root, 'meta', reference.id.slice(0, 2)),
        path.join(root, 'objects', reference.id.slice(0, 2))]) {
        if (!(await lstat(component)).isDirectory()) throw new Error('not-directory');
      }
      const metadata = await bounded(path.join(root, 'meta', reference.id.slice(0, 2), reference.id), 16 * 1024);
      const lines = metadata.toString('utf8').trimEnd().split('\n');
      if (lines.shift() !== 'v1') throw new Error('metadata-version');
      const fields = new Map();
      for (const line of lines) {
        const at = line.indexOf(' '), key = line.slice(0, at), value = line.slice(at + 1);
        if (at < 1 || (key !== 'chunk' && fields.has(key))) throw new Error('invalid-metadata');
        if (key !== 'chunk') fields.set(key, value);
      }
      if (fields.get('sensitivity') === 'secret') throw new Error('secret-bearing');
      if (fields.get('sensitivity') !== 'ordinary' || fields.get('id') !== reference.id
          || fields.get('length') !== String(reference.length) || fields.get('digest') !== reference.digest
          || !['complete', 'truncated'].includes(fields.get('disposition'))
          || !['captured-output', 'diagnostic', 'campaign-plan'].includes(fields.get('kind'))) {
        throw new Error('metadata-mismatch');
      }
      if (reference.disposition && reference.disposition !== fields.get('disposition')) throw new Error('disposition-mismatch');
      const length = Buffer.alloc(8); length.writeBigUInt64BE(BigInt(reference.length));
      const identity = createHash('sha256').update('secode-artifact/id/2').update(fields.get('kind'))
        .update(fields.get('disposition')).update(length).update(reference.digest).digest('hex');
      if (identity !== reference.id) throw new Error('identity-mismatch');
      const bytes = await bounded(path.join(root, 'objects', reference.id.slice(0, 2), reference.id), reference.length);
      if (bytes.length !== reference.length || createHash('sha256').update(bytes).digest('hex') !== reference.digest) {
        throw new Error('digest-mismatch');
      }
      const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
      return { status: 'observed', text, bytes: bytes.length, disposition: fields.get('disposition') };
    } catch (error) {
      // Missing, unreadable and corrupt bytes are unavailable evidence, never empty text.
      return { status: 'unavailable', reason: error.code ?? error.message };
    }
  };
}
