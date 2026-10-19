import test from 'node:test';
import assert from 'node:assert/strict';
import { setImmediate as tick } from 'node:timers/promises';
import { JevService } from './service.mjs';
import { ModelRouter } from './router.mjs';

const rubricVersion = 'mindplayer-capacity-v2';
const catalog = ['native', 'investigation'].map(model => ({ model, inputModalities: ['text'], hidden: false,
  description: model === 'native' ? 'A bounded routine-work configuration.' : 'An unresolved investigation configuration.',
  defaultReasoningEffort: 'high', supportedReasoningEfforts: [{ reasoningEffort: 'low' }, { reasoningEffort: 'high' }] }));
const judgment = (configurations, index = 0) => ({ capability: 'open_problem', confidence: 1,
  probabilities: { bounded_routine: 0, multistep_coordination: 0, open_problem: 1, unknown: 0 },
  contextSufficient: 0.9, highConsequence: 0.8, usage: { inputTokens: 1, outputTokens: 1 },
  configuration: { ...configurations[index], choice: `c${index}` } });
const start = (id, text = '계속해', threadId = 'main') => ({ id, method: 'turn/start', params: {
  threadId, input: [{ type: 'text', text }], cwd: '/preserve-cwd', approvalPolicy: 'never', sandboxPolicy: { type: 'readOnly' },
} });

function fixture(options = {}) {
  const snapshots = [], calls = [], sent = [], received = [], audit = [];
  const service = new JevService({ catalog, accountHome: '/synthetic-account', account: { type: 'chatgpt', email: 'example@example.invalid' },
    expectedThreadId: 'main', apiKey: 'synthetic-key', provider: 'typesafe', externalPromptTransfer: true,
    policyReader: () => null, judgmentLimits: { cooldownMs: 0 }, audit: event => audit.push(event),
    judgeFactory: config => async state => { snapshots.push(structuredClone(state)); calls.push(config); return judgment(config.configurations); }, ...options });
  const router = new ModelRouter({ service, catalog: options.catalog ?? catalog, audit: event => audit.push(event), toServer: event => sent.push(event), toClient: event => received.push(event) });
  const own = (threadId = 'main', id = 'lifecycle') => {
    router.client({ id, method: 'thread/start', params: { sessionStartSource: 'startup' } });
    router.server({ id, result: { thread: { id: threadId, source: 'cli' }, model: 'native', reasoningEffort: 'high' } });
  };
  const submit = async request => { const count = sent.length; router.client(request); await tick();
    assert(sent.length > count || received.some(value => value.id === request.id), 'request resolves after judgment');
    return sent.findLast(value => value.id === request.id && value.method === 'turn/start'); };
  const ack = (request, turnId = `turn-${request.id}`) => router.server({ id: request.id, result: { turn: { id: turnId } } });
  const complete = (request, status = 'completed', turnId = `turn-${request.id}`) => router.server({ method: 'turn/completed', params: { threadId: request.params.threadId, turn: { id: turnId, status } } });
  return { service, router, snapshots, calls, sent, received, audit, own, submit, ack, complete };
}

test('catalog mode sends only current prompt, version and small prior judgment, never history/paging', async () => {
  const f = fixture(); f.own();
  for (let i = 0; i < 10; i++) {
    f.router.server({ method: 'item/completed', params: { threadId: 'main', item: { type: 'userMessage', content: [{ type: 'text', text: 'history-canary' }] } } });
    f.router.server({ method: 'item/completed', params: { threadId: 'main', item: { type: 'agentMessage', phase: 'final_answer', text: 'assistant-canary' } } });
  }
  const request = start(1, '독립적인 새 설계 요청'); const sent = await f.submit(request);
  assert(sent); assert.equal(f.snapshots.length, 1);
  const state = f.snapshots[0]; assert.equal(state.rubricVersion, rubricVersion); assert.equal(state.currentInput, request.params.input[0].text);
  assert.deepEqual(state.recent, []); assert.equal(state.previousDecision, null); assert.equal(state.truncated, false); assert.equal(state.contextKnown, false);
  assert(!JSON.stringify(state).includes('canary')); assert(!f.sent.some(value => value.method === 'thread/turns/list'));
  f.router.close();
});

test('accepted same-thread judgment survives compaction and tracks turn outcome, not goal completion', async () => {
  const f = fixture(); f.own();
  const first = start(1, '원인을 모르는 장애를 분석해'); await f.submit(first); f.ack(first); f.complete(first, 'interrupted');
  f.router.server({ method: 'thread/compacted', params: { threadId: 'main' } });
  await f.submit(start(2));
  assert.deepEqual(f.snapshots[1].previousDecision, { model: 'native', effort: 'high', capability: 'open_problem',
    contextSufficient: 0.9, highConsequence: 0.8, turnOutcome: 'interrupted' });
  assert.deepEqual(f.snapshots[1].recent, []); assert.equal(f.snapshots[1].truncated, false); assert.equal(f.calls.length, 2);
  assert.equal(Object.hasOwn(f.snapshots[1].previousDecision, 'taskCompleted'), false);
  f.router.close();
});

test('rejected or unacknowledged selections never become previous judgment', async () => {
  const f = fixture(); f.own();
  const first = start(1); await f.submit(first);
  assert.equal(f.service.threads.get('main').previousDecision ?? null, null);
  f.router.server({ id: first.id, error: { code: -1, message: 'private-error-canary' } });
  await f.submit(start(2, '다시 분석해')); assert.equal(f.snapshots[1].previousDecision, null);
  assert(!JSON.stringify(f.audit).includes('canary')); f.router.close();
});

