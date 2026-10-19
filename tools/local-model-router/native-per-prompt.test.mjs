import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { createNativeQueueFixture, MODEL, OTHER_MODEL, NATIVE_CONTRACT, THREAD_ID } from './native-queue-fixture.mjs';

// Opt in ONLY after main freezes the Node consumers and supplies the v4 native
// binary. This lane never builds a CLI, reads a login, or calls external Jev.
// MINDPLAYER_TEST_NATIVE_PER_PROMPT=1 MINDPLAYER_NATIVE_CODEX=/absolute/frozen/codex \
//   node --test tools/local-model-router/native-per-prompt.test.mjs
// Native input/queue, default keymap, proxy, ModelRouter and JevService are real.
// Backend lifecycle/approval replies and the judge are synthetic. These payloads
// prove applied request configuration, not inference or backend turn_context.
// Retained .ansi files are actual PTY bytes; .screen.txt is a 40x120 text-cell
// buffer, without OS styling/IME proof. The .txt file is cumulative output.
const options = { skip: process.env.MINDPLAYER_TEST_NATIVE_PER_PROMPT !== '1', timeout: 45_000 };
const F6 = '\x1b[17~', F7 = '\x1b[18~';
const labels = { jev: '[F6] Jev ON · 외부 전송 동의', native: '[F6] Jev OFF · 기본 모델 유지' };
const promptText = request => request.params.input.filter(item => item.type === 'text').map(item => item.text).join('\n');
const submissions = f => f.decisions.map(row => row.request);
const packagedNative = () => readFileSync(new URL('../native-codex/queue.patch', import.meta.url), 'utf8');
const addedNative = () => packagedNative().split('\n').filter(line => line.startsWith('+') && !line.startsWith('+++')).join('\n');

// Read-only REDs before a frozen candidate exists; these do not replace the
// actual native input/queue/identity/cleanup regressions below.
test('packaged native source advertises the strict v4 sticky-session contract', () => {
  const contracts = [...packagedNative().matchAll(/^\+.*\.then_some\("(mindplayer-native-routing-v\d+)"\)/gm)].map(match => match[1]);
  assert.deepEqual(contracts, ['mindplayer-native-routing-v4']);
});

test('packaged native source has both sticky ON and OFF labels', () => {
  const added = addedNative();
  for (const label of ['Jev ON · 외부 전송 동의', 'Jev OFF · 기본 모델 유지']) assert.ok(added.includes(label), label);
  assert.ok(!added.includes('Jev 미선택'), 'managed OFF must not be an unselected submission block');
});

test('packaged native source removes the retired Jev preview RPC and UI', () => {
  const retired = addedNative().match(/mindplayer\/prompt\/preview|MindplayerPreview|MindplayerPromptPreview|전송 문맥/);
  assert.equal(retired?.[0] ?? null, null, 'retired Jev preview must be removed from the packaged native source');
});

function assertMode(f, mode) {
  const screen = f.projectedScreen();
  assert.ok(screen.includes(labels[mode]), screen);
  assert.ok(!screen.includes(labels[mode === 'jev' ? 'native' : 'jev']), 'only the current session mode is shown');
  assert.doesNotMatch(screen, /Jev 미선택|\[F7\]|전송 문맥/);
}

function assertNoPreview(f) {
  assert.equal(f.clientRequests('mindplayer/prompt/preview').length, 0, 'F7 must emit no retired local RPC');
  assert.equal(f.messages.some(message => message.method === 'mindplayer/prompt/preview'), false, 'retired local RPC must never reach the backend');
}

