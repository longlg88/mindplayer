import test from 'node:test';
import assert from 'node:assert/strict';
import { setImmediate as tick } from 'node:timers/promises';
import { chooseModels, ruleDecision, ModelRouter } from './router.mjs';

const catalog = ['model-a', 'model-b'].map((model, index) => ({ model, id: model, hidden: false, isDefault: !index, inputModalities: ['text', 'image'], defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'medium' }] }));
const request = (id = 1, text = '한글\n여러 줄 입력 🐈') => ({ id, method: 'turn/start', params: { threadId: 'same-thread', input: [{ type: 'text', text, text_elements: [] }], model: 'model-a', effort: 'ultra', approvalPolicy: 'on-request', sandboxPolicy: { type: 'readOnly' }, collaborationMode: { mode: 'default', settings: { model: 'model-a', reasoning_effort: 'ultra', developer_instructions: 'Preserve this' } } } });
const interrupt = { id: 99, method: 'turn/interrupt', params: { threadId: 'same-thread', turnId: 'provisional' } };
function harness(options = {}) {
  const sent = [], received = [], audit = [];
  const router = new ModelRouter({ catalog, pair: chooseModels(catalog), toServer: m => sent.push(m), toClient: m => received.push(m), audit: m => audit.push(m), ...options });
  return { router, sent, received, audit };
}
const started = (id = 1, turn = 'turn-1') => ({ id, result: { turn: { id: turn, status: 'inProgress' } } });
const completed = turn => ({ method: 'turn/completed', params: { threadId: 'same-thread', turn: { id: turn, status: 'completed' } } });

test('catalog selection excludes hidden models, requires two distinct real candidates', () => {
  assert.equal(chooseModels(catalog).simple.model, 'model-a');
  assert.throws(() => chooseModels(catalog, { complexModel: 'hallucinated' }));
  assert.throws(() => chooseModels([catalog[0], { ...catalog[1], hidden: true }]));
  assert.throws(() => chooseModels(catalog, { complexModel: 'model-a' }));
});

test('test rules select per prompt, without pretending to be Jev', () => {
  const pair = chooseModels(catalog);
  assert.equal(ruleDecision([{ type: 'text', text: '안녕' }], pair).model, 'model-a');
  assert.equal(ruleDecision([{ type: 'text', text: '원인 분석해줘' }], pair).model, 'model-b');
});

test('retired local preview requests have fixed bounded rejections without judgment or backend calls', async t => {
  for (const managed of [false, true]) {
    let calls = 0;
    const decide = () => { calls++; throw new Error('private-judge-canary'); };
    const service = managed ? { decide, close() {} } : undefined;
    const f = harness({ decide, service });
    t.after(() => f.router.close());
    const original = { ...request(7, 'private-input-canary'.repeat(1000)), method: 'mindplayer/prompt/preview' };
    const before = structuredClone(original);
    for (let attempt = 0; attempt < 2; attempt++) {
      f.router.client(original); await tick();
      assert.deepEqual(f.received.at(-1), {
        id: 7, error: { code: -32601, message: 'Unsupported local method; nothing was sent' },
      });
      assert.equal(f.router.seen.has('number:7'), false);
    }
    assert.deepEqual(original, before);
    assert.equal(calls, 0);
    assert.deepEqual(f.sent, []);
    assert.equal(f.router.requests.size, 0);
    assert.equal(f.router.pending.size, 0);
    assert.equal(f.router.internal.size, 0);
    assert.ok(!JSON.stringify({ received: f.received, audit: f.audit }).includes('canary'));
    assert.equal(f.audit.length, 0);
  }
});

test('retired local preview notifications never reach judgment or the backend', async t => {
  let calls = 0;
  const f = harness({ decide: () => { calls++; } });
  t.after(() => f.router.close());
  f.router.client({ method: 'mindplayer/prompt/preview', params: request().params }); await tick();
  assert.equal(calls, 0);
  assert.deepEqual(f.sent, []);
  assert.deepEqual(f.received, []);
  assert.equal(f.router.seen.size, 0);
});

