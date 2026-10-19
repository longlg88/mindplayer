import test from 'node:test';
import assert from 'node:assert/strict';
import { createNativeQueueFixture, MODEL, OTHER_MODEL, THREAD_ID } from './native-queue-fixture.mjs';

// EXPERIMENTAL, not production behavior: the opt-in native queue keymap disables
// idle !shell Enter. These tests do not endorse shipping those overrides or the
// discarded held-steer prototype. No credentials or native inference are used.
const options = { skip: process.env.MINDPLAYER_TEST_NATIVE_QUEUE !== '1', timeout: 30000 };
// Default-keymap regression suite for the private 0.162.1 native patch:
// MINDPLAYER_TEST_NATIVE_UI=1 MINDPLAYER_NATIVE_CODEX=/absolute/path/to/codex \
//   node --test tools/local-model-router/native-queue.test.mjs
// Requires mindplayer-native-routing-v4; an older/global CLI fails the gate.
// The remote backend and Jev are synthetic;
// these tests prove native UI/protocol behavior, not inference/model serving.
const nativeUiOptions = { skip: process.env.MINDPLAYER_TEST_NATIVE_UI !== '1', timeout: 30000 };
const text = start => start.request.params.input.filter(item => item.type === 'text').map(item => item.text).join('\n');

async function boot(t, config) {
  const fixture = await createNativeQueueFixture(config);
  t.after(() => fixture.close());
  await fixture.wait(() => fixture.messages.some(message => message.method === 'thread/start'), 'native startup handshake');
  await fixture.wait(() => fixture.screen().includes(MODEL), 'native composer rendering');
  return fixture;
}

async function bootNativeUi(t, config) {
  const f = await boot(t, { ...config, queue: false });
  assert.ok(!f.args.some(arg => arg.startsWith('tui.keymap.')), 'native UI tests require the default keymap');
  t.after(() => {
    assert.deepEqual(f.unknown, [], 'fixture must implement the actual native protocol, without fallback errors');
    t.diagnostic(`native=${f.binary}; version=${f.version}; contract=${f.contract}; default keymap; unknownMethods=${f.unknown.length}; real PTY; synthetic backend/Jev; no credentials/inference`);
  });
  return f;
}

async function running(f) {
  await f.enter('A'); await f.waitStart(0); f.acknowledge(0); f.start(0);
  await f.wait(() => /esc to interrupt/i.test(f.screen()), 'native busy rendering');
}

async function queued(f, prompts) {
  await f.wait(() => f.clientRequests('turn/steer').length
    || f.screen().includes('Queued follow-up inputs') && prompts.every(prompt => f.screen().includes(`↳ ${prompt}`)), 'native queued prompts or unexpected steering');
  assert.equal(f.clientRequests('turn/steer').length, 0, 'busy Enter must queue inside the native UI, not send turn/steer');
  assert.equal(f.clientRequests('turn/start').length, 1, 'queued prompts must not reach the router before the active terminal');
  assert.deepEqual(f.calls.map(call => call.text), ['A'], 'queued prompts must not be judged before submission');
}

async function tabQueue(f, ...prompts) {
  for (const prompt of prompts) await f.write(`\x1b[200~${prompt}\x1b[201~\t`);
  await queued(f, prompts);
}

async function drain(f, prompts) {
  for (const [index, prompt] of prompts.entries()) {
    f.complete(index);
    const next = await f.waitStart(index + 1);
    assert.equal(text(next), prompt);
    assert.equal(next.request.params.turnTrigger, 'user', 'follow-up must be an ordinary user turn');
    f.acknowledge(index + 1); f.start(index + 1);
  }
  f.complete(prompts.length);
  await f.expectNoSubmission();
}

test('default native keymap submits idle Enter as one ordinary user turn', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await f.enter('A');
  const start = await f.waitStart(0);
  assert.equal(text(start), 'A');
  assert.equal(start.request.params.turnTrigger, 'user');
  assert.deepEqual(f.calls.map(call => call.text), ['A']);
});

test('default native keymap submits idle !shell through thread/shellCommand', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await f.enter('!printf native-shell-fixture');
  const shell = await f.waitRequest('thread/shellCommand');
  assert.deepEqual(shell.params, { threadId: THREAD_ID, command: 'printf native-shell-fixture', timeoutMs: null });
  await f.expectNoSubmission();
  assert.equal(f.shells.length, 1);
  assert.equal(f.clientRequests('command/exec').length, 0, '0.162.1 !shell is a thread command, not a one-off exec');
  assert.equal(f.clientRequests('turn/start').length, 0);
  assert.equal(f.calls.length, 0, '!shell must not become a model prompt');
});