async function boot(t, config = {}) {
  const f = await createNativeQueueFixture({ ...config, queue: false, perPrompt: true });
  t.after(async () => {
    let evidence;
    try { evidence = f.capture('final'); }
    finally {
      try { await f.close(); }
      finally {
        t.diagnostic(JSON.stringify({ native: f.binary, sha256: f.binarySha256, version: f.version, contract: f.contract,
          evidence, cleanup: f.cleanup, scope: 'real native PTY/proxy/ModelRouter/JevService; synthetic backend and judge; no auth/inference' }));
      }
    }
    assert.deepEqual(f.cleanup.alivePids, [], 'awaited teardown must reap the bridge and actual native child');
    assert.equal(f.cleanup.portClosed, true, 'the actual loopback listener must reject new connections');
    assert.equal(f.cleanup.rootRemoved, true, 'the isolated runtime home must be removed; evidence is retained separately');
    assert.deepEqual(f.unknown, [], 'no unknown backend method may be hidden by the fixture');
    assertNoPreview(f);
  });
  assert.equal(f.contract, NATIVE_CONTRACT);
  assert.equal(f.env.MINDPLAYER_PER_PROMPT_CHOICE, '1', 'retain the internal per-message metadata guard');
  assert.ok(!f.args.some(arg => arg.startsWith('tui.keymap.')), 'use the real default native keymap');
  await f.wait(() => f.messages.some(message => message.method === 'thread/start'), 'native thread handshake');
  await f.wait(() => f.projectedScreen().includes(MODEL) && f.projectedScreen().includes(labels.native), 'new session starts native OFF');
  assertMode(f, 'native');
  await f.captureStable('fresh-native-off');
  return f;
}

async function draft(f, text) {
  // Separate physical text entry from Enter; no injected RPC or fixture choice.
  await f.write(text);
  await f.wait(() => f.projectedScreen().includes('› ' + text.trimEnd()), 'directly typed composer draft');
}

async function choose(f, mode) {
  await f.wait(() => Object.values(labels).some(label => f.projectedScreen().includes(label)), 'current session toggle');
  // A same-mode request must not toggle and then repair the session state.
  if (!f.projectedScreen().includes(labels[mode])) {
    await f.write(F6);
    await f.wait(() => f.projectedScreen().includes(labels[mode]), 'F6 toggles to ' + mode);
  }
  assertMode(f, mode);
}

async function idle(f, mode) {
  await f.wait(() => f.projectedScreen().includes(labels[mode])
    && !/esc to interrupt/i.test(f.projectedScreen()), 'idle composer retains ' + mode);
  assertMode(f, mode);
}

async function submit(f, index) {
  await f.write('\r');
  const start = await f.waitStart(index);
  f.acknowledge(index); f.start(index);
  await f.wait(() => /esc to interrupt/i.test(f.projectedScreen()), 'native consumes turn/started and renders busy');
  return start;
}

async function running(f, text = 'active native prompt') {
  await draft(f, text);
  return submit(f, 0);
}

async function queued(f, text, count) {
  await f.write('\r');
  await f.wait(() => f.projectedScreen().includes('Queued follow-up inputs')
    && f.projectedScreen().split('\n').filter(line => line.includes('↳ ' + text)).length === count,
  'native accepts queued input ' + count);
  assert.equal(f.clientRequests('turn/start').length, 1, 'queue acceptance must not submit to the service');
  assert.equal(f.clientRequests('turn/steer').length, 0, 'busy Enter must not become steering');
}

function assertChoice(request, mode) {
  assert.ok(request.params.clientUserMessageId, 'toggle snapshot accompanies the actual native user-message identity');
  assert.deepEqual(request.params.mindplayerRoutingChoice, { mode, includeRecent: false });
}

function assertConfiguration(start, model, effort) {
  assert.equal(start.request.params.model, model);
  assert.equal(start.request.params.effort, effort);
  assert.equal(start.request.params.collaborationMode.settings.model, model);
  assert.equal(start.request.params.collaborationMode.settings.reasoning_effort, effort);
}

function assertIdentity(start, request) {
  assert.equal(start.request.id, request.id);
  assert.equal(start.request.params.clientUserMessageId, request.params.clientUserMessageId);
  assert.equal(Object.hasOwn(start.request.params, 'mindplayerRoutingChoice'), false, 'local consent is stripped before the backend');
  assert.equal(Object.hasOwn(start.request.params, 'previewId'), false, 'removed preview metadata cannot reach the backend');
}