test('both model fields change, input/approval/sandbox/instructions stay byte-for-byte equivalent', async () => {
  const { router, sent } = harness();
  const original = request(1, '원인 분석\n한글 🐈');
  const snapshot = structuredClone(original);
  router.client(original); await tick();
  assert.equal(sent.length, 1);
  assert.equal(sent[0].params.model, 'model-b');
  assert.equal(sent[0].params.collaborationMode.settings.model, 'model-b');
  assert.equal(sent[0].params.effort, 'medium');
  assert.equal(sent[0].params.collaborationMode.settings.reasoning_effort, 'medium');
  for (const key of ['input', 'threadId', 'approvalPolicy', 'sandboxPolicy']) assert.deepEqual(sent[0].params[key], original.params[key]);
  assert.equal(sent[0].params.collaborationMode.settings.developer_instructions, 'Preserve this');
  assert.deepEqual(original, snapshot);
  router.close();
});

test('private runtime marks only selected fresh starts; native steering and tool output remain unchanged', async () => {
  const f = harness({ requireFreshTurn: true });
  const original = request();
  f.router.client(original); await tick();
  assert.equal(f.sent.at(-1).params.mindplayerRequireNewTurn, true);
  assert.ok(!Object.hasOwn(original.params, 'mindplayerRequireNewTurn'));
  assert.equal(f.sent.at(-1).params.model, 'model-a');
  assert.equal(f.sent.at(-1).params.effort, 'medium');
  f.router.server(started());
  const steer = { id: 2, method: 'turn/steer', params: { threadId: 'same-thread', expectedTurnId: 'turn-1', input: original.params.input } };
  const output = { id: 3, method: 'turn/start', params: { threadId: 'same-thread', input: original.params.input, toolOutput: { output: 'tool result' } } };
  for (const message of [steer, output]) { f.router.client(message); assert.deepEqual(f.sent.at(-1), message); }
  f.router.close();
});

test('private active fresh input rejects immediately and never resumes after any terminal outcome', async () => {
  for (const status of ['completed', 'failed', 'interrupted']) {
    let judgments = 0;
    const f = harness({ requireFreshTurn: true, decisionTimeoutMs: 1, decide: () => {
      judgments++; return { model: 'model-a', effort: 'medium' };
    } });
    f.router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'active' } } });
    f.router.client(request());
    assert.equal(f.received.at(-1).id, 1);
    assert.match(f.received.at(-1).error.message, /already active; nothing was sent/);
    assert.equal(f.router.pending.size, 0);
    assert.equal(judgments, 0); assert.equal(f.sent.length, 0);
    const approval = { id: 77, method: 'item/commandExecution/requestApproval', params: { threadId: 'same-thread' } };
    f.router.server(approval); assert.deepEqual(f.received.at(-1), approval);
    const approved = { id: 77, result: { decision: 'accept' } };
    f.router.client(approved); assert.deepEqual(f.sent.at(-1), approved);
    const escape = { ...interrupt, params: { ...interrupt.params, turnId: 'active' } };
    f.router.client(escape); assert.deepEqual(f.sent.at(-1), escape);
    f.router.server({ method: 'turn/completed', params: { threadId: 'same-thread', turn: { id: 'active', status } } });
    await new Promise(resolve => setTimeout(resolve, 10));
    assert.equal(judgments, 0);
    assert.equal(f.sent.filter(row => row.method === 'turn/start').length, 0);
    assert.equal(f.audit.some(row => row.event === 'route-queued'), false);
    assert.equal(f.audit.find(row => row.event === 'route-failed').reason, 'new-turn-active');
    f.router.close();
  }
});