test('default native keymap queues busy Enter until the active turn completes', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await running(f);
  await f.enter('B'); await queued(f, ['B']);
  await drain(f, ['B']);
  assert.deepEqual(f.starts.map(text), ['A', 'B']);
});

test('default native keymap drains busy B and C as distinct turns without merging or duplication', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await running(f);
  await f.enter('B'); await queued(f, ['B']);
  await f.enter('C'); await queued(f, ['B', 'C']);
  await drain(f, ['B', 'C']);
  assert.deepEqual(f.starts.map(text), ['A', 'B', 'C']);
  assert.equal(new Set(f.starts.map(start => start.request.params.clientUserMessageId)).size, 3, 'each submission needs its own identity');
  assert.deepEqual(f.calls.map(call => call.text), ['A', 'B', 'C']);
  for (const [index, start] of f.starts.entries()) {
    const effort = index === 1 ? 'high' : 'low';
    assert.equal(start.request.params.model, MODEL);
    assert.equal(start.request.params.effort, effort);
    assert.equal(start.request.params.collaborationMode.settings.model, MODEL);
    assert.equal(start.request.params.collaborationMode.settings.reasoning_effort, effort);
  }
  assert.equal(f.clientRequests('turn/start').length, 3);
  assert.equal(f.clientRequests('turn/steer').length, 0);
});

test('default native keymap preserves separate B and C inputs arriving before the start ACK', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await f.enter('A'); await f.waitStart(0);
  await f.pasteEnter('B', 'C');
  assert.equal(f.starts.length, 1);
  assert.equal(f.clientRequests('turn/start').length, 1);
  // Release only the ACK so native can consume the already-written keys while
  // the turn is still pending start. A simultaneous turn/started races the
  // terminal input channel and makes this a busy-input test instead.
  f.acknowledge(0);
  await queued(f, ['B', 'C']);
  assert.equal(f.starts[0].started, false);
  f.start(0);
  await drain(f, ['B', 'C']);
  assert.deepEqual(f.starts.map(text), ['A', 'B', 'C']);
  assert.deepEqual(f.calls.map(call => call.text), ['A', 'B', 'C']);
  assert.equal(f.clientRequests('turn/steer').length, 0);
});

test('native completion before the start reply releases exactly one queued input after the real ACK', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await f.enter('A'); await f.waitStart(0);
  await f.pasteEnter('B'); await queued(f, ['B']);
  f.start(0); f.complete(0);
  await f.expectNoSubmission();
  f.acknowledge(0);
  const next = await f.waitStart(1);
  assert.equal(text(next), 'B');
  assert.deepEqual(f.calls.map(call => call.text), ['A', 'B']);
  f.acknowledge(1); f.start(1); f.complete(1);
  await f.expectNoSubmission();
  assert.equal(f.starts.length, 2);
  assert.equal(f.clientRequests('turn/steer').length, 0);
});

test('native turn/start rejection keeps C queued and unjudged until explicit recovery', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  // Built-in Tab isolates request rejection from the separate busy-Enter bug.
  await running(f); await tabQueue(f, 'B', 'C');
  f.complete(0);
  const pending = await f.waitStart(1);
  assert.equal(text(pending), 'B');
  assert.deepEqual(f.calls.map(call => call.text), ['A', 'B']);
  const offset = f.checkpoint(), traceOffset = f.trace.length;
  f.reject(1);
  await f.wait(() => f.screen(offset).includes('native-fixture-start-rejection'), 'native turn/start rejection rendering');
  await f.expectNoSubmission(400, traceOffset);
  // The native terminal uses delta rendering: an unchanged queue row need not
  // be emitted again after the error. Prove its retained identity below by
  // explicitly recovering C and requiring the exact standalone start payload.
  assert.equal(pending.acked, false);
  assert.equal(pending.started, false, 'a rejected request must never receive a synthetic turn/started');
  const draftOffset = f.checkpoint();
  await f.write('\x1b[200~native-rejection-draft\x1b[201~');
  await f.wait(() => f.screen(draftOffset).includes('native-rejection-draft'), 'responsive composer after request rejection');
  await f.expectNoSubmission(400, traceOffset);
  assert.deepEqual(f.starts.map(text), ['A', 'B']);
  assert.equal(f.clientRequests('turn/start').length, 2);
  assert.deepEqual(f.calls.map(call => call.text), ['A', 'B'], 'C must not reach Jev while recovery is paused');
  // 0.162.1 keymap.rs: Alt-Up edits the last queued prompt; only Enter submits.
  const editOffset = f.checkpoint();
  await f.write('\x1b[1;3A');
  await f.wait(() => /›\s*C/.test(f.screen(editOffset)), 'explicitly recovered C composer');
  await f.expectNoSubmission(400, traceOffset);
  await f.write('\r');
  assert.equal(text(await f.waitStart(2)), 'C');
  f.acknowledge(2); f.start(2); f.complete(2);
  await f.expectNoSubmission();
  assert.deepEqual(f.starts.map(text), ['A', 'B', 'C']);
  assert.deepEqual(f.calls.map(call => call.text), ['A', 'B', 'C']);
  assert.equal(f.clientRequests('turn/start').length, 3);
  assert.equal(f.clientRequests('turn/steer').length, 0);
});