function assertNative(start, request) {
  assertChoice(request, 'native');
  const expected = structuredClone(request);
  delete expected.params.mindplayerRoutingChoice;
  expected.params.mindplayerRequireNewTurn = true;
  assert.deepEqual(start.request, expected, 'OFF preserves every actual native parameter rather than guessing a baseline reset');
  assertIdentity(start, request);
}

test('managed OFF submits a directly typed prompt without F6 or judgment', options, async t => {
  const f = await boot(t, { select: () => { throw new Error('default OFF must not judge'); } });
  await draft(f, 'default native prompt');
  const start = await submit(f, 0);
  assertNative(start, submissions(f)[0]);
  assertConfiguration(start, MODEL, 'low');
  assert.equal(promptText(start.request), 'default native prompt');
  assert.equal(f.calls.length, 0);
  assert.equal(f.clientRequests('turn/start').length, 1);
  f.complete(0);
  await idle(f, 'native');
});

test('managed F6 toggles ON and OFF without submitting or modifying the typed draft', options, async t => {
  const f = await boot(t);
  const text = 'F6 only toggles this session';
  await draft(f, text);
  const offset = f.trace.length;
  for (const mode of ['jev', 'native']) {
    await f.write(F6);
    await f.wait(() => f.projectedScreen().includes(labels[mode]), 'F6 ' + mode);
    assertMode(f, mode);
    assert.ok(f.projectedScreen().includes('› ' + text));
    await f.captureStable('f6-' + mode);
  }
  await f.expectNoSubmission(300, offset);
  assert.equal(submissions(f).length, 0);
  assert.equal(f.calls.length, 0);
});

test('one F6 judges consecutive typed followups until the next F6 submits native without judgment', options, async t => {
  const f = await boot(t, { select: () => ({ model: OTHER_MODEL, effort: 'high' }) });
  await f.write(F6);
  await f.wait(() => f.projectedScreen().includes(labels.jev), 'single F6 enables the session');
  for (const [index, mode] of ['jev', 'jev', 'native'].entries()) {
    if (mode === 'native') {
      await f.write(F6);
      await f.wait(() => f.projectedScreen().includes(labels.native), 'next F6 disables the session');
    }
    await draft(f, 'direct prompt ' + (index + 1));
    assertMode(f, mode);
    const start = await submit(f, index), request = submissions(f)[index];
    assertChoice(request, mode); assertIdentity(start, request);
    assert.equal(promptText(start.request), 'direct prompt ' + (index + 1));
    if (mode === 'jev') assertConfiguration(start, OTHER_MODEL, 'high');
    else assertNative(start, request);
    f.complete(index);
    await idle(f, mode);
    await f.captureStable('followup-' + (index + 1) + '-' + mode);
  }
  assert.deepEqual(f.calls.map(call => call.text), ['direct prompt 1', 'direct prompt 2']);
  assert.equal(f.clientRequests('turn/start').length, 3);
  assert.equal(f.clientRequests('turn/steer').length, 0);
  assert.equal(new Set(submissions(f).map(request => request.params.clientUserMessageId)).size, 3);
});

test('managed native OFF preserves the complete incoming model and effort without any judgment', options, async t => {
  const f = await boot(t, { select: () => { throw new Error('native nonuse must not judge'); } });
  await draft(f, 'native configuration stays exact');
  await choose(f, 'native');
  const start = await submit(f, 0);
  assertNative(start, submissions(f)[0]);
  assertConfiguration(start, MODEL, 'low');
  assert.equal(f.calls.length, 0);
  f.complete(0);
});