test('explicit transfer denial still guards owned fresh input without modifying helper submissions', async () => {
  for (const owned of [true, false]) {
    const service = { threads: new Map([['same-thread', { owned }]]), observe() {}, close() {},
      async decide() { return { passthrough: true, reason: owned ? 'external-transfer-policy-unset' : 'non-user-thread' }; } };
    const f = harness({ service, requireFreshTurn: true });
    const original = request(); f.router.client(original); await tick();
    assert.equal(f.sent.at(-1).params.mindplayerRequireNewTurn, owned ? true : undefined);
    assert.deepEqual(f.sent.at(-1).params.input, original.params.input);
    assert.ok(!Object.hasOwn(original.params, 'mindplayerRequireNewTurn'));
    f.router.close();
  }
});

test('consecutive turns route independently on exactly the same thread', async () => {
  const { router, sent } = harness();
  router.client(request()); await tick(); router.server(started()); router.server(completed('turn-1'));
  router.client(request(2, '비교해줘')); await tick();
  assert.deepEqual(sent.map(m => m.params.model), ['model-a', 'model-b']);
  assert.deepEqual(sent.map(m => m.params.threadId), ['same-thread', 'same-thread']);
  router.close();
});

test('Jev can change only effort on consecutive turns without changing the model', async () => {
  const model = { ...catalog[0], supportedReasoningEfforts: ['low', 'medium', 'high'].map(reasoningEffort => ({ reasoningEffort })) };
  const efforts = ['high', 'medium', 'low'];
  const service = { catalog: [model], observe() {}, close() {}, async decide() {
    return { model: model.model, effort: efforts.shift(), reason: 'catalog-guided-jev' };
  } };
  const { router, sent, received, audit } = harness({ catalog: [model], service });
  for (const [index, effort] of ['high', 'medium', 'low'].entries()) {
    const original = request(index + 1);
    const snapshot = structuredClone(original);
    router.client(original); await tick();
    const params = sent.at(-1).params;
    assert.equal(params.model, model.model);
    assert.equal(params.effort, effort);
    assert.equal(params.collaborationMode.settings.model, model.model);
    assert.equal(params.collaborationMode.settings.reasoning_effort, effort);
    assert.equal(params.collaborationMode.settings.developer_instructions, 'Preserve this');
    assert.deepEqual(original, snapshot);
    router.server(started(index + 1, `effort-${index}`));
    router.server(completed(`effort-${index}`));
  }
  assert.equal(sent.length, 3);
  assert.equal(received.some(message => message.error), false);
  assert.deepEqual(audit.filter(event => event.event === 'route-requested').map(event => event.effort), ['high', 'medium', 'low']);
  router.close();
});

test('server approval requests/replies with overlapping numeric IDs pass unchanged', async () => {
  const { router, sent, received } = harness();
  const start = request(); router.client(start); await tick();
  const approval = { id: 1, method: 'item/commandExecution/requestApproval', params: { threadId: 'same-thread', turnId: 'turn-1', itemId: 'item-1', command: 'test' } };
  router.server(approval);
  assert.deepEqual(received.at(-1), approval);
  for (const decision of ['decline', 'cancel']) {
    const response = { id: 1, result: { decision } }; router.client(response); assert.deepEqual(sent.at(-1), response);
  }
  router.server(started());
  const resolved = { method: 'serverRequest/resolved', params: { threadId: 'same-thread', requestId: 1 } };
  router.server(resolved); assert.deepEqual(received.at(-1), resolved);
  assert.equal(router.pending.size, 0);
  router.close();
});

test('an unknown pre-start interrupt cannot cancel an unrelated pending request', async () => {
  let resolve;
  const { router, sent, received } = harness({ decide: () => new Promise(r => { resolve = r; }) });
  router.client(request()); await tick(); router.client(interrupt);
  resolve({ model: 'model-b' }); await tick();
  assert.equal(sent.length, 1);
  assert.equal(received[0].id, 99); assert.ok(received[0].error);
  router.close();
});

