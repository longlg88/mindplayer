import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { channel } from 'node:diagnostics_channel';
import { EventEmitter } from 'node:events';
import { chmodSync, copyFileSync, mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, sep } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

// Manual opt-in only. This spends two Jev judgments and two tiny native turns:
// MINDPLAYER_TEST_NATIVE_ROUTING=1 MINDPLAYER_TEST_CODEX_HOME=/absolute/account/home \
//   node --test tools/local-model-router/native-routing.test.mjs
// Optional MINDPLAYER_NATIVE_CODEX selects the installed native executable.
// Authentication is copied, never moved; source config/sessions are not copied.
// Chosen: real JevService + ModelRouter + stdio app-server + native rollout.
// Effort-only probes omit Jev; a simulated backend cannot prove native settings.
// This is two ORDINARY sequential turns, independent of the native PTY fixture.
// It does not test busy input, steering, model serving identity, judgment quality,
// cost savings, or guarantee that the two real Jev selections will differ.
const NATIVE_VERSION = 'codex-cli 0.162.1';
const PROMPT = 'Reply only OK.';
const INSTRUCTIONS = 'Reply only OK. Do not use tools, commands, web search, skills, or subagents.';

function until(events, check, label, timeoutMs = 60_000) {
  return new Promise((resolve, reject) => {
    let timer;
    const finish = (error, value) => {
      clearTimeout(timer);
      events.off('change', inspect);
      if (error) reject(error); else resolve(value);
    };
    const inspect = () => {
      try { const value = check(); if (value) finish(null, value); }
      catch (error) { finish(error); }
    };
    timer = setTimeout(() => finish(new Error(`Timed out: ${label}`)), timeoutMs);
    events.on('change', inspect);
    inspect();
  });
}