test('managed live /plan snapshots ON and judges its normalized input and actual Plan baseline', options, async t => {
  const f = await boot(t, { plan: true, select: () => ({ model: MODEL, effort: 'low' }) });
  const text = 'actual live plan input';
  await choose(f, 'jev');
  await draft(f, '/plan   ' + text + '   ');
  const start = await submit(f, 0), request = submissions(f)[0];
  assertChoice(request, 'jev'); assertIdentity(start, request);
  assert.equal(promptText(request), text);
  assert.equal(request.params.input.length, 1);
  assert.equal(request.params.turnTrigger, 'user');
  assert.equal(request.params.collaborationMode.mode, 'plan');
  assertConfiguration({ request }, OTHER_MODEL, 'high');
  assert.equal(start.request.params.collaborationMode.mode, 'plan');
  assert.deepEqual(start.request.params.input, request.params.input);
  assertConfiguration(start, MODEL, 'low');
  assert.deepEqual(f.calls.map(call => call.text), [text]);
  assert.equal(f.calls[0].context.currentInput, text);
  assert.deepEqual(f.calls[0].context.baseline, { model: OTHER_MODEL, effort: 'high' });
  assert.deepEqual(f.calls[0].context.recent, []);
  const settings = await f.waitRequest('thread/settings/update');
  assert.equal(settings.params.collaborationMode.mode, 'plan');
  assert.equal(settings.params.collaborationMode.settings.model, OTHER_MODEL);
  assert.equal(settings.params.collaborationMode.settings.reasoning_effort, 'high');
  f.complete(0);
  await idle(f, 'jev');
});

test('managed queued /plan retains OFF despite the later composer switching ON', options, async t => {
  const f = await boot(t, { plan: true, select: () => { throw new Error('queued OFF must not borrow later ON'); } });
  await running(f);
  const text = 'actual queued plan input';
  await draft(f, '/plan   ' + text + '   ');
  await f.write('\r');
  await f.wait(() => f.projectedScreen().includes('Queued follow-up inputs')
    && f.projectedScreen().split('\n').some(line => line.includes('↳ ') && line.includes(text)), 'native accepts the queued Plan command');
  assert.equal(submissions(f).length, 1);
  await choose(f, 'jev');
  await draft(f, 'later composer keeps its session toggle');
  f.capture('queued-plan-later-composer');
  await f.expectNoSubmission();
  f.complete(0);

  const start = await f.waitStart(1), request = submissions(f)[1];
  assertNative(start, request);
  assert.notEqual(request.params.clientUserMessageId, submissions(f)[0].params.clientUserMessageId);
  assert.equal(promptText(request), text);
  assert.equal(request.params.input.length, 1);
  assert.equal(request.params.turnTrigger, 'user');
  assert.equal(request.params.collaborationMode.mode, 'plan');
  assertConfiguration({ request }, OTHER_MODEL, 'high');
  f.acknowledge(1); f.start(1);
  await f.wait(() => /esc to interrupt/i.test(f.projectedScreen())
    && f.projectedScreen().includes('later composer keeps its session toggle'), 'queued Plan starts without replacing the later draft');
  assertMode(f, 'jev');
  f.complete(1);
  await f.expectNoSubmission();
  assert.equal(f.clientRequests('turn/start').length, 2);
  assert.equal(f.clientRequests('turn/steer').length, 0);
  assert.equal(f.calls.length, 0);
});

test('managed /init submits OFF on the exact generated USER turn without a selection block', options, async t => {
  const f = await boot(t, { select: () => { throw new Error('native /init must not judge'); } });
  await draft(f, '/init');
  const start = await submit(f, 0), request = submissions(f)[0];
  assertNative(start, request);
  assert.equal(request.params.turnTrigger, 'user');
  assert.equal(request.params.input.length, 1);
  const generated = promptText(request);
  // Pin the genuine rust-v0.162.1 asset. Synthetic inference generates no file.
  assert.equal(Buffer.byteLength(generated), 1836);
  assert.equal(createHash('sha256').update(generated).digest('hex'), '70dfe903081ca894fbec7d80b40da2b5560a6106422fdfd0db2907dc9f5aa525');
  assertConfiguration(start, MODEL, 'low');
  assert.equal(f.calls.length, 0);
  f.capture('init-generated-native-turn');
  f.complete(0);
});