test('cancelling an unsent attempt releases its ID and identical input is judged afresh', async t => {
  const judgments = [];
  const f = harness({ requireFreshTurn: true, decide: (input, pair, signal) => new Promise(resolve => {
    judgments.push({ input, signal, resolve });
  }) });
  t.after(() => f.router.close());
  const original = request(1, 'identical prompt');
  original.params.clientUserMessageId = 'attempt-one';
  f.router.client(original); await tick();
  const cancel = { id: 2, method: 'mindplayer/turn/cancel', params: { threadId: 'same-thread', clientUserMessageId: 'attempt-one' } };
  f.router.client(cancel);
  assert.equal(judgments[0].signal.aborted, true);
  assert.equal(f.router.pending.size, 0);
  assert.equal(f.router.seen.has('number:1'), false);
  assert.equal(f.router.seen.has('number:2'), false);
  assert.equal(f.received.find(row => row.id === 1)?.error?.code, -32800);
  assert.deepEqual(f.received.find(row => row.id === 2)?.result, { cancelled: true, requestId: 1 });
  assert.equal(f.sent.length, 0);

  const next = structuredClone(original);
  next.params.clientUserMessageId = 'attempt-two';
  f.router.client(next); await tick();
  assert.equal(judgments.length, 2, 'explicit resubmission must perform a new judgment');
  f.router.client({ ...cancel, id: 3 });
  assert.equal(f.received.find(row => row.id === 3)?.result?.cancelled, false, 'a stale cancellation cannot cancel a reused RPC ID');
  assert.equal(judgments[1].signal.aborted, false);
  judgments[0].resolve({ model: 'model-b' }); await tick();
  assert.equal(f.sent.length, 0, 'late cancelled judgment must never submit');
  judgments[1].resolve({ model: 'model-a' }); await tick();
  assert.equal(f.sent.length, 1);
  assert.equal(f.sent[0].params.clientUserMessageId, 'attempt-two');
  assert.deepEqual(f.sent[0].params.input, original.params.input);
  assert.equal(f.audit.filter(row => row.event === 'route-deciding').length, 2);
  assert.equal(f.audit.filter(row => row.event === 'route-requested').length, 1);
});

test('local cancellation checks typed request IDs, never cancels sent input or forwards a local RPC', async t => {
  let resolve;
  const f = harness({ decide: () => new Promise(yes => resolve = yes) });
  t.after(() => f.router.close());
  f.router.client(request(1)); await tick();
  const cancel = (id, params) => f.router.client({ id, method: 'mindplayer/turn/cancel', params: { threadId: 'same-thread', ...params } });
  for (const [id, params] of [[2, {}], [3, { requestId: '1' }], [4, { requestId: 1, threadId: 'another-thread' }],
    [7, { requestId: 1, clientUserMessageId: 'unrelated-attempt' }]]) {
    cancel(id, params);
    assert.equal(f.received.at(-1).result.cancelled, false);
  }
  cancel(5, { requestId: 1 });
  assert.equal(f.received.at(-1).result.cancelled, true);
  resolve({ model: 'model-b' }); await tick();
  assert.equal(f.sent.length, 0);
  f.router.decide = () => ({ model: 'model-a' });
  f.router.client(request(1)); await tick();
  cancel(6, { requestId: 1 });
  assert.equal(f.received.at(-1).result.cancelled, false);
  assert.equal(f.router.pending.get('same-thread').phase, 'sent');
  assert.deepEqual(f.sent.map(row => row.method), ['turn/start']);
});

test('immediate local cancellation never starts inference and rejects conflicting attempt IDs', async t => {
  let calls = 0;
  const f = harness({ decide: () => { calls++; return { model: 'model-a' }; } });
  t.after(() => f.router.close());
  const start = request(1);
  start.params.clientUserMessageId = 'pending-attempt';
  f.router.client(start);
  f.router.client({ id: 2, method: 'mindplayer/turn/cancel', params: {
    threadId: 'same-thread', clientUserMessageId: 'pending-attempt', requestId: '1',
  } });
  assert.deepEqual(f.received.at(-1).result, { cancelled: false });
  f.router.client({ id: 3, method: 'mindplayer/turn/cancel', params: {
    threadId: 'same-thread', clientUserMessageId: 'pending-attempt', requestId: 1,
  } });
  assert.deepEqual(f.received.at(-1).result, { cancelled: true, requestId: 1 });
  await tick();
  assert.equal(calls, 0);
  assert.equal(f.sent.length, 0);
  assert.equal(f.router.pending.size, 0);
});