test('previous judgment cannot cross thread, catalog refresh or ownership boundaries', async () => {
  const f = fixture(); f.own();
  const first = start(1); await f.submit(first); f.ack(first); f.complete(first);
  f.own('other', 'other-lifecycle'); const other = start(2, '계속해', 'other'); await f.submit(other);
  assert.equal(f.snapshots.at(-1).previousDecision, null); f.ack(other); f.complete(other);
  f.router.client({ id: 'catalog-refresh', method: 'model/list', params: {} });
  f.router.server({ id: 'catalog-refresh', result: { data: catalog, nextCursor: null } });
  const next = start(3); await f.submit(next); assert.equal(f.snapshots.at(-1).previousDecision, null); f.ack(next); f.complete(next);
  f.own('main', 'new-lifecycle'); await f.submit(start(4)); assert.equal(f.snapshots.at(-1).previousDecision, null);
  f.router.close();
});

test('unrelated completion cannot change accepted judgment outcome; completion before ACK is correlated', async () => {
  const f = fixture(); f.own(); const first = start(1); await f.submit(first);
  f.router.server({ method: 'turn/started', params: { threadId: 'main', turn: { id: 'turn-1' } } });
  f.complete(first, 'failed'); f.ack(first);
  f.router.server({ method: 'turn/completed', params: { threadId: 'main', turn: { id: 'unrelated', status: 'completed' } } });
  await f.submit(start(2)); assert.equal(f.snapshots.at(-1).previousDecision.turnOutcome, 'failed'); f.router.close();
});

test('resume in default catalog mode does not fetch transcript pages', async () => {
  const f = fixture();
  f.router.client({ id: 'resume', method: 'thread/resume', params: { threadId: 'main' } });
  f.router.server({ id: 'resume', result: { thread: { id: 'main', source: 'cli' }, model: 'native', reasoningEffort: 'high' } });
  assert(!f.sent.some(value => value.method === 'thread/turns/list'));
  await f.submit(start(1)); assert.equal(f.snapshots[0].previousDecision, null); f.router.close();
});

test('known tool and context contradictions exclude alternatives without inventing unknown requirements', async () => {
  const rows = [
    { ...catalog[0], supportedTools: ['shell'], contextWindow: 100_000 },
    { ...catalog[1], supportedTools: [], contextWindow: 1000 },
  ];
  for (const requirements of [{ tools: ['shell'] }, { contextTokens: 4000 }, {}]) {
    const f = fixture({ catalog: rows, requirementsForTurn: () => requirements }); f.own();
    await f.submit(start(1, 'self-contained request'));
    const offered = f.calls[0].configurations;
    assert.equal(offered.some(value => value.model === 'investigation'), Object.keys(requirements).length === 0);
    assert.equal(f.snapshots[0].contextKnown, false); f.router.close();
  }
});

test('known usage block and invalid requirements fail before Jev without implicit baseline replacement', async () => {
  for (const requirements of [{ ordinaryUsageAllowed: false }, { usageBlocked: true }, { contextTokens: null }, { tools: 'shell' }]) {
    const f = fixture({ requirementsForTurn: () => requirements }); f.own();
    assert.equal(await f.submit(start(1)), undefined); assert.equal(f.calls.length, 0);
    assert.match(f.received.at(-1).error.message, /nothing was sent/); f.router.close();
  }
});

test('wrong typed ACK cannot promote a pending decision; account change drops its state', async () => {
  const f = fixture(); f.own(); const first = start(1); await f.submit(first);
  f.service.observe({ ...first, id: '1' }, { result: { turn: { id: 'wrong-type' } } });
  assert.equal(f.service.threads.get('main').previousDecision, undefined);
  f.ack(first); f.complete(first); assert(f.service.threads.get('main').previousDecision);
  f.service.observe({ method: 'account/read' }, { result: { account: { type: 'chatgpt', email: 'different@example.invalid' } } });
  assert.equal(f.service.threads.get('main').previousDecision, undefined);
  assert.equal(f.service.threads.get('main').owned, false); f.router.close();
});

test('accepted null-effort switch stays exact in baseline and previous judgment for the next prompt', async () => {
  const rows = [...catalog, { ...catalog[0], model: 'no-reasoning', defaultReasoningEffort: null, supportedReasoningEfforts: [] }];
  const states = [];
  const f = fixture({ catalog: rows, judgeFactory: config => async state => {
    states.push(structuredClone(state)); return judgment(config.configurations, config.configurations.findIndex(value => value.model === 'no-reasoning'));
  } }); f.own();
  const first = start(1); const forwarded = await f.submit(first);
  assert.equal(forwarded.params.model, 'no-reasoning'); assert.equal(forwarded.params.effort, null);
  f.ack(first); f.complete(first);
  assert.deepEqual(f.service.threads.get('main').baseline, { model: 'no-reasoning', effort: null });
  const second = await f.submit(start(2)); assert(second); assert.equal(second.params.effort, null);
  assert.equal(states[1].previousDecision.effort, null);
  assert.deepEqual(states[1].baseline, { model: 'no-reasoning', effort: null }); f.router.close();
});