test('identical queued text snapshots ON ON OFF by identity despite a later ON toggle', options, async t => {
  const f = await boot(t, { select: () => ({ model: OTHER_MODEL, effort: 'high' }) });
  await running(f);
  const repeated = 'identical queued prompt';
  await choose(f, 'jev');
  for (let index = 0; index < 3; index++) {
    if (index === 2) await choose(f, 'native');
    await draft(f, repeated);
    await queued(f, repeated, index + 1);
  }
  await choose(f, 'jev');
  await draft(f, 'unsubmitted later draft');
  assert.equal(f.calls.length, 0, 'queued inputs are never judged early');
  f.capture('queued-on-on-off-later-on');

  for (let index = 1; index <= 3; index++) {
    f.complete(index - 1);
    const start = await f.waitStart(index), request = submissions(f)[index];
    assert.equal(promptText(start.request), repeated);
    assert.equal(start.request.params.turnTrigger, 'user');
    assertIdentity(start, request);
    assertChoice(request, index < 3 ? 'jev' : 'native');
    if (index < 3) assertConfiguration(start, OTHER_MODEL, 'high');
    else assertNative(start, request);
    f.acknowledge(index); f.start(index);
    await f.wait(() => /esc to interrupt/i.test(f.projectedScreen()), 'native queued turn ' + index + ' starts');
    assertMode(f, 'jev');
  }
  f.complete(3);
  await f.expectNoSubmission();
  assert.equal(new Set(submissions(f).map(request => request.params.clientUserMessageId)).size, 4);
  assert.deepEqual(f.calls.map(call => call.text), [repeated, repeated]);
  assert.equal(f.clientRequests('turn/start').length, 4);
  assert.equal(f.clientRequests('turn/steer').length, 0);
});

for (const mode of ['native', 'jev']) test('managed F7 leaves ' + mode + ' unchanged with no Jev UI or RPC', options, async t => {
  const f = await boot(t);
  await choose(f, mode);
  const text = 'F7 preserves the native draft';
  await draft(f, text);
  const offset = f.trace.length;
  await f.write(F7);
  // An edit after F7 proves the real native loop consumed those bytes.
  await f.write(' edited');
  await f.wait(() => f.projectedScreen().includes('› ' + text + ' edited'), 'native composer remains usable after F7');
  assertMode(f, mode);
  assertNoPreview(f);
  await f.expectNoSubmission(300, offset);
  assert.equal(f.calls.length, 0);
  assert.equal(submissions(f).length, 0);
  await f.captureStable('f7-noeffect-' + mode);
  const start = await submit(f, 0), request = submissions(f)[0];
  assertChoice(request, mode); assertIdentity(start, request);
  assert.equal(promptText(start.request), text + ' edited');
  assert.deepEqual(f.calls.map(call => call.text), mode === 'jev' ? [text + ' edited'] : []);
  f.complete(0);
});

test('editing and clearing an ON draft preserves ON and judges only the submitted replacement', options, async t => {
  const f = await boot(t);
  await choose(f, 'jev');
  await draft(f, 'discard this draft');
  await f.write('\x7f');
  await f.wait(() => f.projectedScreen().includes('› discard this draf'), 'native backspace edits the draft');
  assertMode(f, 'jev');
  await f.write('\x15');
  await draft(f, 'edited replacement');
  assertMode(f, 'jev');
  const start = await submit(f, 0), request = submissions(f)[0];
  assertChoice(request, 'jev'); assertIdentity(start, request);
  assert.equal(promptText(start.request), 'edited replacement');
  assert.deepEqual(f.calls.map(call => call.text), ['edited replacement']);
  f.complete(0);
  await idle(f, 'jev');
});