test('interrupt while start acknowledgement is outstanding preserves matching real target, no resend', async () => {
  const { router, sent } = harness();
  const cancel = { ...interrupt, params: { ...interrupt.params, turnId: 'real-turn' } };
  router.client(request()); await tick(); router.client(cancel);
  assert.equal(sent.length, 1);
  router.server(started(1, 'real-turn'));
  assert.equal(sent.length, 2); assert.deepEqual(sent[1], cancel);
  assert.equal(sent.filter(m => m.method === 'turn/start').length, 1);
  router.close();
});

test('disconnect during selection permanently suppresses a late send', async () => {
  let resolve;
  const { router, sent } = harness({ decide: () => new Promise(r => { resolve = r; }) });
  router.client(request()); await tick(); router.close(); resolve({ model: 'model-b' }); await tick();
  assert.equal(sent.length, 0);
});

test('selector defects and timeout fail explicitly before submission, without fallback', async () => {
  for (const decide of [() => Promise.reject(new Error('offline')), () => new Promise(() => {})]) {
    const { router, sent, audit, received } = harness({ decide, decisionTimeoutMs: 5 });
    router.client(request()); await new Promise(r => setTimeout(r, 15));
    assert.equal(sent.length, 0); assert.ok(received[0].error);
    assert.ok(['selector-error', 'selector-timeout'].includes(audit.find(e => e.event === 'route-failed').reason)); router.close();
  }
});

test('Jev TLS failure stays actionable and private without submitting or retrying', async () => {
  const secret = 'private-upstream-key-and-prompt';
  const { router, sent, received, audit } = harness({ decide: () => {
    throw Object.assign(new Error(secret), { code: 'tls-error', cause: secret });
  } });
  router.client(request()); await tick();
  assert.equal(sent.length, 0);
  assert.equal(received.length, 1);
  assert.match(received[0].error.message, /Jev TLS certificate verification failed/);
  assert.match(received[0].error.message, /nothing was sent/);
  assert.equal(audit.find(e => e.event === 'route-failed').reason, 'jev-tls-error');
  assert.ok(!JSON.stringify({ received, audit }).includes(secret));
  router.close();
});

test('invalid Jev response is distinguished without accepting it or replaying the prompt', async () => {
  const { router, sent, received, audit } = harness({ decide: () => {
    throw Object.assign(new Error('private-response-body'), { code: 'invalid-response' });
  } });
  router.client(request()); await tick();
  assert.equal(sent.length, 0);
  assert.match(received[0].error.message, /Jev response validation failed/);
  assert.equal(audit.find(e => e.event === 'route-failed').reason, 'jev-invalid-response');
  assert.ok(!JSON.stringify({ received, audit }).includes('private-response-body'));
  router.close();
});

test('Jev transport, authentication and size failures expose only fixed pre-submission diagnostics', async () => {
  for (const code of ['jev-key-unavailable', 'unauthorized', 'payment-required', 'rate-limited', 'http-error', 'network-error', 'timeout', 'request-too-large', 'response-too-large']) {
    let calls = 0;
    const { router, sent, received, audit } = harness({ decide: () => {
      calls++; throw Object.assign(new Error('private-key-and-prompt-canary'), { code });
    } });
    router.client(request()); await tick();
    assert.equal(calls, 1); assert.equal(sent.length, 0); assert.equal(received.length, 1);
    assert.match(received[0].error.message, /Jev/); assert.match(received[0].error.message, /nothing was sent/);
    assert.equal(audit.find(event => event.event === 'route-failed').reason, code.startsWith('jev-') ? code : `jev-${code}`);
    assert.ok(!JSON.stringify({ received, audit }).includes('canary')); router.close();
  }
});

