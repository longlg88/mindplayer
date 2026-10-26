import test from 'node:test';
import assert from 'node:assert/strict';
import { setImmediate as tick } from 'node:timers/promises';
import { JevService } from './service.mjs';
import { ModelRouter } from './router.mjs';

const models = ['model-a', 'model-b', 'model-c'];
const catalog = models.map(model => ({
  model, hidden: false, displayName: model, description: 'Synthetic general-purpose text model.',
  inputModalities: ['text', 'image'], defaultReasoningEffort: 'medium',
  supportedReasoningEfforts: ['low', 'medium', 'high'].map(reasoningEffort => ({ reasoningEffort })),
}));
const overload = { message: 'Synthetic capacity failure', codexErrorInfo: 'serverOverloaded', additionalDetails: null };
const menuModels = call => [...new Set(call.configurations.map(value => value.model))].sort();

function judgment(configurations, { capability = 'bounded_routine', index = 0 } = {}) {
  assert.ok(configurations[index], 'the real service must offer a configuration to the judge');
  return {
    capability, confidence: 1,
    probabilities: Object.fromEntries(['bounded_routine', 'multistep_coordination', 'open_problem', 'unknown'].map(value => [value, Number(value === capability)])),
    contextSufficient: 1, highConsequence: 0, usage: { inputTokens: 1, outputTokens: 1 }, gatewayCostUsd: null,
    configuration: { ...configurations[index], choice: `c${index}`, confidence: 1,
      probabilities: Object.fromEntries(configurations.map((_, i) => [`c${i}`, Number(i === index)])) },
  };
}

function harness(t, { rows = catalog, judge = call => judgment(call.configurations), account = 'one@example.invalid' } = {}) {
  let time = 1_000_000, id = 1;
  const sent = [], received = [], audit = [], calls = [];
  const service = new JevService({
    catalog: rows, accountHome: '/synthetic/account', account: { type: 'chatgpt', email: account },
    expectedThreadId: 'main', apiKey: 'synthetic-test-key', externalPromptTransfer: true,
    requirePromptChoice: true, policyReader: () => null, now: () => time, judgmentLimits: { cooldownMs: 0 },
    judgeFactory: options => async context => {
      const call = { configurations: structuredClone(options.configurations), capacityRecovery: options.capacityRecovery,
        context: structuredClone(context) };
      calls.push(call);
      return judge(call);
    },
    audit: event => audit.push(event),
  });
  const router = new ModelRouter({ catalog: rows, service, toServer: event => sent.push(event),
    toClient: event => received.push(event), audit: event => audit.push(event) });
  t.after(() => router.close());
  const lifecycle = (threadId = 'main', { ephemeral = false, provider = 'synthetic-provider', method = 'thread/start' } = {}) => {
    const requestId = id++;
    router.client({ id: requestId, method, params: { threadId, ephemeral, sessionStartSource: 'startup' } });
    router.server({ id: requestId, result: { thread: { id: threadId, source: 'cli', ephemeral },
      model: 'model-a', reasoningEffort: 'high', modelProvider: provider } });
  };
  const request = ({ threadId = 'main', model = 'model-a', mode = 'jev', text = 'Return the number 7.' } = {}) => ({
    id: id++, method: 'turn/start', params: { threadId, model, effort: 'high',
      input: [{ type: 'text', text }], approvalPolicy: 'never', sandboxPolicy: { type: 'readOnly' },
      collaborationMode: { mode: 'default', settings: { model, reasoning_effort: 'high', developer_instructions: 'Synthetic instruction.' } },
      mindplayerRoutingChoice: { mode, includeRecent: false } },
  });
  const submit = async (options = {}) => {
    const original = request(options);
    router.client(original); await tick();
    const forwarded = sent.find(value => value.id === original.id && value.method === 'turn/start');
    assert.ok(forwarded, `request ${original.id} must reach the synthetic backend`);
    return forwarded;
  };
  const ack = (forwarded, turnId = `turn-${forwarded.id}`) => {
    router.server({ id: forwarded.id, result: { turn: { id: turnId, status: 'inProgress' } } });
    return turnId;
  };
  const complete = (turnId, { threadId = 'main', status = 'failed', error = overload } = {}) => {
    router.server({ method: 'turn/completed', params: { threadId, turn: { id: turnId, status, error } } });
  };
  const error = (turnId, { threadId = 'main', willRetry = false, value = overload } = {}) => {
    router.server({ method: 'error', params: { threadId, turnId, willRetry, error: value } });
  };
  const reroute = (turnId, fromModel, toModel) => router.server({ method: 'model/rerouted',
    params: { threadId: 'main', turnId, fromModel, toModel, reason: 'Synthetic server reroute' } });
  const fail = async (options = {}) => {
    const forwarded = await submit(options);
    const turnId = ack(forwarded);
    complete(turnId, { threadId: options.threadId ?? 'main' });
    return turnId;
  };
  const probe = async (options = {}) => {
    const decision = await service.decide(request(options));
    return { decision, call: calls.at(-1) };
  };
  lifecycle();
  return { router, service, sent, received, audit, calls, lifecycle, request, submit, ack, complete, error, reroute, fail, probe,
    now: () => time, advance: milliseconds => { time += milliseconds; } };
}