test('queued ON judgment uses the actual prior terminal outcome after native drain', options, async t => {
  const f = await boot(t, { select: () => ({ model: OTHER_MODEL, effort: 'high' }) });
  await choose(f, 'jev');
  await draft(f, 'first Jev prompt');
  await submit(f, 0);
  await f.wait(() => f.previousDecision?.turnId === f.starts[0].turn.id, 'actual service binds the acknowledged first decision');
  assert.equal(f.previousDecision.summary.turnOutcome, 'unknown');
  await draft(f, 'queued ON followup');
  await queued(f, 'queued ON followup', 1);
  await choose(f, 'native');
  assert.equal(f.calls.length, 1);
  f.complete(0);
  await f.wait(() => f.previousDecision?.turnId === f.starts[0].turn.id
    && f.previousDecision.summary.turnOutcome === 'completed', 'actual service observes the prior native terminal');
  const start = await f.waitStart(1), request = submissions(f)[1];
  assertChoice(request, 'jev'); assertIdentity(start, request);
  assert.equal(f.calls.length, 2);
  assert.equal(f.calls[1].context.currentInput, 'queued ON followup');
  assert.deepEqual(f.calls[1].context.previousDecision, {
    model: OTHER_MODEL, effort: 'high', capability: 'bounded_routine',
    contextSufficient: 1, highConsequence: 0, turnOutcome: 'completed',
  });
  f.acknowledge(1); f.start(1); f.complete(1);
  await idle(f, 'native');
});

test('separate native sessions start OFF and do not share toggles or judgment state', options, async t => {
  const left = await boot(t), right = await boot(t);
  assert.notEqual(left.root, right.root);
  assert.notEqual(left.cleanup.port, right.cleanup.port);
  assert.notDeepEqual(left.cleanup.ownedPids, right.cleanup.ownedPids);
  await choose(left, 'jev');
  assertMode(right, 'native');
  await draft(left, 'left same input');
  const a = await submit(left, 0);
  left.complete(0); await idle(left, 'jev');
  await draft(right, 'left same input');
  const b = await submit(right, 0);
  assertChoice(submissions(left)[0], 'jev'); assertIdentity(a, submissions(left)[0]);
  assertNative(b, submissions(right)[0]);
  assert.notEqual(a.request.params.clientUserMessageId, b.request.params.clientUserMessageId);
  assert.equal(left.calls.length, 1);
  assert.equal(right.calls.length, 0);
  assert.equal(right.previousDecision, null);
  right.complete(0); await idle(right, 'native');
  assertMode(left, 'jev');
  await left.captureStable('isolated-session-on');
  await right.captureStable('isolated-session-off');
});

test('managed idle shell preserves its native thread command while Jev stays ON', options, async t => {
  const f = await boot(t);
  await choose(f, 'jev');
  await f.enter('!printf native-per-prompt-shell');
  const request = await f.waitRequest('thread/shellCommand');
  assert.deepEqual(request.params, { threadId: THREAD_ID, command: 'printf native-per-prompt-shell', timeoutMs: null });
  assert.equal(f.clientRequests('turn/start').length, 0);
  assert.equal(f.calls.length, 0);
  assert.equal(submissions(f).length, 0);
  assertMode(f, 'jev');
});

test('managed Escape interrupts the actual native turn without judging its queued ON input', options, async t => {
  const f = await boot(t);
  await running(f);
  await choose(f, 'jev');
  await draft(f, 'queued selected input');
  await queued(f, 'queued selected input', 1);
  await f.write('\x1b');
  const interrupt = await f.waitRequest('turn/interrupt', 0, 2500);
  assert.deepEqual(interrupt.params, { threadId: THREAD_ID, turnId: f.starts[0].turn.id });
  assert.equal(f.starts[0].completed, false, 'Escape must not depend on backend completion');
  f.complete(0, 'interrupted');
  await f.expectNoSubmission();
  assert.equal(f.calls.length, 0, 'interruption must not auto-submit or judge the queued input');
  assert.equal(f.starts.length, 1);
  assertMode(f, 'jev');
});