test('current-family policy failures expose a bounded cause without leaking upstream details or submitting', async () => {
  for (const code of ['registry-review-required', 'latest-family-anchor-unavailable', 'latest-family-effort-unsupported',
    'latest-family-policy-conflict', 'latest-family-selection-invalid', 'baseline-ineligible', 'external-transfer-policy-unset']) {
    const { router, sent, received, audit } = harness({ decide: () => {
      throw Object.assign(new Error('private-policy-and-prompt-canary'), { code });
    } });
    router.client(request()); await tick();
    assert.equal(sent.length, 0); assert.equal(received.length, 1);
    assert.match(received[0].error.message, /nothing was sent/);
    assert.equal(audit.find(event => event.event === 'route-failed').reason, code);
    assert.ok(!JSON.stringify({ received, audit }).includes('canary'));
    router.close();
  }
});

test('unsupported model/image fails before inference rather than selecting an arbitrary substitute', async () => {
  for (const options of [{ decide: () => ({ model: 'unknown' }) }, { catalog: catalog.map(m => ({ ...m, inputModalities: ['text'] })) }]) {
    const { router, sent, received } = harness(options);
    const message = request(); message.params.input.push({ type: 'localImage', path: '/tmp/sample.png' });
    router.client(message); await tick();
    assert.equal(sent.length, 0); assert.ok(received[0].error); router.close();
  }
});

test('active steering, tool output, and server streams are not rewritten; a new turn waits then routes', async () => {
  const { router, sent, received } = harness();
  router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'active' } } });
  const activeInput = request(1, '분석'); router.client(activeInput); assert.equal(sent.length, 0);
  const steer = { id: 2, method: 'turn/steer', params: { threadId: 'same-thread', expectedTurnId: 'active', input: [{ type: 'text', text: '추가' }] } };
  router.client(steer); assert.deepEqual(sent.at(-1), steer);
  const tool = { id: 3, method: 'turn/start', params: { threadId: 'same-thread', input: [], toolOutput: { name: 'test', output: 'ok' } } };
  router.client(tool); assert.deepEqual(sent.at(-1), tool);
  const delta = { method: 'item/agentMessage/delta', params: { delta: '한글 🐈' } };
  router.server(delta); assert.deepEqual(received.at(-1), delta);
  router.server(completed('active')); await tick();
  assert.equal(sent.filter(m => m.method === 'turn/start' && m.params.input.length).length, 1);
  assert.equal(sent.at(-1).params.model, 'model-b');
  router.close();
});

test('closing a queued new turn never runs its selector or forwards it', async () => {
  let calls = 0;
  const { router, sent } = harness({ decide: () => { calls++; return { model: 'model-b' }; } });
  router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'active' } } });
  router.client(request()); router.close(); await tick();
  assert.equal(calls, 0); assert.equal(sent.length, 0);
});

test('tool output acknowledgement cannot consume a queued user turn', async () => {
  const { router, sent, received } = harness();
  router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'active' } } });
  router.client(request(1, '분석'));
  router.client({ id: 2, method: 'turn/start', params: { threadId: 'same-thread', input: [], toolOutput: { name: 'test', output: 'ok' } } });
  router.server(started(2, 'active'));
  assert.equal(router.pending.size, 1);
  router.server(completed('active')); await tick();
  assert.equal(sent.filter(m => m.method === 'turn/start' && m.params.input.length).length, 1);
  assert.equal(received.filter(m => m.id === 1).length, 0);
  router.close();
});

test('a late tool-output acknowledgement cannot revive a completed turn or strand later input', async () => {
  const { router, sent, received } = harness();
  router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'active' } } });
  router.client(request(1, '분석'));
  router.client({ id: 2, method: 'turn/start', params: { threadId: 'same-thread', input: [], toolOutput: { name: 'test', output: 'ok' } } });
  router.server(completed('active'));
  router.server(started(2, 'active'));
  await tick();
  assert.equal(router.active.has('same-thread'), false);
  assert.equal(sent.filter(m => m.id === 1).length, 1);
  assert.equal(received.some(m => m.id === 1 && m.error), false);
  router.server(started(1, 'turn-1')); router.server(completed('turn-1'));
  router.client(request(3, '다음')); await tick();
  assert.equal(sent.filter(m => m.id === 3).length, 1);
  assert.equal(router.pending.get('same-thread').phase, 'sent');
  router.close();
});