test('native Escape sends a real interrupt before the backend supplies a terminal', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await running(f);
  await f.write('\x1b');
  const interrupt = await f.waitRequest('turn/interrupt', 0, 2500);
  assert.deepEqual(interrupt.params, { threadId: THREAD_ID, turnId: f.starts[0].turn.id });
  assert.equal(f.starts[0].completed, false, 'a synthetic completion must not unblock Escape');
  f.complete(0, 'interrupted');
});

test('native Escape remains responsive while a busy Enter prompt is queued', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await running(f);
  await f.enter('B'); await queued(f, ['B']);
  await f.write('\x1b');
  const interrupt = await f.waitRequest('turn/interrupt', 0, 2500);
  assert.equal(interrupt.params.turnId, f.starts[0].turn.id);
  assert.equal(f.starts[0].completed, false, 'Escape must work while the backend is still busy');
  f.complete(0, 'interrupted');
  await f.expectNoSubmission();
  assert.deepEqual(f.starts.map(text), ['A']);
});

test('native Escape remains responsive with a built-in Tab queue control', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await running(f); await tabQueue(f, 'B');
  await f.write('\x1b');
  const interrupt = await f.waitRequest('turn/interrupt', 0, 2500);
  assert.equal(interrupt.params.turnId, f.starts[0].turn.id);
  assert.equal(f.starts[0].completed, false);
  f.complete(0, 'interrupted');
  await f.expectNoSubmission();
  assert.deepEqual(f.starts.map(text), ['A']);
});

async function answerApproval(f) {
  const offset = f.checkpoint();
  const id = f.requestApproval(0);
  await f.wait(() => f.screen(offset).includes('Would you like to run the following command?'), 'real native approval popup', 2500);
  await f.write('\r');
  const reply = await f.waitReply(id, 2500);
  assert.deepEqual(reply.result, { decision: 'accept' }, 'default Enter must answer the actual server approval request');
  assert.equal(f.starts[0].completed, false, 'backend must not finish the turn to unblock the approval');
  f.resolveApproval(id);
}

test('native approval popup returns a matching server-request decision', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await running(f);
  await answerApproval(f);
});

test('native approval Enter remains responsive while a busy Enter prompt is queued', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await running(f);
  await f.enter('B'); await queued(f, ['B']);
  await answerApproval(f);
  await f.expectNoSubmission();
  assert.equal(f.clientRequests('turn/start').length, 1, 'approval confirmation must not submit the queued prompt');
  await drain(f, ['B']);
  assert.deepEqual(f.calls.map(call => call.text), ['A', 'B']);
});

test('native approval Enter remains responsive with a built-in Tab queue control', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  await running(f); await tabQueue(f, 'B');
  await answerApproval(f);
  await f.expectNoSubmission();
  assert.equal(f.clientRequests('turn/start').length, 1);
  await drain(f, ['B']);
  assert.deepEqual(f.calls.map(call => call.text), ['A', 'B']);
});

async function assertNoAutosendAfterTerminal(f, status) {
  const offset = f.checkpoint(), traceOffset = f.trace.length;
  const marker = status === 'failed' ? 'native-fixture-terminal-failure' : 'Conversation interrupted';
  f.complete(0, status);
  await f.wait(() => f.screen(offset).includes(marker), 'native terminal rendering');
  await f.expectNoSubmission(400, traceOffset);
  const rendered = f.screen(offset), terminalScreen = rendered.slice(rendered.indexOf(marker) + marker.length);
  assert.ok(terminalScreen.includes('Queued follow-up inputs') && terminalScreen.includes('↳ B') && terminalScreen.includes('↳ C'),
    'terminal recovery must retain B and C as separate queued prompts, not merge them into one composer draft');
  // A new editable draft proves the event loop has processed the terminal;
  // do not synthesize a review/compact error to inhibit native queue drain.
  const draftOffset = f.checkpoint();
  await f.write('\x1b[200~native-after-terminal\x1b[201~');
  await f.wait(() => f.screen(draftOffset).includes('native-after-terminal'), 'responsive composer after terminal');
  await f.expectNoSubmission(400, traceOffset);
  assert.equal(f.clientRequests('turn/start').length, 1);
  assert.equal(f.clientRequests('turn/steer').length, 0);
  assert.deepEqual(f.calls.map(call => call.text), ['A']);
}