test('managed Escape cancels a pending ON judgment and resubmits identical input without another F6', options, async t => {
  const judgments = [];
  const f = await boot(t, { decisionTimeoutMs: 15_000, select: (text, { signal }) => new Promise((resolve, reject) => {
    signal.addEventListener('abort', () => reject(Object.assign(new Error('Synthetic judgment cancelled'), { code: 'aborted' })), { once: true });
    judgments.push({ text, signal, resolve });
  }) });
  await choose(f, 'jev');
  const text = 'identical managed cancel retry';
  await draft(f, text); await f.write('\r');
  await f.wait(() => f.calls.length === 1, 'first pending ON judgment');
  const first = submissions(f)[0];
  assertChoice(first, 'jev');
  await f.write('\x1b');
  await f.wait(() => f.trace.some(row => row.kind === 'audit' && row.event.event === 'route-cancelled'
    && row.event.requestId === first.id), 'matching actual pending judgment cancellation', 2500);
  assert.equal(judgments[0].signal.aborted, true);
  assert.equal(f.starts.length, 0);
  assert.equal(f.clientRequests('turn/interrupt').length, 0);
  await f.expectNoSubmission();
  assertMode(f, 'jev');
  await f.write('\x15');
  // The identical history row cannot prove a new draft has rendered. Use the
  // real bracketed-paste boundary so Enter cannot join a pending typing burst.
  await f.pasteEnter(text);
  await f.wait(() => f.calls.length === 2, 'explicit identical retry keeps ON');
  const second = submissions(f)[1];
  assertChoice(second, 'jev');
  assert.notEqual(second.id, first.id);
  assert.notEqual(second.params.clientUserMessageId, first.params.clientUserMessageId);
  judgments[0].resolve({ model: OTHER_MODEL, effort: 'high' });
  judgments[1].resolve({ model: MODEL, effort: 'low' });
  const start = await f.waitStart(0);
  assertIdentity(start, second);
  assertConfiguration(start, MODEL, 'low');
  f.acknowledge(0); f.start(0); f.complete(0);
  await f.expectNoSubmission();
  assert.equal(f.starts.length, 1, 'late cancelled result cannot replay the old input');
  assert.deepEqual(f.calls.map(call => call.text), [text, text]);
});

test('later F6 cannot rewrite an ON message already forwarded before its start ACK', options, async t => {
  const f = await boot(t, { select: () => ({ model: OTHER_MODEL, effort: 'high' }) });
  await choose(f, 'jev');
  await draft(f, 'submitted before later toggle');
  await f.write('\r');
  const start = await f.waitStart(0), request = submissions(f)[0];
  await choose(f, 'native');
  assertChoice(request, 'jev'); assertIdentity(start, request);
  assertConfiguration(start, OTHER_MODEL, 'high');
  f.acknowledge(0); f.start(0); f.complete(0);
  await idle(f, 'native');
  assert.equal(f.clientRequests('turn/start').length, 1);
  assert.equal(f.calls.length, 1);
});

test('managed approval Enter answers the matching server request without creating a prompt', options, async t => {
  const f = await boot(t);
  await choose(f, 'jev');
  await draft(f, 'active ON approval prompt'); await submit(f, 0);
  const offset = f.checkpoint(), calls = f.calls.length;
  const id = f.requestApproval(0);
  await f.wait(() => f.screen(offset).includes('Would you like to run the following command?'), 'actual native approval popup', 2500);
  await f.write('\r');
  const response = await f.waitReply(id, 2500);
  assert.deepEqual(response.result, { decision: 'accept' });
  assert.equal(f.starts[0].completed, false, 'approval must work before backend completion');
  assert.equal(f.clientRequests('turn/start').length, 1);
  assert.equal(f.calls.length, calls);
  f.resolveApproval(id); f.complete(0);
  await idle(f, 'jev');
});