test('another native turn starting during judgment invalidates the unsent new turn', async () => {
  let resolve;
  const { router, sent, received } = harness({ decide: () => new Promise(r => resolve = r) });
  router.client(request()); await tick();
  router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'other' } } });
  resolve({ model: 'model-b' }); await tick();
  assert.equal(sent.length, 0); assert.ok(received.find(m => m.id === 1)?.error); router.close();
});

test('closing a thread rejects waiting input and clears active ownership without replay', async () => {
  const { router, sent, received } = harness();
  router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'active' } } });
  router.client(request());
  router.server({ method: 'thread/closed', params: { threadId: 'same-thread' } }); await tick();
  assert.equal(router.pending.size, 0); assert.equal(router.active.size, 0);
  assert.ok(received.find(m => m.id === 1)?.error); assert.equal(sent.length, 0); router.close();
});

test('duplicate IDs and second pending starts never execute twice', async () => {
  const { router, sent, received } = harness();
  router.client(request()); assert.throws(() => router.client(request()));
  router.client(request(2)); assert.ok(received[0].error); await tick();
  assert.equal(sent.length, 1); router.close();
});

test('backend rejection is returned without fallback inference or automatic retry', async () => {
  const { router, sent, received } = harness(); router.client(request()); await tick();
  const failure = { id: 1, error: { code: -32000, message: 'denied' } }; router.server(failure);
  assert.deepEqual(received.at(-1), failure); assert.equal(sent.length, 1); router.close();
});

test('RPC IDs can be reused after their response, but not while pending', () => {
  const { router, sent } = harness();
  const read = { id: 1, method: 'account/read', params: {} };
  router.client(read); router.server({ id: 1, result: {} }); router.client(read);
  assert.equal(sent.length, 2); assert.throws(() => router.client(read)); router.close();
});

test('stale interrupts and rejected starts never manufacture cancellation success', async () => {
  for (const reply of [started(), { id: 1, error: { code: -32000, message: 'denied' } }, { id: 1, result: {} }]) {
    const { router, sent, received } = harness();
    router.client(request()); await tick(); router.client(interrupt); router.server(reply);
    assert.equal(sent.length, 1);
    const cancelReply = received.find(m => m.id === interrupt.id);
    assert.ok(cancelReply.error); assert.equal(cancelReply.result, undefined);
    assert.equal(router.seen.has('number:99'), false); router.close();
  }
});

test('queued interrupt is acknowledged only by the real server and releases its RPC ID', async () => {
  const { router, received } = harness();
  const cancel = { ...interrupt, params: { ...interrupt.params, turnId: 'turn-1' } };
  router.client(request()); await tick(); router.client(cancel); router.server(started());
  assert.equal(received.some(m => m.id === cancel.id), false);
  router.server({ id: cancel.id, result: {} });
  assert.deepEqual(received.at(-1), { id: cancel.id, result: {} });
  assert.equal(router.seen.has('number:99'), false); router.close();
});

test('a selector cannot escape the configured pair through a third visible model', async () => {
  const { router, sent, received } = harness({ catalog: [...catalog, { ...catalog[0], model: 'third-model' }], decide: () => ({ model: 'third-model' }) });
  router.client(request()); await tick();
  assert.equal(sent.length, 0); assert.ok(received[0].error); router.close();
});

test('a transport send failure is reported as uncertain, never silently retried', async () => {
  let calls = 0;
  const { router, received, audit } = harness({ toServer() { calls++; throw new Error('pipe failed'); } });
  router.client(request()); await tick();
  assert.equal(calls, 1); assert.match(received[0].error.message, /outcome unknown/);
  assert.equal(audit.find(e => e.event === 'route-failed').reason, 'submission-uncertain-no-replay'); router.close();
});

