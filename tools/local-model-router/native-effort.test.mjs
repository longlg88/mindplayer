import test from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { copyFileSync, chmodSync, mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { openAppServer } from './app-server.mjs';
import { childEnvironment } from './service.mjs';

// Explicit opt-in: two tiny, real Codex inference turns, never a user's thread.
// This establishes native configuration application, not serving attestation
// or Jev judgment quality. The separate router tests establish pair selection.
test('native app-server records effort-only changes on the same model and thread', {
  skip: process.env.MINDPLAYER_TEST_NATIVE_EFFORT !== '1', timeout: 120_000,
}, async t => {
  assert.ok(process.env.MINDPLAYER_TEST_CODEX_HOME, 'explicit account home required');
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'mindplayer-native-effort-')));
  const pending = new Map();
  let transport;
  const stop = () => {
    transport?.close();
    for (const waiter of pending.values()) waiter.reject(new Error('Test closed'));
    pending.clear();
  };
  t.signal.addEventListener('abort', stop, { once: true });
  // Register before setup: even a failure immediately after copying auth must
  // remove the private copy. Teardown also runs when the test is cancelled.
  t.after(async () => {
    stop();
    t.signal.removeEventListener('abort', stop);
    // The owned process-group transport escalates after 1.5 seconds.
    await delay(1800);
    rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  });
  const home = join(root, 'codex');
  mkdirSync(home, { mode: 0o700 });
  // Isolate all Codex state and repo instructions, retaining only authentication.
  copyFileSync(join(process.env.MINDPLAYER_TEST_CODEX_HOME, 'auth.json'), join(home, 'auth.json'));
  chmodSync(join(home, 'auth.json'), 0o600);
  const env = { ...childEnvironment(process.env), CODEX_HOME: home };
  transport = openAppServer({ cwd: root, env, signal: t.signal });
  const terminal = new Map();
  let nextId = 0;
  transport.events.on('message', message => {
    if (message.method === 'turn/completed') {
      const turn = message.params.turn;
      terminal.set(turn.id, turn);
    }
    if (message.method && message.id !== undefined) {
      transport.send({ id: message.id, error: { code: -32601, message: 'No tools or approvals in this isolated test' } });
    } else if (!message.method && pending.has(message.id)) {
      const waiter = pending.get(message.id);
      pending.delete(message.id);
      if (message.error) waiter.reject(new Error(`Native RPC failed (${message.error.code})`));
      else waiter.resolve(message.result);
    }
  });
  const request = (method, params) => new Promise((resolve, reject) => {
    if (t.signal.aborted) { reject(new Error('Test cancelled')); return; }
    const id = ++nextId;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Native ${method} timed out`)); }, 30_000);
    pending.set(id, { resolve: result => { clearTimeout(timer); resolve(result); }, reject: error => { clearTimeout(timer); reject(error); } });
    transport.send({ id, method, params });
  });
  await request('initialize', { clientInfo: { name: 'mindplayer_native_effort_test', version: '1' } });
  transport.send({ method: 'initialized', params: {} });
  const { data } = await request('model/list', { limit: 100 });
  const candidates = data.filter(row => !row.hidden && ['medium', 'high'].every(effort => row.supportedReasoningEfforts?.some(value => value.reasoningEffort === effort)));
  const model = candidates.find(row => row.model === 'gpt-5.6-luna') ?? candidates.find(row => row.isDefault) ?? candidates[0];
  assert.ok(model, 'native catalog must support both efforts');
  const started = await request('thread/start', { model: model.model, cwd: root, approvalPolicy: 'never', sandbox: 'read-only',
    developerInstructions: 'For this configuration test, answer only OK. Do not use tools or subagents.', ephemeral: false });
  const threadId = started.thread.id;
  const results = [];
  for (const effort of ['high', 'medium']) {
    const { turn } = await request('turn/start', { threadId, model: model.model, effort, input: [{ type: 'text', text: 'Reply only OK.' }] });
    const signal = AbortSignal.any([t.signal, AbortSignal.timeout(30_000)]);
    while (!terminal.has(turn.id)) await once(transport.events, 'message', { signal });
    const outcome = terminal.get(turn.id);
    assert.equal(outcome.status, 'completed', 'native inference must complete');
    const state = await request('thread/read', { threadId, includeTurns: false });
    const path = state.thread.path ?? started.thread.path;
    assert.ok(path?.startsWith(home + '/'), 'rollout must belong to the isolated account home');
    const contexts = readFileSync(path, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line)).filter(row => row.type === 'turn_context');
    const context = contexts.find(row => row.payload.turn_id === turn.id)?.payload;
    assert.ok(context, 'actual native turn_context required');
    assert.equal(context.model, model.model);
    assert.equal(context.effort, effort);
    assert.equal(context.collaboration_mode?.settings?.reasoning_effort, effort);
    results.push({ turnId: turn.id, model: context.model, effort: context.effort, collaborationEffort: context.collaboration_mode.settings.reasoning_effort, outcome: outcome.status });
  }
  console.log(JSON.stringify({ nativeConfigurationProof: results, sameThread: true, actualInference: true, servingAttestation: false }));
});
