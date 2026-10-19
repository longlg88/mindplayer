import test from 'node:test';
import assert from 'node:assert/strict';
import { createNativeQueueFixture, MODEL, THREAD_ID } from './native-queue-fixture.mjs';

// Real private native UI, synthetic backend/Jev, isolated empty home. No auth,
// copied credentials, external inference, or claim of actual settings receipts.
// MINDPLAYER_TEST_NATIVE_UI=1 MINDPLAYER_NATIVE_CODEX=/absolute/private/codex \
//   node --test tools/local-model-router/native-cancel.test.mjs
for (const goalStatus of [null, 'active', 'paused']) test(`native Escape cancels a pending judgment and identical resubmission is fresh (goal=${goalStatus ?? 'none'})`, {
  skip: process.env.MINDPLAYER_TEST_NATIVE_UI !== '1', timeout: 30_000,
}, async t => {
  const judgments = [];
  const f = await createNativeQueueFixture({ queue: false, goals: goalStatus !== null, decisionTimeoutMs: 15_000, select: (text, { signal }) => new Promise((resolve, reject) => {
    signal.addEventListener('abort', () => reject(Object.assign(new Error('Synthetic judgment cancelled'), { code: 'aborted' })), { once: true });
    judgments.push({ text, signal, resolve });
  }) });
  t.after(() => f.close());
  t.after(() => t.diagnostic(`native=${f.binary}; version=${f.version}; contract=${f.contract}; real PTY; synthetic backend/Jev; no credentials/inference`));
  await f.wait(() => f.messages.some(message => message.method === 'thread/start'), 'native startup handshake');
  await f.wait(() => f.screen().includes(MODEL), 'native composer rendering');
  if (goalStatus) f.deliver({ method: 'thread/goal/updated', params: { threadId: THREAD_ID, turnId: null,
    goal: { threadId: THREAD_ID, objective: 'Synthetic goal state only; no goal execution', status: goalStatus,
      tokenBudget: null, tokensUsed: 0, timeUsedSeconds: 0, createdAt: 1700000000, updatedAt: 1700000000 } } });
  const prompt = 'identical cancel retry';
  await f.pasteEnter(prompt);
  await f.wait(() => f.calls.length === 1, 'first judgment');
  assert.equal(judgments.length, 1);
  const first = f.attempts[0];
  assert.ok(first.params.clientUserMessageId);
  assert.equal(f.starts.length, 0, 'cancel must happen before backend submission');
  await f.write('\x1b');
  await f.wait(() => f.trace.some(row => row.kind === 'audit' && row.event.event === 'route-cancelled'
    && row.event.requestId === first.id), 'exact pending attempt cancellation', 2500);
  assert.equal(judgments[0].signal.aborted, true);
  assert.equal(f.starts.length, 0);
  assert.equal(f.clientRequests('turn/interrupt').length, 0, 'a local cancellation is not a real-turn interrupt');
  await f.expectNoSubmission();

  // The native widget may restore the cancelled draft. Clear it before the
  // user's explicit identical submission; no automatic queue replay is allowed.
  await f.write('\x15');
  await f.pasteEnter(prompt);
  await f.wait(() => f.calls.length === 2, 'fresh identical judgment', 2500);
  assert.equal(judgments.length, 2);
  const second = f.attempts[1];
  assert.notEqual(second.id, first.id);
  assert.notEqual(second.params.clientUserMessageId, first.params.clientUserMessageId);
  assert.deepEqual(second.params.input, first.params.input);
  assert.deepEqual(f.calls.map(call => call.text), [prompt, prompt]);
  judgments[0].resolve({ model: MODEL, effort: 'high' });
  judgments[1].resolve({ model: MODEL, effort: 'low' });
  const start = await f.waitStart(0);
  assert.equal(start.request.id, second.id);
  assert.equal(start.request.params.effort, 'low');
  f.acknowledge(0); f.start(0); f.complete(0);
  await f.expectNoSubmission();
  assert.equal(f.starts.length, 1, 'only the fresh attempt reaches the backend');
  assert.deepEqual(f.unknown, []);
});

test('native pending Escape after forwarding waits for the real turn ID and interrupts once', {
  skip: process.env.MINDPLAYER_TEST_NATIVE_UI !== '1', timeout: 30_000,
}, async t => {
  const f = await createNativeQueueFixture({ queue: false });
  t.after(() => f.close());
  t.after(() => t.diagnostic(`native=${f.binary}; version=${f.version}; contract=${f.contract}; real PTY; synthetic backend/Jev; no credentials/inference`));
  await f.wait(() => f.messages.some(message => message.method === 'thread/start'), 'native startup handshake');
  await f.wait(() => f.screen().includes(MODEL), 'native composer rendering');
  await f.pasteEnter('forwarded cancel');
  const start = await f.waitStart(0);
  await f.write('\x1b');
  await f.wait(() => f.clientRequests('mindplayer/turn/cancel').length === 1, 'responsive pending Escape', 2500);
  assert.equal(f.messages.filter(message => message.method === 'turn/interrupt').length, 0, 'an unacknowledged start has no real turn ID');
  assert.equal(f.trace.some(row => row.kind === 'audit' && row.event.event === 'route-cancelled'), false, 'a forwarded start cannot be withdrawn locally');
  f.acknowledge(0); f.start(0);
  const interrupt = await f.waitRequest('turn/interrupt', 0, 2500);
  assert.deepEqual(interrupt.params, { threadId: THREAD_ID, turnId: start.turn.id });
  f.complete(0, 'interrupted');
  await f.expectNoSubmission();
  assert.equal(f.clientRequests('turn/interrupt').length, 1);
  assert.equal(f.clientRequests('turn/start').length, 1);
  assert.equal(f.starts.length, 1, 'a late ACK must not resend the input');
  assert.deepEqual(f.calls.map(call => call.text), ['forwarded cancel']);
  assert.deepEqual(f.unknown, []);
});