test('a correlated terminal overload removes every effort of the failed model from the next Jev menu', async t => {
  const f = harness(t);
  await f.fail();
  const { decision, call } = await f.probe();
  assert.deepEqual(menuModels(call), ['model-b', 'model-c']);
  assert.equal(decision.model, 'model-b');
});

test('capacity recovery preserves the actual baseline and uses a judge option without new snapshot fields', async t => {
  const f = harness(t);
  await f.fail();
  const { call } = await f.probe();
  assert.deepEqual(call.context.baseline, { model: 'model-a', effort: 'high' });
  assert.equal(call.capacityRecovery, true);
  assert.deepEqual(Object.keys(call.context).sort(), ['schemaVersion', 'currentInput', 'recent', 'baseline', 'contextKnown',
    'truncated', 'attachments', 'rubricVersion', 'previousDecision'].sort());
});

test('a non-retryable overload followed by a failed completion excludes the correlated model', async t => {
  const f = harness(t), turnId = f.ack(await f.submit());
  f.error(turnId);
  f.complete(turnId, { error: null });
  assert.deepEqual(menuModels((await f.probe()).call), ['model-b', 'model-c']);
});

for (const earlierError of [false, true]) {
  test(`terminal overload ${earlierError ? 'with an earlier error' : 'in the completion'} before ACK waits for the matching submission`, async t => {
    const f = harness(t), forwarded = await f.submit(), turnId = 'before-ack';
    if (earlierError) f.error(turnId);
    f.complete(turnId, { error: earlierError ? null : overload });
    f.advance(10_000);
    f.ack(forwarded, turnId);
    assert.deepEqual(menuModels((await f.probe()).call), ['model-b', 'model-c']);
    f.advance(50_000);
    assert.deepEqual(menuModels((await f.probe()).call), models, 'expiry is measured from failure, not a delayed ACK');
  });
}