test('completion before the start response does not leave a stale active model', async () => {
  const { router, sent } = harness();
  router.client(request()); await tick();
  router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'turn-1' } } });
  router.server(completed('turn-1')); router.server(started());
  router.client(request(2, '분석')); await tick();
  assert.equal(sent.at(-1).params.model, 'model-b'); assert.equal(router.active.size, 0); router.close();
});

test('start notifications before ACK carry their pending request ID for pane lifecycle correlation', async () => {
  const { router, sent, audit } = harness();
  router.client(request(7)); await tick();
  router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'turn-7', status: 'inProgress' } } });
  assert.deepEqual(audit.find(e => e.event === 'turn-started'), {
    event: 'turn-started', threadId: 'same-thread', requestId: 7, turnId: 'turn-7',
  });
  router.server(started(7, 'turn-7'));
  assert.equal(sent.length, 1, 'event ordering never resends the user turn');
  router.close();
});

test('an unrelated late completion cannot erase completion-before-ACK and strand the next prompt', async () => {
  const { router, sent } = harness();
  router.client(request()); await tick();
  router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'turn-1' } } });
  router.server(completed('turn-1'));
  router.server(completed('older-turn'));
  router.server(started());
  assert.equal(router.active.size, 0, 'late ACK cannot revive a completed turn');
  router.client(request(2, '분석')); await tick();
  assert.equal(sent.length, 2, 'next prompt must not wait on a completed turn');
  router.close();
});

test('thread closure emits a correlated lifecycle record independently of transport closure', async () => {
  const { router, audit } = harness();
  router.client(request()); await tick(); router.server(started());
  router.server({ method: 'thread/closed', params: { threadId: 'same-thread' } });
  assert.ok(audit.some(e => e.event === 'thread-closed' && e.threadId === 'same-thread'));
  router.close();
});

test('a late ACK after thread closure cannot revive activity or forward a queued interrupt', async () => {
  const { router, sent, received } = harness();
  router.client(request()); await tick();
  router.client({ ...interrupt, params: { ...interrupt.params, turnId: 'turn-1' } });
  router.server({ method: 'thread/closed', params: { threadId: 'same-thread' } });
  router.server(started());
  assert.equal(router.active.size, 0);
  assert.equal(router.pending.size, 0);
  assert.equal(sent.length, 1);
  assert.ok(received.find(message => message.id === interrupt.id)?.error);
  router.close();
});

test('completion history before ACK is bounded and never causes a replay', async () => {
  const { router, sent } = harness();
  router.client(request()); await tick();
  for (let index = 0; index < 128; index++) router.server(completed(`old-${index}`));
  assert.throws(() => router.server(completed('excess')), /Excessive completions/);
  router.close();
  assert.equal(sent.length, 1);
});

test('thread closure rejects queued interrupts before any late start notification', async () => {
  const { router, sent, received } = harness();
  router.client(request()); await tick();
  router.client({ ...interrupt, params: { ...interrupt.params, turnId: 'turn-1' } });
  router.server({ method: 'thread/closed', params: { threadId: 'same-thread' } });
  assert.ok(received.find(message => message.id === interrupt.id)?.error);
  router.server({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'turn-1' } } });
  assert.equal(router.active.size, 0);
  assert.equal(sent.filter(message => message.method === 'turn/interrupt').length, 0);
  router.server(started());
  assert.equal(router.active.size, 0);
  router.close();
});

test('a closed thread ignores late start after ACK but can be explicitly resumed', async () => {
  const { router } = harness();
  router.client(request()); await tick(); router.server(started());
  router.server({ method: 'thread/closed', params: { threadId: 'same-thread' } });
  const notification = { method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'turn-1' } } };
  router.server(notification);
  assert.equal(router.active.size, 0);
  router.client({ id: 2, method: 'thread/resume', params: { threadId: 'same-thread' } });
  router.server({ id: 2, result: { thread: { id: 'same-thread' } } });
  router.server(notification);
  assert.equal(router.active.get('same-thread'), 'turn-1');
  router.close();
});