for (const status of ['interrupted', 'failed']) {
  test(`default native keymap does not autosend busy Enter prompts after terminal status ${status}`, nativeUiOptions, async t => {
    const f = await bootNativeUi(t);
    await running(f);
    await f.enter('B'); await queued(f, ['B']);
    await f.enter('C'); await queued(f, ['B', 'C']);
    await assertNoAutosendAfterTerminal(f, status);
  });

  test(`native Tab queue does not autosend after terminal status ${status}`, nativeUiOptions, async t => {
    const f = await bootNativeUi(t);
    await running(f); await tabQueue(f, 'B', 'C');
    await assertNoAutosendAfterTerminal(f, status);
  });
}

test('installed native composer sends idle Enter as a turn/start', options, async t => {
  const f = await boot(t);
  await f.enter('A');
  assert.equal(text(await f.waitStart(0)), 'A');
  assert.equal(f.calls.length, 1);
  t.diagnostic(`EXPERIMENTAL keymap; native=${f.version}; real PTY; synthetic protocol backend and Jev; no inference`);
});

test('installed native composer keeps busy Enter queued until the running turn completes', options, async t => {
  const f = await boot(t);
  await f.enter('A'); await f.waitStart(0); f.acknowledge(0); f.start(0);
  await f.enter('B');
  await f.wait(() => /queued/i.test(f.screen()), 'native queued input rendering');
  assert.equal(f.starts.length, 1); assert.equal(f.calls.length, 1);
  assert.equal(f.clientRequests('turn/start').length, 1, 'B must wait inside the native TUI, before reaching the proxy');
  assert.equal(f.messages.filter(message => message.method === 'turn/steer').length, 0);
  f.complete(0);
  assert.equal(text(await f.waitStart(1)), 'B');
  assert.equal(f.calls.length, 2);
});

test('installed native composer drains rapid pre-ACK inputs as separate ordinary turns', options, async t => {
  const f = await boot(t);
  await f.enter('A'); await f.waitStart(0);
  // Native waits on the start ACK before processing more terminal input.
  // Bracketed paste gives two explicit input boundaries without the native
  // unbracketed paste-burst detector treating B+Enter+C as one pasted string.
  await f.write('\x1b[200~B\x1b[201~\r\x1b[200~C\x1b[201~\r');
  assert.equal(f.starts.length, 1); assert.equal(f.calls.length, 1);
  f.acknowledge(0); f.start(0);
  await f.wait(() => /queued/i.test(f.screen()) && f.screen().includes('↳ B') && f.screen().includes('↳ C'), 'native renders two separate queued prompts after ACK');
  assert.equal(f.starts.length, 1); assert.equal(f.calls.length, 1);
  assert.equal(f.clientRequests('turn/start').length, 1, 'native queue must retain both prompts while A runs');
  f.complete(0);
  assert.equal(text(await f.waitStart(1)), 'B');
  f.acknowledge(1); f.start(1); f.complete(1);
  assert.equal(text(await f.waitStart(2)), 'C');
  assert.deepEqual(f.calls.map(call => call.text), ['A', 'B', 'C']);
  assert.equal(f.messages.filter(message => message.method === 'turn/steer').length, 0);
});

test('same-model consecutive native turns apply low high low effort at both configuration levels', nativeUiOptions, async t => {
  const f = await bootNativeUi(t);
  for (const [index, prompt] of ['A', 'B', 'C'].entries()) {
    await f.enter(prompt);
    const params = (await f.waitStart(index)).request.params;
    const effort = index === 1 ? 'high' : 'low';
    assert.equal(params.model, MODEL); assert.equal(params.effort, effort);
    assert.equal(params.collaborationMode.settings.model, MODEL);
    assert.equal(params.collaborationMode.settings.reasoning_effort, effort);
    f.acknowledge(index); f.start(index); f.complete(index);
  }
  assert.deepEqual(f.calls.map(call => call.effort), ['low', 'high', 'low']);
});

test('native turn routing applies a changed model and effort together', nativeUiOptions, async t => {
  const f = await bootNativeUi(t, { select: () => ({ model: OTHER_MODEL, effort: 'high' }) });
  await f.enter('B');
  const params = (await f.waitStart(0)).request.params;
  assert.equal(params.model, OTHER_MODEL); assert.equal(params.effort, 'high');
  assert.equal(params.collaborationMode.settings.model, OTHER_MODEL);
  assert.equal(params.collaborationMode.settings.reasoning_effort, 'high');
  f.acknowledge(0); f.start(0); f.complete(0);
  await f.expectNoSubmission();
});