test('duplicate failed completions do not extend the original 60000 millisecond exclusion', async t => {
  const f = harness(t), turnId = await f.fail();
  f.advance(30_000); f.complete(turnId);
  f.advance(29_999);
  assert.deepEqual(menuModels((await f.probe()).call), ['model-b', 'model-c']);
  f.advance(1);
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

test('successive failures exclude both models until each exact independent expiry', async t => {
  const f = harness(t);
  await f.fail();
  f.advance(10_000);
  await f.fail();
  assert.deepEqual(f.sent.filter(value => value.method === 'turn/start').map(value => value.params.model), ['model-a', 'model-b']);
  assert.deepEqual(menuModels((await f.probe()).call), ['model-c']);
  f.advance(50_000);
  assert.deepEqual(menuModels((await f.probe()).call), ['model-a', 'model-c']);
  f.advance(10_000);
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

test('an intermediate retryable overload followed by success does not exclude a model', async t => {
  const f = harness(t), turnId = f.ack(await f.submit());
  f.error(turnId, { willRetry: true });
  f.complete(turnId, { status: 'completed', error: null });
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

test('a non-retryable overload without a failed terminal outcome does not exclude a model', async t => {
  const f = harness(t), turnId = f.ack(await f.submit());
  f.error(turnId);
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

test('an interrupted turn with an overload error does not exclude a model', async t => {
  const f = harness(t), turnId = f.ack(await f.submit());
  f.complete(turnId, { status: 'interrupted' });
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

for (const code of ['unauthorized', 'usageLimitExceeded', 'httpConnectionFailed']) {
  test(`a terminal ${code} error does not become a capacity exclusion`, async t => {
    const f = harness(t), turnId = f.ack(await f.submit());
    f.complete(turnId, { error: { ...overload, codexErrorInfo: code } });
    assert.deepEqual(menuModels((await f.probe()).call), models);
  });
}

test('raw network error text mentioning serverOverloaded is not capacity evidence', async t => {
  const f = harness(t), turnId = f.ack(await f.submit());
  f.complete(turnId, { error: { message: 'serverOverloaded network failure', codexErrorInfo: null } });
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

test('an RPC rejection without an acknowledged turn never excludes or replays the requested model', async t => {
  const f = harness(t), forwarded = await f.submit();
  f.router.server({ id: forwarded.id, error: { code: -32000, message: 'Synthetic overload', data: overload } });
  await tick();
  assert.equal(f.sent.filter(value => value.method === 'turn/start').length, 1);
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

test('an unknown turn ID cannot attribute its failure to an owned active turn', async t => {
  const f = harness(t);
  f.ack(await f.submit());
  f.error('unknown-turn'); f.complete('unknown-turn');
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

test('a mismatched completion before ACK cannot borrow the subsequent acknowledged turn model', async t => {
  const f = harness(t), forwarded = await f.submit();
  f.complete('unrelated-turn'); f.ack(forwarded, 'actual-turn');
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

test('overload events from an unrelated thread do not affect the main user menu', async t => {
  const f = harness(t);
  f.error('unrelated-turn', { threadId: 'unowned' });
  f.complete('unrelated-turn', { threadId: 'unowned' });
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

test('an acknowledged helper turn cannot create a capacity exclusion for the main user', async t => {
  const f = harness(t);
  f.lifecycle('helper', { ephemeral: true });
  await f.fail({ threadId: 'helper' });
  assert.equal(f.calls.length, 0, 'verified helpers bypass Jev');
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

for (const method of ['turn/steer', 'tool-output']) {
  test(`an acknowledged ${method} request cannot establish main user turn capacity ownership`, async t => {
    const f = harness(t), request = f.request();
    if (method === 'turn/steer') request.method = method;
    else request.params.toolOutput = { output: 'Synthetic tool result' };
    f.router.client(request);
    f.ack(request, 'non-user-turn'); f.complete('non-user-turn');
    assert.equal(f.calls.length, 0);
    assert.deepEqual(menuModels((await f.probe()).call), models);
  });
}

test('native OFF preserves the blocked model without Jev judgment, switching, or automatic replay', async t => {
  const f = harness(t);
  await f.fail();
  const calls = f.calls.length, request = f.request({ mode: 'native' }), expected = structuredClone(request);
  delete expected.params.mindplayerRoutingChoice;
  f.router.client(request); await tick();
  assert.deepEqual(f.sent.at(-1), expected);
  f.complete(f.ack(expected)); await tick();
  assert.equal(f.calls.length, calls);
  assert.equal(f.sent.filter(value => value.method === 'turn/start').length, 2);
  assert.deepEqual(menuModels((await f.probe()).call), ['model-b', 'model-c']);
});

test('an exclusion applies across owned threads sharing the same account and model provider', async t => {
  const f = harness(t);
  f.lifecycle('peer');
  await f.fail();
  assert.deepEqual(menuModels((await f.probe({ threadId: 'peer' })).call), ['model-b', 'model-c']);
});

test('a capacity exclusion does not cross model providers in the same connection', async t => {
  const f = harness(t);
  f.lifecycle('peer', { provider: 'other-synthetic-provider' });
  await f.fail();
  assert.deepEqual(menuModels((await f.probe({ threadId: 'peer' })).call), models);
});

test('a capacity exclusion does not cross connections with the same synthetic account', async t => {
  const first = harness(t), second = harness(t);
  await first.fail();
  assert.deepEqual(menuModels((await second.probe()).call), models);
});

test('a capacity exclusion does not cross an account change in the same connection', async t => {
  const f = harness(t);
  await f.fail();
  f.router.client({ id: 'account-change', method: 'account/read', params: {} });
  f.router.server({ id: 'account-change', result: { account: { type: 'chatgpt', email: 'two@example.invalid' } } });
  f.lifecycle();
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

for (const mode of ['unknown capability', 'truncated input', 'image attachment', 'unknown input type']) {
  test(`an excluded baseline cannot be used as a fallback for ${mode}`, async t => {
    const f = harness(t, { judge: call => judgment(call.configurations,
      { capability: mode === 'unknown capability' ? 'unknown' : 'bounded_routine' }) });
    await f.fail();
    const request = f.request();
    if (mode === 'truncated input') request.params.input[0].text = 'x'.repeat(20_000);
    if (mode === 'image attachment') request.params.input.push({ type: 'image', url: 'data:image/png;base64,c3ludGhldGlj' });
    if (mode === 'unknown input type') request.params.input.push({ type: 'syntheticFutureInput' });
    await assert.rejects(f.service.decide(request), { code: 'capacity-no-safe-alternative' });
    assert.equal(f.calls.at(-1).capacityRecovery, true);
    assert.deepEqual(menuModels(f.calls.at(-1)), ['model-b', 'model-c']);
  });
}

test('no remaining safe candidate blocks submission with the bounded capacity error', async t => {
  const f = harness(t, { rows: [catalog[0]] });
  await f.fail();
  await assert.rejects(f.service.decide(f.request()), { code: 'capacity-no-safe-alternative' });
  const request = f.request();
  f.router.client(request); await tick();
  assert.equal(f.sent.filter(value => value.method === 'turn/start').length, 1);
  assert.ok(f.received.find(value => value.id === request.id)?.error);
  assert.ok(f.audit.some(value => value.event === 'route-failed' && value.requestId === request.id && value.reason === 'capacity-no-safe-alternative'));
});

for (const beforeAck of [false, true]) {
  test(`a valid reroute chain ${beforeAck ? 'before' : 'after'} ACK attributes overload only to its final serving model`, async t => {
    const f = harness(t), forwarded = await f.submit(), turnId = 'rerouted-turn';
    if (!beforeAck) f.ack(forwarded, turnId);
    f.reroute(turnId, 'model-a', 'model-b'); f.reroute(turnId, 'model-b', 'model-c');
    f.complete(turnId);
    if (beforeAck) f.ack(forwarded, turnId);
    assert.deepEqual(menuModels((await f.probe()).call), ['model-a', 'model-b']);
  });
}

for (const kind of ['missing source', 'wrong source', 'conflicting chain']) {
  test(`a reroute with ${kind} never guesses a model to exclude`, async t => {
    const f = harness(t), turnId = f.ack(await f.submit());
    if (kind === 'conflicting chain') f.reroute(turnId, 'model-a', 'model-b');
    f.reroute(turnId, kind === 'missing source' ? undefined : kind === 'wrong source' ? 'model-b' : 'model-a', 'model-c');
    f.complete(turnId);
    assert.deepEqual(menuModels((await f.probe()).call), models);
  });
}

test('a capacity event invalidates an outstanding decision before any backend submission', async t => {
  let release;
  const f = harness(t, { judge: call => call.context.currentInput === 'held decision'
    ? new Promise(resolve => { release = () => resolve(judgment(call.configurations)); }) : judgment(call.configurations) });
  f.lifecycle('peer');
  const turnId = f.ack(await f.submit());
  const request = f.request({ threadId: 'peer', text: 'held decision' });
  f.router.client(request); await tick();
  assert.equal(typeof release, 'function');
  f.complete(turnId); release(); await tick();
  assert.equal(f.sent.some(value => value.id === request.id && value.method === 'turn/start'), false);
  assert.ok(f.received.find(value => value.id === request.id)?.error);
});

test('an ACK for a submission sent before a same-thread lifecycle reset cannot bind the new owner', async t => {
  const f = harness(t), forwarded = await f.submit();
  f.lifecycle('main', { method: 'thread/resume' });
  f.ack(forwarded, 'old-owner-turn'); f.complete('old-owner-turn');
  assert.deepEqual(menuModels((await f.probe()).call), models);
});

test('closing an unrelated helper before the main ACK preserves the main submission capacity ownership', async t => {
  const f = harness(t), forwarded = await f.submit();
  f.lifecycle('helper', { ephemeral: true });
  f.router.server({ method: 'thread/closed', params: { threadId: 'helper' } });
  f.complete(f.ack(forwarded, 'main-after-helper-close'));
  assert.deepEqual(menuModels((await f.probe()).call), ['model-b', 'model-c']);
});

for (const beforeAck of [false, true]) {
  test(`identical reroute duplicates ${beforeAck ? 'before' : 'after'} ACK exclude the final serving model once`, async t => {
    const f = harness(t), forwarded = await f.submit(), turnId = 'duplicate-reroute-turn';
    if (!beforeAck) f.ack(forwarded, turnId);
    f.reroute(turnId, 'model-a', 'model-b');
    f.reroute(turnId, 'model-a', 'model-b');
    if (beforeAck) f.ack(forwarded, turnId);
    f.complete(turnId);
    assert.deepEqual(menuModels((await f.probe()).call), ['model-a', 'model-c']);
  });
}

test('a genuine reroute loop retains nonconsecutive repeated edges when attributing the final model', async t => {
  const f = harness(t), forwarded = await f.submit(), turnId = 'reroute-loop-turn';
  f.reroute(turnId, 'model-a', 'model-b');
  f.ack(forwarded, turnId);
  f.reroute(turnId, 'model-b', 'model-a');
  f.reroute(turnId, 'model-a', 'model-b');
  f.complete(turnId);
  assert.deepEqual(menuModels((await f.probe()).call), ['model-a', 'model-c']);
});
