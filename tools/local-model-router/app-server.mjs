import { spawn } from 'node:child_process';
import { EventEmitter } from 'node:events';

export const APP_SERVER_INPUT_MAX_BYTES = 64 * 1024 * 1024;
export const APP_SERVER_JSONL_MAX_LINE_BYTES = APP_SERVER_INPUT_MAX_BYTES;
const UNSUPPORTED_SERVER_REQUEST_CODE = -32601;

// JSONL transport only; credentials stay in Codex's existing environment/home.
export function openAppServer({ binary = 'codex', cwd, env = process.env, args = [], maxBytes = APP_SERVER_INPUT_MAX_BYTES, signal } = {}) {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1 || maxBytes > APP_SERVER_INPUT_MAX_BYTES) throw new Error('Invalid transport size bound');
  const events = new EventEmitter();
  const child = spawn(binary, [...args, 'app-server', '--listen', 'stdio://'], {
    cwd, env, stdio: ['pipe', 'pipe', 'pipe'], detached: process.platform !== 'win32',
  });
  let parts = [];
  let bufferBytes = 0;
  let queuedBytes = 0;
  let stopped = false;
  let abortHandler;
  let killTimer;
  const signalOwnedProcessGroup = signal => {
    if (!child.pid) return;
    try {
      if (process.platform !== 'win32') process.kill(-child.pid, signal);
      else child.kill(signal);
    } catch (error) {
      if (error.code !== 'ESRCH') throw error;
    }
  };
  const fail = (reason = 'closed-by-client') => {
    if (stopped) return;
    stopped = true;
    if (abortHandler) signal?.removeEventListener('abort', abortHandler);
    child.stdin.destroy();
    signalOwnedProcessGroup('SIGTERM');
    killTimer = setTimeout(() => signalOwnedProcessGroup('SIGKILL'), 1500);
    events.emit('closed', typeof reason === 'string' ? reason : 'child-process-error');
  };
  child.stdout.on('data', chunk => {
    if (stopped) return;
    // Scan each byte once, including multi-megabyte plugin catalog lines.
    for (let offset = 0; offset < chunk.length;) {
      const newline = chunk.indexOf(10, offset);
      const end = newline === -1 ? chunk.length : newline;
      bufferBytes += end - offset;
      if (bufferBytes > maxBytes) return fail('jsonl-size-limit');
      parts.push(chunk.subarray(offset, end));
      if (newline === -1) return;
      const line = Buffer.concat(parts, bufferBytes).toString('utf8');
      parts = []; bufferBytes = 0; offset = newline + 1;
      if (!line.trim()) continue;
      try { events.emit('message', JSON.parse(line)); }
      catch { fail('invalid-jsonl-or-handler'); return; }
    }
  });
  // Drain diagnostics without saving auth details, prompts, or tool output.
  child.stderr.resume();
  child.on('error', fail);
  child.stdin.on('error', fail);
  child.on('exit', (code, signal) => {
    if (!stopped) fail(`child-exit-${code ?? signal}`);
  });
  if (signal) {
    abortHandler = () => fail('aborted');
    if (signal.aborted) abortHandler();
    else signal.addEventListener('abort', abortHandler, { once: true });
  }
  return {
    events,
    send(message) {
      if (stopped || child.stdin.destroyed) throw new Error('Codex connection closed');
      const line = JSON.stringify(message) + '\n';
      const bytes = Buffer.byteLength(line);
      if (bytes > maxBytes || queuedBytes + bytes > maxBytes) {
        fail('input-size-limit'); throw new Error('Codex input queue exceeded');
      }
      queuedBytes += bytes;
      child.stdin.write(line, () => { queuedBytes -= bytes; });
    },
    close: fail,
  };
}

export async function probeModels(options = {}) {
  const transport = openAppServer(options);
  let nextId = 0;
  const pending = new Map();
  transport.events.on('message', message => {
    if (message.method) {
      if (message.id !== undefined) {
        transport.send({ id: message.id, error: {
          code: UNSUPPORTED_SERVER_REQUEST_CODE,
          message: 'Server-initiated RPC requests are unsupported by the model probe',
        } });
      }
      return;
    }
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    if (message.error) waiter.reject(new Error(`Codex RPC failed (${message.error.code})`));
    else waiter.resolve(message.result);
  });
  const request = (method, params) => new Promise((resolve, reject) => {
    const id = ++nextId;
    pending.set(id, { resolve, reject });
    transport.send({ id, method, params });
  });
  const timer = setTimeout(() => transport.close(), 20_000);
  transport.events.on('closed', () => {
    for (const waiter of pending.values()) waiter.reject(new Error('Codex probe closed/timed out'));
    pending.clear();
  });
  try {
    await request('initialize', { clientInfo: { name: 'mindplayer_local_router_probe', title: 'Local router probe', version: '0.1.0' } });
    transport.send({ method: 'initialized', params: {} });
    const models = [];
    let cursor = null;
    do {
      const page = await request('model/list', { cursor, limit: 100 });
      if (!Array.isArray(page.data)) throw new Error('Unexpected model/list response');
      models.push(...page.data);
      cursor = page.nextCursor;
      if (models.length > 1000) throw new Error('Model catalog exceeds PoC limit');
    } while (cursor);
    if (options.includeAccount) {
      // Read-only identity evidence; never write email/auth data to diagnostics.
      const result = await request('account/read', {});
      return { catalog: models, account: result?.account ?? null };
    }
    return models;
  } finally { clearTimeout(timer); transport.close(); }
}