test('real Jev model and effort reach two ordinary native turns and their rollout contexts', {
  skip: process.env.MINDPLAYER_TEST_NATIVE_ROUTING !== '1', timeout: 210_000,
}, async t => {
  const sourceHome = process.env.MINDPLAYER_TEST_CODEX_HOME;
  assert.ok(sourceHome && isAbsolute(sourceHome), 'MINDPLAYER_TEST_CODEX_HOME must explicitly name an absolute account home');
  assert.notEqual(process.platform, 'win32', 'owned POSIX process-group cleanup is required');
  const [{ openAppServer }, { createJevClient }, { ModelRouter }, { childEnvironment, JevService, loadJevCredentials }] = await Promise.all([
    import('./app-server.mjs'), import('./jev.mjs'), import('./router.mjs'), import('./service.mjs'),
  ]);
  const credential = loadJevCredentials();
  assert.ok(credential?.apiKey && credential.externalPromptTransfer, 'existing service-side Jev credential and transfer consent required');
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'mindplayer-native-routing-')));
  const home = join(root, 'codex'), userHome = join(root, 'user'), cwd = join(root, 'work');
  let transport, router, child, childClosed;
  let closing = false, failure, sequence = 0, nextId = 0;
  const events = new EventEmitter(), pending = new Map();
  const sent = [], starts = [], terminals = [], answers = [], audit = [], judgments = [];
  const mark = value => ({ sequence: ++sequence, ...value });
  const fail = error => {
    failure ??= error;
    for (const waiter of pending.values()) waiter.reject(failure);
    pending.clear();
    events.emit('change');
  };
  t.after(async () => {
    closing = true;
    router?.close();
    transport?.close();
    fail(new Error('Native routing test closed'));
    if (child) {
      // closed on the transport means shutdown STARTED, not process exit.
      // Observe the actual child, then wait past the transport's owned-group
      // SIGKILL timer before checking descendants and removing this exact tmp.
      let closeTimer;
      try {
        await Promise.race([childClosed, new Promise((_, reject) => {
          closeTimer = setTimeout(() => reject(new Error('Owned native process did not close; isolated tmp retained')), 10_000);
        })]);
      } finally { clearTimeout(closeTimer); }
      await delay(1800);
      if (child.pid) {
        let absent = false;
        for (let attempt = 0; attempt < 50; attempt++) {
          try { process.kill(-child.pid, 0); }
          catch (error) { if (error.code === 'ESRCH') { absent = true; break; } throw new Error('Could not verify owned native process-group exit; isolated tmp retained'); }
          await delay(20);
        }
        assert.ok(absent, 'owned native process group must exit before deleting isolated tmp');
      }
    }
    rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  });
  for (const path of [home, userHome, cwd, join(root, 'tmp')]) mkdirSync(path, { mode: 0o700 });
  copyFileSync(join(realpathSync(sourceHome), 'auth.json'), join(home, 'auth.json'));
  chmodSync(join(home, 'auth.json'), 0o600);
  // An allowlist, not inherited credentials/configuration. Jev's key remains
  // in this process; the existing macOS Jev client uses system trust and stdin
  // for curl configuration, with an empty child environment and no key argv.
  const env = childEnvironment({ PATH: `${dirname(process.execPath)}:/opt/homebrew/bin:/usr/bin:/bin`,
    HOME: userHome, CODEX_HOME: home, TMPDIR: join(root, 'tmp') + sep,
    XDG_CONFIG_HOME: join(userHome, '.config'), XDG_CACHE_HOME: join(userHome, '.cache'),
    XDG_DATA_HOME: join(userHome, '.local', 'share'), LANG: 'en_US.UTF-8', RUST_BACKTRACE: '0' });
  assert.ok(!Object.values(env).some(value => value.includes(credential.apiKey)), 'Jev key must not enter native child environment');
  const binary = process.env.MINDPLAYER_NATIVE_CODEX ?? '/opt/homebrew/bin/codex';
  let version;
  try { version = execFileSync(binary, ['--version'], { env, encoding: 'utf8', timeout: 10_000, stdio: ['ignore', 'pipe', 'pipe'] }).trim(); }
  catch { throw new Error('Could not verify installed native Codex version'); }
  assert.equal(version, NATIVE_VERSION, 'test is pinned to native 0.162.1');
  const disabled = ['shell_tool', 'unified_exec', 'shell_snapshot', 'multi_agent', 'apps', 'plugins',
    'browser_use', 'computer_use', 'image_generation', 'view_image', 'sleep_tool', 'goals', 'hooks'];
  const args = ['-c', 'cli_auth_credentials_store="file"', '-c', 'web_search="disabled"',
    ...disabled.flatMap(name => ['-c', `features.${name}=false`])];
  // Node publishes the real ChildProcess at construction. Capture only the
  // synchronous openAppServer spawn; do not replace transport/spawn or retain
  // unrelated processes (including the service's system-trust HTTP transport).
  const spawnChannel = channel('child_process');
  const capture = value => {
    child = value.process;
    childClosed = new Promise(resolve => child.once('close', resolve));
  };
  spawnChannel.subscribe(capture);
  try { transport = openAppServer({ binary, cwd, env, args }); }
  finally { spawnChannel.unsubscribe(capture); }
  assert.ok(child && child.spawnargs?.includes('app-server'), 'actual owned app-server child must be observable for cleanup');

  const receive = message => {
    if (message.method) {
      if (message.id !== undefined) {
        // No approval/tool RPC may execute in this isolated inference test.
        router ? router.client({ id: message.id, error: { code: -32601, message: 'Tools and approvals are forbidden in this test' } })
          : transport.send({ id: message.id, error: { code: -32601, message: 'Tools and approvals are forbidden in this test' } });
        fail(new Error('Native requested a tool or approval in the no-tools test'));
      }
      return;
    }
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    if (message.error) {
      const failed = audit.findLast(row => row.requestId === message.id && ['route-failed', 'jev-failed', 'start-rejected'].includes(row.event));
      const reason = /^[a-z0-9-]{1,64}$/.test(failed?.reason) ? failed.reason : 'upstream-rejection';
      waiter.reject(new Error(`Native RPC rejected (${reason}); upstream error payload suppressed`));
    }
    else waiter.resolve(message.result);
  };
  transport.events.on('message', message => {
    if (closing) return;
    try {
      if (message.method === 'turn/started') starts.push(mark({ threadId: message.params.threadId, turnId: message.params.turn.id }));
      if (message.method === 'turn/completed') terminals.push(mark({ threadId: message.params.threadId,
        turnId: message.params.turn.id, status: message.params.turn.status }));
      if (message.method === 'item/started' || message.method === 'item/completed') {
        const item = message.params.item;
        if (!['userMessage', 'agentMessage', 'reasoning'].includes(item?.type)) throw new Error('Native emitted a tool item in the no-tools test');
        if (message.method === 'item/completed' && item.type === 'agentMessage' && item.phase !== 'commentary') {
          answers.push({ turnId: message.params.turnId, text: item.text });
        }
      }
      if (router) router.server(message); else receive(message);
      events.emit('change');
    } catch { fail(new Error('Native protocol or no-tools verification failed; upstream payload suppressed')); }
  });
  transport.events.on('closed', () => { if (!closing) fail(new Error('Native transport closed unexpectedly; no replay')); });
  const submit = (method, params) => {
    const id = ++nextId;
    const promise = new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Native ${method} timed out; no retry`)); }, 65_000);
      pending.set(id, { resolve: value => { clearTimeout(timer); resolve(value); }, reject: error => { clearTimeout(timer); reject(error); } });
      try { if (failure) throw failure; if (router) router.client({ id, method, params }); else transport.send({ id, method, params }); }
      catch { pending.get(id)?.reject(new Error('Native request could not be sent; no retry')); pending.delete(id); }
    });
    return { id, promise };
  };
  const request = (method, params) => submit(method, params).promise;
  await request('initialize', { clientInfo: { name: 'mindplayer_native_routing_test', version: '1' }, capabilities: { experimentalApi: true } });
  transport.send({ method: 'initialized', params: {} });
  const catalog = [];
  let cursor = null;
  do {
    const page = await request('model/list', { cursor, limit: 100 });
    assert.ok(Array.isArray(page.data), 'actual native catalog required');
    catalog.push(...page.data);
    assert.ok(catalog.length <= 1000, 'native catalog size bound');
    cursor = page.nextCursor;
  } while (cursor);
  const { account } = await request('account/read', {});
  assert.equal(account?.type, 'chatgpt', 'existing subscription login required');
  const candidates = catalog.filter(row => !row.hidden && row.inputModalities?.includes('text')
    && ['medium', 'high'].every(effort => row.supportedReasoningEfforts?.some(value => value.reasoningEffort === effort)));
  const initial = candidates.find(row => row.isDefault) ?? candidates[0];
  assert.ok(initial, 'actual catalog must offer a text model with medium and high baselines');
  const record = event => {
    // Fixed safe fields only; no accounts, credential objects, raw RPCs or bodies.
    const row = { event: event.event };
    for (const key of ['requestId', 'threadId', 'turnId', 'model', 'effort', 'reason', 'owned', 'startup']) {
      if (event[key] !== undefined) row[key] = event[key];
    }
    audit.push(mark(row));
  };
  const service = new JevService({ catalog, accountHome: home, account, ...credential, policyReader: () => null,
    judgmentLimits: { timeoutMs: 30_000 }, requirementsForTurn: () => ({ provider: 'codex', inputModalities: ['text'], tools: [] }),
    // Transparent observation of the real validated response, never a supplied
    // judgment/fetch mock. Full native catalog; no fabricated candidates or tiers.
    judgeFactory: options => {
      const judge = createJevClient(options);
      return async (context, options) => {
        const row = mark({ promptMatches: context.currentInput === PROMPT });
        judgments.push(row);
        const result = await judge(context, options);
        Object.assign(row, { returned: true, model: result.configuration?.model, effort: result.configuration?.effort });
        return result;
      };
    }, audit: record });
  router = new ModelRouter({ catalog, service, audit: record, decisionTimeoutMs: 31_000, toClient: receive,
    toServer: message => { if (message.method === 'turn/start') sent.push(mark({ id: message.id, params: structuredClone(message.params) })); transport.send(message); } });
  const started = await request('thread/start', { model: initial.model, cwd, approvalPolicy: 'never', sandbox: 'read-only',
    developerInstructions: INSTRUCTIONS, ephemeral: false, dynamicTools: [], environments: [],
    sessionStartSource: 'startup', threadSource: 'user' });
  const threadId = started.thread.id;
  assert.equal(service.threads.get(threadId)?.owned, true, 'ownership must come from explicit startup request and actual backend lifecycle');
  assert.equal(audit.filter(row => row.event === 'thread-ownership' && row.threadId === threadId && row.owned && row.startup).length, 1);
  const proof = [];
  for (const [index, baselineEffort] of ['high', 'medium'].entries()) {
    const ticket = submit('turn/start', { threadId, model: initial.model, effort: baselineEffort,
      input: [{ type: 'text', text: PROMPT }], cwd, approvalPolicy: 'never', sandboxPolicy: { type: 'readOnly', networkAccess: false },
      collaborationMode: { mode: 'default', settings: { model: initial.model, reasoning_effort: baselineEffort, developer_instructions: INSTRUCTIONS } } });
    const { turn } = await ticket.promise;
    const terminal = await until(events, () => {
      if (failure) throw failure;
      return terminals.find(row => row.turnId === turn.id);
    }, 'actual native turn completion');
    assert.equal(terminal.status, 'completed', 'real native inference must complete');
    const actualStart = starts.filter(row => row.turnId === turn.id);
    assert.equal(actualStart.length, 1, 'one actual turn/started per request');
    assert.equal(actualStart[0].threadId, threadId);
    assert.ok(actualStart[0].sequence < terminal.sequence, 'start must precede actual terminal');
    if (index) assert.ok(terminals[0].sequence < judgments[index].sequence, 'second ordinary turn is judged after first terminal');
    assert.match(answers.filter(row => row.turnId === turn.id).map(row => row.text).join('').trim(), /^OK[.!]?$/, 'actual bounded assistant output required');
    const selected = audit.filter(row => row.event === 'model-selected' && row.requestId === ticket.id);
    assert.equal(selected.length, 1, 'exactly one real service selection per request');
    assert.equal(selected[0].reason, 'catalog-guided-jev', 'no baseline fallback may masquerade as a Jev configuration');
    assert.equal(judgments.length, index + 1);
    assert.equal(judgments[index].returned, true, 'real Jev call must return a validated judgment');
    assert.equal(judgments[index].promptMatches, true);
    const forwarded = sent.filter(row => row.id === ticket.id);
    assert.equal(forwarded.length, 1, 'no duplicate/replay to native');
    const params = forwarded[0].params;
    for (const pair of [selected[0], params, { model: params.collaborationMode.settings.model, effort: params.collaborationMode.settings.reasoning_effort }]) {
      assert.equal(pair.model, judgments[index].model);
      assert.equal(pair.effort, judgments[index].effort);
    }
    const state = await request('thread/read', { threadId, includeTurns: false });
    const path = realpathSync(state.thread.path ?? started.thread.path);
    const withinHome = relative(home, path);
    assert.ok(withinHome && !withinHome.startsWith('..' + sep) && withinHome !== '..' && !isAbsolute(withinHome), 'rollout must be inside isolated CODEX_HOME');
    const contexts = readFileSync(path, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line)).filter(row => row.type === 'turn_context' && row.payload.turn_id === turn.id);
    assert.equal(contexts.length, 1, 'one actual native turn_context for the acknowledged turn');
    const context = contexts[0].payload;
    assert.equal(context.model, judgments[index].model);
    assert.equal(context.effort, judgments[index].effort);
    assert.equal(context.collaboration_mode?.settings?.model, judgments[index].model);
    assert.equal(context.collaboration_mode?.settings?.reasoning_effort, judgments[index].effort);
    assert.equal(context.approval_policy, 'never');
    assert.equal(context.sandbox_policy?.type, 'read-only');
    proof.push({ requestId: ticket.id, turnId: turn.id, threadId, model: context.model, effort: context.effort,
      collaborationModel: context.collaboration_mode.settings.model, collaborationEffort: context.collaboration_mode.settings.reasoning_effort, status: terminal.status });
  }
  assert.equal(new Set(proof.map(row => row.turnId)).size, 2, 'two distinct actual turns on one thread');
  assert.equal(sent.length, 2);
  assert.equal(starts.length, 2);
  assert.equal(terminals.length, 2);
  for (const event of ['jev-started', 'jev-judged', 'model-selected', 'route-requested', 'start-acknowledged']) {
    assert.equal(audit.filter(row => row.event === event).length, 2, `exactly two ${event} events`);
  }
  assert.equal(audit.filter(row => ['route-skipped', 'route-failed', 'jev-failed', 'start-rejected'].includes(row.event)).length, 0);
  t.diagnostic(JSON.stringify({ nativeVersion: version, provider: credential.provider, catalogSource: 'actual model/list',
    realJev: true, actualInference: true, ordinarySequentialTurns: true, busyInputProven: false, servingAttestation: false, nativeRoutingProof: proof }));
});
