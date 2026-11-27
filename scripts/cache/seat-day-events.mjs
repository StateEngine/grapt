import { DatabaseSync } from 'node:sqlite';
import { lstat } from 'node:fs/promises';

// This is operational history, including work later rewound out of the transcript.
const families = [
  'session.assistant-message.opened.',
  'session.tool.input.started.', 'session.tool.nested.started.', 'session.tool.terminal.',
  'session.compaction.call.started.', 'session.compaction.call.ended.', 'session.compaction.ended.',
  'session.request.context.', 'session.execution.failed.', 'session.step.failed.',
];
export const supported = new Set([
  'session.assistant-message.opened.1', 'session.assistant-message.opened.2',
  'session.tool.input.started.1', 'session.tool.input.started.2', 'session.tool.nested.started.1',
  'session.tool.terminal.1', 'session.compaction.call.started.1', 'session.compaction.call.ended.1',
  'session.compaction.ended.2', 'session.request.context.1', 'session.execution.failed.1',
  'session.step.failed.2', 'session.step.failed.3',
]);

export async function readEvents(database, sessions) {
  if (!(await lstat(database)).isFile()) throw new Error('Accounting database is not a regular file');
  if (sessions.length > 256) throw new Error('Event report exceeds 256 sessions');
  const db = new DatabaseSync(database, { readOnly: true, enableExtensions: false, timeout: 0 });
  const events = [], unsupported = new Map();
  let bytes = 0, scanned = 0;
  try {
    const query = db.prepare(`SELECT
      CASE WHEN length(CAST(id AS BLOB)) <= 256 THEN id END AS id, seq, created,
      CASE WHEN length(CAST(type AS BLOB)) <= 128 THEN type END AS type,
      CASE WHEN length(CAST(data AS BLOB)) <= 1048576 THEN data END AS data
      FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?
      AND (${families.map(() => 'type LIKE ?').join(' OR ')}) ORDER BY seq LIMIT 131073`);
    for (const session of sessions) {
      for (const row of query.iterate(session, ...families.map(family => `${family}%`))) {
        if (++scanned > 131072) throw new Error('Operational report exceeds 131072 events; narrow the session set');
        if (typeof row.id !== 'string' || typeof row.type !== 'string' || typeof row.data !== 'string'
            || !Number.isSafeInteger(row.seq) || row.seq < 0 || !Number.isSafeInteger(row.created)) {
          throw new Error('Operational event is malformed or exceeds its byte bound');
        }
        bytes += Buffer.byteLength(row.data);
        if (bytes > 64 * 1024 * 1024) throw new Error('Operational report exceeds 64 MiB of event data');
        if (!supported.has(row.type)) { unsupported.set(row.type, (unsupported.get(row.type) ?? 0) + 1); continue; }
        const data = JSON.parse(row.data);
        if (!data || typeof data !== 'object' || Array.isArray(data)) throw new Error(`Invalid ${row.type} payload`);
        events.push({ ...row, session, data });
      }
    }
    return { events, coverage: { events_read: scanned, event_bytes: bytes,
      unsupported_event_versions: Object.fromEntries(unsupported),
      basis: 'durable operational history, including rewound work; successive read-only snapshots' } };
  } finally { db.close(); }
}
