import { constants, openSync, closeSync, fstatSync, readSync, writeFileSync, renameSync, unlinkSync } from 'node:fs';
import { randomUUID } from 'node:crypto';

const identifier = value => typeof value === 'string' && /^[a-zA-Z0-9._:/-]{1,128}$/.test(value);
const requestId = value => Number.isSafeInteger(value) || identifier(value);

/** Pane-owned, bounded metadata only. Never write native terminal escape codes,
 * prompts, assistant output, account identifiers, keys, or upstream errors. */
export function createStatusSink(path) {
  if (!path) return () => {};
  let fd;
  let state;
  try {
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size > 4096 || (stat.mode & 0o077) || (process.getuid && stat.uid !== process.getuid())) throw new Error();
    const bytes = Buffer.alloc(4097);
    const count = readSync(fd, bytes, 0, bytes.length, 0);
    if (count > 4096) throw new Error();
    const initial = JSON.parse(bytes.subarray(0, count).toString('utf8'));
    if (initial.schemaVersion !== 2 || !identifier(initial.generation) || initial.revision !== 0 || initial.stage !== 'ready') throw new Error();
    state = { schemaVersion: 2, generation: initial.generation, revision: 0, stage: 'ready', threadId: null,
      requestId: null, turnId: null, model: null, effort: null, observedModel: null, reason: null, serverModelReported: false };
  } catch { throw new Error('Invalid pane status channel'); }
  finally { if (fd !== undefined) closeSync(fd); }
  let owned = null;
  let submitted = false;
  const terminal = stage => ['completed', 'failed', 'interrupted', 'closed'].includes(stage);
  // Bound stale-turn protection to recent pane-local history, not transcripts.
  const retiredTurns = new Set();
  const retire = turnId => {
    if (!identifier(turnId)) return;
    retiredTurns.add(turnId);
    if (retiredTurns.size > 512) retiredTurns.delete(retiredTurns.values().next().value);
  };
  const correlated = event => identifier(event.turnId) && !retiredTurns.has(event.turnId)
    && (event.requestId === undefined || event.requestId === state.requestId)
    && (state.turnId !== null ? event.turnId === state.turnId
      : submitted && state.stage === 'requested' && event.requestId === state.requestId);
  const publish = patch => {
    const next = { ...state, ...patch, revision: state.revision + 1 };
    const temporary = `${path}.${randomUUID()}.tmp`;
    try {
      writeFileSync(temporary, JSON.stringify(next), { flag: 'wx', mode: 0o600 });
      renameSync(temporary, path);
      state = next;
    } finally { try { unlinkSync(temporary); } catch { /* renamed or not created */ } }
  };
  return event => {
    if (event.event === 'thread-ownership' && event.owned === true && identifier(event.threadId)) {
      if (owned === event.threadId) return;
      owned = event.threadId;
      submitted = false;
      retiredTurns.clear();
      publish({ stage: 'ready', threadId: owned, requestId: null, turnId: null, model: null, effort: null,
        observedModel: null, serverModelReported: false, reason: null });
      return;
    }
    if (event.event === 'connection-closed-no-replay') {
      owned = null;
      publish({ stage: 'closed', reason: 'connection-closed' });
      return;
    }
    if (!owned || event.threadId !== owned) return;
    if (event.event === 'thread-closed') {
      retire(state.turnId);
      submitted = false;
      owned = null;
      publish({ stage: 'closed', reason: 'thread-closed' });
    } else if (event.event === 'route-deciding' || event.event === 'jev-started') {
      if (!requestId(event.requestId)) return;
      if (state.requestId === event.requestId && state.stage === 'judging') return;
      retire(state.turnId);
      submitted = false;
      publish({ stage: 'judging', requestId: requestId(event.requestId) ? event.requestId : null,
        turnId: null, model: null, effort: null, observedModel: null, serverModelReported: false, reason: null });
    } else if (event.event === 'model-selected' || event.event === 'route-requested' || event.event === 'route-skipped') {
      if (event.requestId !== state.requestId || !['judging', 'requested'].includes(state.stage)) return;
      if (event.model != null && !identifier(event.model)) return;
      if (event.event !== 'model-selected') submitted = true;
      publish({ stage: 'requested', model: identifier(event.model) ? event.model : null, effort: identifier(event.effort) ? event.effort : null,
        reason: identifier(event.reason) ? event.reason : null });
    } else if (event.event === 'start-acknowledged') {
      if (event.requestId !== state.requestId || !correlated(event) || terminal(state.stage)) return;
      // Notifications may precede the JSON-RPC reply. An ACK must never undo
      // actual execution, completion, or a later request's lifecycle.
      if (state.stage === 'running') return;
      publish({ stage: 'accepted', turnId: event.turnId });
    } else if (event.event === 'turn-started') {
      if (!correlated(event) || terminal(state.stage)) return;
      publish({ stage: 'running', turnId: event.turnId });
    } else if (event.event === 'model/rerouted') {
      // safetyBuffering.model and turn_context.model are configured values,
      // not serving receipts. Reroute is supplementary server-change telemetry,
      // never a prerequisite for a normal turn to be running or completed.
      const model = event.toModel;
      if (event.turnId !== state.turnId || !identifier(model) || !['accepted', 'running', 'completed'].includes(state.stage)) return;
      publish({ observedModel: model, serverModelReported: true });
    } else if (event.event === 'turn-completed') {
      if (!correlated(event) || terminal(state.stage) || !['completed', 'interrupted', 'failed'].includes(event.status)) return;
      publish({ stage: event.status === 'completed' ? 'completed' : event.status === 'interrupted' ? 'interrupted' : 'failed',
        turnId: event.turnId,
        reason: event.status === 'failed' ? 'execution-failed' : state.reason });
      retire(event.turnId);
    } else if (event.event === 'route-cancelled') {
      if (event.requestId !== state.requestId || !['judging', 'requested'].includes(state.stage) || submitted) return;
      submitted = false;
      publish({ stage: 'interrupted', reason: 'user-cancelled' });
    } else if (event.event === 'route-failed' || event.event === 'start-rejected') {
      if (event.requestId !== undefined && event.requestId !== state.requestId) return;
      publish({ stage: 'failed', reason: identifier(event.reason) ? event.reason : 'submission-rejected' });
    }
  };
}
