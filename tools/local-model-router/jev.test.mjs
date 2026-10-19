import test from 'node:test';
import assert from 'node:assert/strict';
import { getEventListeners } from 'node:events';
import { inspect } from 'node:util';
import { QUESTIONS, JevError, createJevClient, validateJudgment } from './jev.mjs';
import { RUBRIC_VERSION, RUBRIC_INSTRUCTIONS, CONFIGURATION_INSTRUCTIONS, buildJudgmentSnapshot } from './rubric.mjs';

const API_KEY = 'test-key-SECRET_CANARY';
const SECRET = 'private-prompt-SECRET_CANARY';
const CAPABILITIES = ['bounded_routine', 'multistep_coordination', 'open_problem', 'unknown'];
const OPTIONS = { apiKey: API_KEY };
const fixture = (model = 'typesafe-ai/jev') => ({
  model,
  answers: {
    required_capability: { type: 'choice', choice: 'open_problem', confidence: 0.613, probabilities: { bounded_routine: 0.1, multistep_coordination: 0.15, open_problem: 0.7, unknown: 0.05 } },
    context_sufficient: { type: 'noul', noul: 0.92 },
    high_consequence: { type: 'noul', noul: 0.23 },
  },
  usage: { input_tokens: 317, output_tokens: 34 },
});
const normalized = () => ({ capability: 'open_problem', confidence: 0.613, probabilities: { bounded_routine: 0.1, multistep_coordination: 0.15, open_problem: 0.7, unknown: 0.05 }, contextSufficient: 0.92, highConsequence: 0.23, usage: { inputTokens: 317, outputTokens: 34 }, gatewayCostUsd: null, generationId: null });
const jsonResponse = (body = fixture(), options) => new Response(JSON.stringify(body), options);
function safeError(code) {
  return error => {
    assert.ok(error instanceof JevError);
    assert.equal(error.code, code);
    assert.equal(error.name, 'JevError');
    assert.equal(error.cause, undefined);
    for (const text of [String(error), error.stack, JSON.stringify(error), inspect(error, { showHidden: true })]) {
      assert.ok(!text.includes(API_KEY));
      assert.ok(!text.includes(SECRET));
    }
    return true;
  };
}
function mocked(fetchImpl, options = {}) {
  let calls = 0;
  const judge = createJevClient({ ...OPTIONS, ...options, fetchImpl: (...args) => { calls++; return fetchImpl(...args); } });
  return { judge, calls: () => calls };
}
function streamResponse(chunks, { status = 200, headers, finish = true, cancel = () => {} } = {}) {
  return new Response(new ReadableStream({ start(controller) {
    for (const chunk of chunks) controller.enqueue(typeof chunk === 'string' ? new TextEncoder().encode(chunk) : chunk);
    if (finish) controller.close();
  }, cancel }), { status, headers });
}

test('exports exactly the four agreed entry points', async () => {
  assert.deepEqual(Object.keys(await import('./jev.mjs')).sort(), ['QUESTIONS', 'JevError', 'createJevClient', 'validateJudgment'].sort());
});

test('three complete independent questions have immutable closed criteria', () => {
  assert.deepEqual(Object.keys(QUESTIONS), ['required_capability', 'context_sufficient', 'high_consequence']);
  assert.equal(QUESTIONS.required_capability.type, 'choice');
  assert.deepEqual(Object.keys(QUESTIONS.required_capability.criteria), CAPABILITIES);
  for (const [id, question] of Object.entries(QUESTIONS)) {
    assert.ok(question.instructions.length > 50);
    for (const otherId of Object.keys(QUESTIONS).filter(key => key !== id)) assert.ok(!question.instructions.includes(otherId));
    assert.ok(Object.isFrozen(question));
    assert.ok(Object.isFrozen(question.criteria));
  }
  for (const key of ['context_sufficient', 'high_consequence']) {
    assert.equal(QUESTIONS[key].type, 'noul');
    assert.deepEqual(Object.keys(QUESTIONS[key].criteria), ['true', 'false']);
  }
  assert.ok(Object.isFrozen(QUESTIONS));
});

test('policy questions expose the versioned general rubric without candidate selection or cross-question dependencies', async () => {
  const state = buildJudgmentSnapshot({ input: 'A self-contained task', baseline: { model: 'baseline', effort: 'max' } });
  const f = mocked((_, init) => {
    const { questions } = JSON.parse(init.body);
    assert.deepEqual(questions, QUESTIONS);
    assert.ok(questions.required_capability.instructions.includes(RUBRIC_INSTRUCTIONS));
    assert.doesNotMatch(questions.required_capability.instructions, /\bc0\b|select exact model/);
    for (const question of Object.values(questions)) {
      assert.ok(question.instructions.includes(RUBRIC_VERSION));
      assert.match(question.instructions, /prior judgment, not task truth/);
      assert.match(question.instructions, /Current explicit task overrides/);
      assert.match(question.instructions, /short|Short/);
    }
    return jsonResponse();
  });
  assert.deepEqual(await f.judge(state), normalized());
  assert.equal(f.calls(), 1);
});

test('both outbound adapters actually transmit offered-only API reference prices and refresh expiry per call', async () => {
  const originalNow = Date.now;
  let now = Date.parse('2026-10-18T12:00:00Z');
  Date.now = () => now;
  try {
    for (const provider of ['gateway', 'typesafe']) {
      const configurations = [
        { model: 'gpt-6-luna', effort: 'low', description: 'native Luna' },
        { model: 'gpt-6-astra', effort: 'high', description: 'native Astra' },
        { model: 'gpt-6.1-sol', effort: 'medium', description: 'native Sol' },
      ];
      let calls = 0;
      const judge = createJevClient({ apiKey: API_KEY, provider, configurations, fetchImpl: async (_, init) => {
        calls++;
        const request = JSON.parse(init.body);
        assert.deepEqual(request.questions.required_capability, QUESTIONS.required_capability);
        assert.deepEqual(Object.values(request.questions.configuration.criteria), configurations);
        const instructions = request.questions.configuration.instructions;
        if (calls === 1) {
          const ref = JSON.parse(instructions.split('\nAPI_PRICE_REFERENCE=')[1]);
          assert.deepEqual(Object.keys(ref.models), configurations.map(item => item.model));
          assert.equal(ref.models['gpt-6-astra'].tiers[0].output, 50);
          assert.equal(ref.models['gpt-6.1-sol'].tiers[1].input, 4);
          assert.match(instructions, /not subscription quota or an actual native bill/);
        } else assert.equal(instructions, CONFIGURATION_INSTRUCTIONS);
        assert.ok(Buffer.byteLength(init.body) <= 32 * 1024);
        const body = fixture(provider === 'gateway' ? 'typesafe-ai/jev' : 'jev-1.14.0');
        body.answers.configuration = { type: 'choice', choice: 'c1', confidence: 0.9,
          probabilities: { c0: 0.1, c1: 0.8, c2: 0.1 } };
        return jsonResponse(body);
      } });
      now = Date.parse('2026-10-18T12:00:00Z');
      assert.equal((await judge({ currentInput: 'A supplied task' })).configuration.model, 'gpt-6-astra');
      now = Date.parse('2026-11-18T00:00:00Z');
      assert.equal((await judge({ currentInput: 'A supplied task' })).configuration.effort, 'high');
      assert.equal(calls, 2);
    }
  } finally { Date.now = originalNow; }
});

test('normalization preserves measured values, numeric Nouls, and documented usage', () => {
  const body = fixture(), before = structuredClone(body);
  const result = validateJudgment(body);
  assert.deepEqual(result, normalized());
  assert.deepEqual(body, before);
  result.probabilities.open_problem = 0;
  assert.equal(body.answers.required_capability.probabilities.open_problem, 0.7);
});

test('direct TypeSafe physical Jev versions preserve the exact normalized shape', () => {
  for (const model of ['jev-1.13.0', 'jev-1.14.0', 'jev-2.0.0']) {
    const body = fixture(model), before = structuredClone(body);
    assert.deepEqual(validateJudgment(body, { provider: 'typesafe' }), normalized());
    assert.deepEqual(body, before);
  }
});

test('Gateway defaults and explicit selection reject direct model IDs and aliases', () => {
  assert.deepEqual(validateJudgment(fixture(), { provider: 'gateway' }), normalized());
  for (const model of ['jev-1.13.0', 'jev-1.14.0', 'jev-latest', 'jev-preview']) {
    assert.throws(() => validateJudgment(fixture(model)), safeError('invalid-response'));
    assert.throws(() => validateJudgment(fixture(model), { provider: 'gateway' }), safeError('invalid-response'));
  }
});

test('direct model validation rejects aliases, malformed versions, and coercion', () => {
  for (const model of ['typesafe-ai/jev', 'jev-latest', 'jev-preview', 'jev', 'jev-1.13', 'jev-1.13.0.1', 'jev-01.13.0', 'jev-1.013.0', 'jev-1.13.00', 'jev-1.13.0-beta', 'jev-1.13.0+build', 'other-1.13.0', ' jev-1.13.0', 'jev-1.13.0\n', 'jev-1.13.0\r', 'jev-1.13.0' + SECRET, null, 1, {}, ['jev-1.13.0'], { toString() { throw new Error(API_KEY + SECRET); } }]) {
    assert.throws(() => validateJudgment(fixture(model), { provider: 'typesafe' }), safeError('invalid-response'));
  }
});

test('all capability enums, ties, endpoints, and zero token counts are valid', () => {
  for (const capability of CAPABILITIES) {
    const body = fixture();
    Object.assign(body.answers.required_capability, { choice: capability, confidence: 0, probabilities: Object.fromEntries(CAPABILITIES.map(key => [key, 0.25])) });
    body.answers.context_sufficient.noul = 0;
    body.answers.high_consequence.noul = 1;
    body.usage = { input_tokens: 0, output_tokens: 0 };
    assert.equal(validateJudgment(body).capability, capability);
  }
});

test('distribution sum tolerance is 1e-6 and never renormalizes the measurements', () => {
  const body = fixture();
  body.answers.required_capability.probabilities.open_problem += 5e-7;
  assert.equal(validateJudgment(body).probabilities.open_problem, 0.7000004999999999);
  body.answers.required_capability.probabilities.open_problem += 2e-6;
  assert.throws(() => validateJudgment(body), safeError('invalid-response'));
});

const malformed = [
  ['null body', () => null], ['array body', () => []],
  ['missing model', body => { delete body.model; }], ['different model / implicit fallback', body => { body.model = 'some-other-model'; }],
  ['missing answers', body => { delete body.answers; }], ['answers array', body => { body.answers = []; }],
  ['missing answer', body => { delete body.answers.high_consequence; }], ['extra answer', body => { body.answers.extra = SECRET; }],
  ['unknown enum', body => { body.answers.required_capability.choice = SECRET; }],
  ['wrong Choice type', body => { body.answers.required_capability.type = 'noul'; }],
  ['inconsistent choice', body => { body.answers.required_capability.choice = 'bounded_routine'; }],
  ['missing confidence', body => { delete body.answers.required_capability.confidence; }],
  ['string confidence', body => { body.answers.required_capability.confidence = '0.7'; }],
  ['NaN confidence', body => { body.answers.required_capability.confidence = NaN; }],
  ['infinite confidence', body => { body.answers.required_capability.confidence = Infinity; }],
  ['negative confidence', body => { body.answers.required_capability.confidence = -0.1; }],
  ['confidence above one', body => { body.answers.required_capability.confidence = 1.1; }],
  ['empty fallback distribution', body => { Object.assign(body.answers.required_capability, { confidence: 0, probabilities: {} }); }],
  ['array distribution', body => { body.answers.required_capability.probabilities = [0.1, 0.15, 0.7, 0.05]; }],
  ['missing distribution key', body => { delete body.answers.required_capability.probabilities.unknown; }],
  ['extra distribution key', body => { body.answers.required_capability.probabilities[SECRET] = 0; }],
  ['wrong distribution key', body => { body.answers.required_capability.probabilities[SECRET] = 0.05; delete body.answers.required_capability.probabilities.unknown; }],
  ['string probability', body => { body.answers.required_capability.probabilities.unknown = '0.05'; }],
  ['NaN probability', body => { body.answers.required_capability.probabilities.unknown = NaN; }],
  ['infinite probability', body => { body.answers.required_capability.probabilities.unknown = Infinity; }],
  ['negative probability', body => { body.answers.required_capability.probabilities.unknown = -0.05; }],
  ['probability above one', body => { body.answers.required_capability.probabilities.open_problem = 1.1; }],
  ['bad distribution sum', body => { body.answers.required_capability.probabilities.open_problem = 0.6; }],
  ['missing Noul', body => { delete body.answers.context_sufficient.noul; }],
  ['Noul boolean', body => { body.answers.context_sufficient.noul = true; }],
  ['Noul string', body => { body.answers.context_sufficient.noul = '0.92'; }],
  ['Noul object', body => { body.answers.context_sufficient.noul = { confidence: 0.92 }; }],
  ['Noul confidence', body => { body.answers.context_sufficient.confidence = 0.9; }],
  ['Noul NaN', body => { body.answers.high_consequence.noul = NaN; }],
  ['Noul out of range', body => { body.answers.high_consequence.noul = 1.01; }],
  ['wrong Noul type', body => { body.answers.high_consequence.type = 'choice'; }],
  ['missing usage', body => { delete body.usage; }], ['usage array', body => { body.usage = []; }],
  ['missing input tokens', body => { delete body.usage.input_tokens; }],
  ['missing output tokens', body => { delete body.usage.output_tokens; }],
  ['camel-case usage', body => { body.usage = { inputTokens: 317, outputTokens: 34 }; }],
  ['null usage tokens', body => { body.usage.input_tokens = null; }],
  ['negative usage', body => { body.usage.input_tokens = -1; }],
  ['fractional usage', body => { body.usage.output_tokens = 1.5; }],
  ['unsafe usage integer', body => { body.usage.output_tokens = Number.MAX_SAFE_INTEGER + 1; }],
  ['infinite usage', body => { body.usage.output_tokens = Infinity; }],
  ['string usage', body => { body.usage.output_tokens = '34'; }],
];
for (const [label, change] of malformed) {
  test(`schema rejects ${label} without exposing source data`, () => {
    const body = fixture();
    const replacement = change(body);
    assert.throws(() => validateJudgment(replacement === undefined ? body : replacement), safeError('invalid-response'));
  });
}

test('optional Gateway metadata reads only documented cost and generationId', () => {
  const body = fixture();
  body.provider_metadata = { gateway: { cost: '0.00001155', marketCost: '100', surchargeCost: '200', gatewayCost: '999', generationId: 'gen_abc-123_DEF', routing: { finalProvider: 'typesafe-ai' } } };
  assert.deepEqual(validateJudgment(body), { ...normalized(), gatewayCostUsd: 0.00001155, generationId: 'gen_abc-123_DEF' });
  body.provider_metadata.gateway.cost = '0';
  assert.equal(validateJudgment(body).gatewayCostUsd, 0);
});

test('absent metadata and individual missing fields remain unknown, without guessing', () => {
  for (const metadata of [{}, { gateway: {} }, { gateway: { gatewayCost: '4', marketCost: '5', surchargeCost: '6' } }]) {
    const body = fixture(); body.provider_metadata = metadata;
    assert.deepEqual(validateJudgment(body), normalized());
  }
  const body = fixture(); body.provider_metadata = { gateway: { generationId: 'gen_only' } };
  assert.equal(validateJudgment(body).gatewayCostUsd, null);
  body.provider_metadata = { gateway: { cost: '0.125' } };
  assert.equal(validateJudgment(body).generationId, null);
});

for (const cost of [null, 0.01, '', '-1', '+1', '1e-3', 'NaN', 'Infinity', ' 0.1 ', '01.2', '.1', '1.', '9'.repeat(65), '9007199254740992', SECRET]) {
  test(`malformed Gateway cost ${String(cost).includes(SECRET) ? 'canary' : JSON.stringify(cost)} is rejected`, () => {
    const body = fixture(); body.provider_metadata = { gateway: { cost } };
    assert.throws(() => validateJudgment(body), safeError('invalid-response'));
  });
}
for (const generationId of [null, 1, '', 'gen_', 'other_abc', 'gen_' + 'a'.repeat(129), 'gen_\n' + SECRET, 'gen_../' + SECRET]) {
  test('malformed Gateway generation ID is rejected safely', () => {
    const body = fixture(); body.provider_metadata = { gateway: { generationId } };
    assert.throws(() => validateJudgment(body), safeError('invalid-response'));
  });
}

test('one fixed authenticated POST carries the full state and questions without fallback options', async () => {
  const state = { prompt: '원인 분석 🐈\n' + SECRET, recent: [{ role: 'user', text: 'earlier' }] };
  const before = structuredClone(state);
  const f = mocked((url, options) => {
    assert.equal(url, 'https://ai-gateway.vercel.sh/typesafe/v1/systemone');
    assert.equal(options.method, 'POST'); assert.equal(options.redirect, 'error');
    assert.deepEqual(options.headers, { Authorization: `Bearer ${API_KEY}`, 'Content-Type': 'application/json' });
    assert.ok(options.signal instanceof AbortSignal); assert.equal(options.signal.aborted, false);
    assert.deepEqual(JSON.parse(options.body), { model: 'typesafe-ai/jev', state, questions: QUESTIONS });
    return jsonResponse();
  });
  assert.deepEqual(await f.judge(state), normalized());
  assert.equal(f.calls(), 1); assert.deepEqual(state, before);
});

for (const [provider, endpoint, requestModel, responseModel] of [
  ['gateway', 'https://ai-gateway.vercel.sh/typesafe/v1/systemone', 'typesafe-ai/jev', 'typesafe-ai/jev'],
  ['typesafe', 'https://api.typesafe.ai/v1/systemone', 'jev-latest', 'jev-1.13.0'],
]) {
  test(`explicit ${provider} uses its fixed endpoint, Bearer auth, and model contract`, async () => {
    const state = { prompt: '원인 분석 🐈\n' + SECRET, recent: [{ role: 'user', text: 'earlier' }] };
    const before = structuredClone(state);
    const f = mocked((url, options) => {
      assert.equal(url, endpoint);
      assert.equal(options.method, 'POST'); assert.equal(options.redirect, 'error');
      assert.deepEqual(options.headers, { Authorization: `Bearer ${API_KEY}`, 'Content-Type': 'application/json' });
      assert.ok(options.signal instanceof AbortSignal); assert.equal(options.signal.aborted, false);
      assert.deepEqual(JSON.parse(options.body), { model: requestModel, state, questions: QUESTIONS });
      return jsonResponse(fixture(responseModel));
    }, { provider });
    assert.deepEqual(await f.judge(state), normalized());
    assert.equal(f.calls(), 1); assert.deepEqual(state, before);
  });
}

test('transport rejects model responses belonging to the other provider without retry', async () => {
  for (const [provider, model] of [['typesafe', 'typesafe-ai/jev'], ['typesafe', 'jev-latest'], ['gateway', 'jev-1.13.0'], ['gateway', 'jev-latest'], [undefined, 'jev-1.13.0']]) {
    const f = mocked(() => jsonResponse(fixture(model)), { provider });
    await assert.rejects(f.judge(SECRET), safeError('invalid-response'));
    assert.equal(f.calls(), 1);
  }
});

test('direct request byte boundary includes its own model, state, and unchanged questions', async () => {
  const state = '한글 🐈\n"\\';
  const bytes = Buffer.byteLength(JSON.stringify({ model: 'jev-latest', state, questions: QUESTIONS }));
  const accepted = mocked(() => jsonResponse(fixture('jev-1.13.0')), { provider: 'typesafe', maxRequestBytes: bytes });
  assert.deepEqual(await accepted.judge(state), normalized());
  const denied = mocked(() => { throw new Error(SECRET); }, { provider: 'typesafe', maxRequestBytes: bytes - 1 });
  await assert.rejects(denied.judge(state), safeError('request-too-large'));
  assert.equal(denied.calls(), 0);
});

test('invalid providers fail both entry points safely before transport or coercion', () => {
  let calls = 0;
  const fetchImpl = () => { calls++; throw new Error(API_KEY + SECRET); };
  for (const provider of [null, '', 'TYPESAFE', 'vercel', 'toString', '__proto__', API_KEY + SECRET, 0, true, {}, ['typesafe'], Symbol('typesafe'), { toString() { throw new Error(API_KEY + SECRET); } }]) {
    assert.throws(() => createJevClient({ ...OPTIONS, provider, fetchImpl }), safeError('invalid-config'));
    assert.throws(() => validateJudgment(fixture(), { provider }), safeError('invalid-config'));
  }
  assert.equal(calls, 0);
});

test('UTF-8 request bound includes questions, criteria, envelope, and JSON escaping', async () => {
  const state = '한글 🐈\n"\\';
  const bytes = Buffer.byteLength(JSON.stringify({ model: 'typesafe-ai/jev', state, questions: QUESTIONS }));
  const accepted = mocked(() => jsonResponse(), { maxRequestBytes: bytes });
  assert.deepEqual(await accepted.judge(state), normalized());
  const denied = mocked(() => { throw new Error(SECRET); }, { maxRequestBytes: bytes - 1 });
  await assert.rejects(denied.judge(state), safeError('request-too-large'));
  assert.equal(denied.calls(), 0);
  const questionsAlone = mocked(() => jsonResponse(), { maxRequestBytes: 1 });
  await assert.rejects(questionsAlone.judge(''), safeError('request-too-large'));
  assert.equal(questionsAlone.calls(), 0);
});

test('invalid state serialization is rejected before transport without leaking secrets', async () => {
  const cycle = {}; cycle.self = cycle;
  const poison = { toJSON() { throw new Error(API_KEY + SECRET); } };
  for (const state of [undefined, null, 1, true, () => {}, { n: Infinity }, { n: NaN }, { n: 1n }, { n: undefined }, cycle, poison]) {
    const f = mocked(() => jsonResponse());
    await assert.rejects(f.judge(state), safeError('invalid-request'));
    assert.equal(f.calls(), 0);
  }
});

test('invalid options are rejected with a fixed configuration error', () => {
  for (const options of [{ apiKey: undefined }, { apiKey: '' }, { apiKey: API_KEY + '\n' + SECRET }, { fetchImpl: null }, ...[0, -1, NaN, Infinity, 0.5, 2 ** 31].map(timeoutMs => ({ timeoutMs })), ...[0, -1, Infinity, NaN, 0.5, 32 * 1024 * 1024 + 1].flatMap(value => [{ maxRequestBytes: value }, { maxResponseBytes: value }])]) {
    assert.throws(() => createJevClient({ ...OPTIONS, fetchImpl: () => jsonResponse(), ...options }), safeError('invalid-config'));
  }
  safeError('network-error')(new JevError(API_KEY + SECRET));
});

for (const [status, code] of [[401, 'unauthorized'], [402, 'payment-required'], [429, 'rate-limited'], [400, 'http-error'], [403, 'http-error'], [422, 'http-error'], [500, 'http-error'], [529, 'http-error'], [302, 'http-error']]) {
  test(`HTTP ${status} is sanitized, cancelled, and never retried`, async () => {
    let cancelled = 0, read = 0, internalSignal;
    const f = mocked((_, options) => {
      internalSignal = options.signal;
      return { status, statusText: API_KEY + SECRET, headers: new Headers({ location: 'https://untrusted.invalid/' + API_KEY }), body: { getReader() { read++; throw new Error(SECRET); }, cancel() { cancelled++; } } };
    });
    await assert.rejects(f.judge({ prompt: SECRET }), safeError(code));
    assert.equal(f.calls(), 1); assert.equal(read, 0); assert.equal(cancelled, 1); assert.equal(internalSignal.aborted, true);
  });
}

test('redirected responses are rejected even from a nonconforming fetch implementation', async () => {
  const f = mocked(() => ({ status: 200, redirected: true }));
  await assert.rejects(f.judge('test'), safeError('http-error'));
  assert.equal(f.calls(), 1);
});

test('TLS errors are classified through the fetch cause and never retried or exposed', async () => {
  for (const code of ['UNABLE_TO_GET_ISSUER_CERT_LOCALLY', 'DEPTH_ZERO_SELF_SIGNED_CERT', 'ERR_TLS_CERT_ALTNAME_INVALID', 'ERR_SSL_WRONG_VERSION_NUMBER']) {
    const f = mocked(() => { throw new TypeError(API_KEY + SECRET, { cause: Object.assign(new Error(SECRET), { code }) }); });
    await assert.rejects(f.judge('test'), safeError('tls-error'));
    assert.equal(f.calls(), 1);
  }
});

test('network failures expose neither raw errors nor their causes', async () => {
  const f = mocked(() => Promise.reject(Object.assign(new Error(API_KEY + SECRET), { code: SECRET })));
  await assert.rejects(f.judge('test'), safeError('network-error'));
  assert.equal(f.calls(), 1);
});

test('deadline rejects a stalled fetch even when the implementation ignores abort', { timeout: 1000 }, async () => {
  let internalSignal;
  const f = mocked((_, options) => { internalSignal = options.signal; return new Promise(() => {}); }, { timeoutMs: 20 });
  await assert.rejects(f.judge('test'), safeError('timeout'));
  assert.equal(internalSignal.aborted, true); assert.equal(f.calls(), 1);
});

test('deadline includes a slow body after successful headers and cancels its stream', { timeout: 1000 }, async () => {
  let cancelled = 0, internalSignal;
  const f = mocked((_, options) => {
    internalSignal = options.signal;
    return streamResponse(['{'], { finish: false, cancel() { cancelled++; } });
  }, { timeoutMs: 20 });
  await assert.rejects(f.judge('test'), safeError('timeout'));
  assert.equal(internalSignal.aborted, true); assert.equal(cancelled, 1);
});

test('pre-cancel prevents transport and ignores the private abort reason', async () => {
  const controller = new AbortController(); controller.abort(new Error(API_KEY + SECRET));
  const f = mocked(() => jsonResponse());
  await assert.rejects(f.judge('test', { signal: controller.signal }), safeError('aborted'));
  assert.equal(f.calls(), 0); assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
});

test('cancellation during fetch returns promptly, aborts transport, and removes listeners', { timeout: 1000 }, async () => {
  const controller = new AbortController(); let internalSignal;
  const f = mocked((_, options) => { internalSignal = options.signal; queueMicrotask(() => controller.abort(SECRET)); return new Promise(() => {}); });
  await assert.rejects(f.judge('test', { signal: controller.signal }), safeError('aborted'));
  assert.equal(internalSignal.aborted, true); assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
});

test('cancellation during body read cancels the reader without leaking the reason', { timeout: 1000 }, async () => {
  const controller = new AbortController(); let cancelled = 0;
  const f = mocked(() => {
    const response = streamResponse(['{'], { finish: false, cancel() { cancelled++; } });
    setImmediate(() => controller.abort(new Error(API_KEY + SECRET)));
    return response;
  });
  await assert.rejects(f.judge('test', { signal: controller.signal }), safeError('aborted'));
  assert.equal(cancelled, 1);
});

test('late fetch completion is discarded and its unread body is cancelled', { timeout: 1000 }, async () => {
  let resolveFetch, cancelled = 0;
  const f = mocked(() => new Promise(resolve => { resolveFetch = resolve; }), { timeoutMs: 20 });
  await assert.rejects(f.judge('test'), safeError('timeout'));
  resolveFetch(streamResponse([], { finish: false, cancel() { cancelled++; } }));
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(cancelled, 1);
});

test('successful completion removes external abort listeners and leaves later calls independent', async () => {
  const controller = new AbortController(); const f = mocked(() => jsonResponse());
  await f.judge('one', { signal: controller.signal });
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  controller.abort(SECRET);
  assert.deepEqual(await f.judge('two'), normalized());
});

test('stream reader respects byte limit without trusting a missing or false Content-Length', async () => {
  for (const headers of [undefined, { 'content-length': '1' }, { 'content-length': 'garbage' }]) {
    let cancelled = 0;
    const f = mocked(() => streamResponse(['🐈', '🐈'], { headers, finish: false, cancel() { cancelled++; } }), { maxResponseBytes: 7 });
    await assert.rejects(f.judge('test'), safeError('response-too-large'));
    assert.equal(cancelled, 1); assert.equal(f.calls(), 1);
  }
});

test('declared oversize response is rejected before acquiring a reader', async () => {
  let read = 0, cancelled = 0;
  const f = mocked(() => ({ status: 200, headers: new Headers({ 'content-length': '99999' }), body: { getReader() { read++; }, cancel() { cancelled++; } } }), { maxResponseBytes: 100 });
  await assert.rejects(f.judge('test'), safeError('response-too-large'));
  assert.equal(read, 0); assert.equal(cancelled, 1);
});

test('bounded streamed JSON supports split UTF-8 and the exact response byte boundary', async () => {
  const body = fixture(); body.note = '한글 🐈';
  const bytes = Buffer.from(JSON.stringify(body));
  const f = mocked(() => streamResponse(Array.from(bytes, byte => Uint8Array.of(byte))), { maxResponseBytes: bytes.length });
  assert.deepEqual(await f.judge('test'), normalized());
  const denied = mocked(() => streamResponse([bytes]), { maxResponseBytes: bytes.length - 1 });
  await assert.rejects(denied.judge('test'), safeError('response-too-large'));
});

test('invalid JSON, UTF-8, empty bodies, and schema failures are safe', async () => {
  for (const response of [new Response(API_KEY + SECRET), new Response('{'), new Response(''), new Response(Uint8Array.of(0xff)), jsonResponse({ secret: API_KEY + SECRET }), new Response(null)]) {
    const f = mocked(() => response);
    await assert.rejects(f.judge('test'), safeError('invalid-response'));
    assert.equal(f.calls(), 1);
  }
});

test('body transport failure is sanitized and cancels the internal request', async () => {
  let internalSignal;
  const f = mocked((_, options) => {
    internalSignal = options.signal;
    return new Response(new ReadableStream({ start(controller) { controller.error(new Error(API_KEY + SECRET)); } }));
  });
  await assert.rejects(f.judge('test'), safeError('network-error'));
  assert.equal(internalSignal.aborted, true);
});

test('a hanging stream cancellation cannot hold the client past its deadline', { timeout: 1000 }, async () => {
  const f = mocked(() => streamResponse(['{'], { finish: false, cancel() { return new Promise(() => {}); } }), { timeoutMs: 20 });
  await assert.rejects(f.judge('test'), safeError('timeout'));
});

const configurations = [
  { model: 'baseline', effort: 'max', description: 'Native general problem-solving model; maximum reasoning.' },
  { model: 'toString', effort: 'low', description: 'Native model for bounded edits; low reasoning.' },
];
function selectionFixture(model = 'typesafe-ai/jev') {
  const body = fixture(model);
  body.answers.configuration = { type: 'choice', choice: 'c1', confidence: 0.7, probabilities: { c0: 0.1, c1: 0.9 } };
  return body;
}

test('optional configuration Choice shares one real client call and normalizes exact supplied model/effort for both providers', async () => {
  for (const [provider, responseModel] of [['gateway', 'typesafe-ai/jev'], ['typesafe', 'jev-1.13.0']]) {
    const state = { currentInput: 'bounded edit', baseline: { model: 'baseline', effort: 'max' }, contextKnown: true };
    const before = structuredClone(configurations);
    const f = mocked((_, init) => {
      const request = JSON.parse(init.body);
      assert.deepEqual(Object.keys(request.questions), [...Object.keys(QUESTIONS), 'configuration']);
      for (const name of Object.keys(QUESTIONS)) assert.deepEqual(request.questions[name], QUESTIONS[name]);
      assert.equal(request.questions.configuration.type, 'choice');
      assert.equal(request.questions.configuration.instructions, CONFIGURATION_INSTRUCTIONS);
      assert.deepEqual(request.questions.configuration.criteria, { c0: before[0], c1: before[1] });
      assert.match(request.questions.configuration.instructions, /least sufficient capacity/);
      assert.match(request.questions.configuration.instructions, /high consequence/);
      assert.match(request.questions.configuration.instructions, /guaranteed cost optimum/);
      assert.ok(!init.body.includes(API_KEY));
      return jsonResponse(selectionFixture(responseModel));
    }, { provider, configurations });
    const result = await f.judge(state);
    assert.deepEqual(result, { ...normalized(), configuration: {
      model: 'toString', effort: 'low', choice: 'c1', confidence: 0.7, probabilities: { c0: 0.1, c1: 0.9 },
    } });
    assert.deepEqual(configurations, before); assert.equal(f.calls(), 1);
    assert.deepEqual(Object.keys(QUESTIONS), ['required_capability', 'context_sufficient', 'high_consequence']);
  }
});

test('configuration criteria are snapshotted before callers mutate their source objects', async () => {
  const source = structuredClone(configurations);
  const f = mocked((_, init) => {
    assert.equal(JSON.parse(init.body).questions.configuration.criteria.c1.model, 'toString');
    return jsonResponse(selectionFixture());
  }, { configurations: source });
  source[1].model = 'changed'; source.push({ ...source[0], model: 'extra' });
  assert.equal((await f.judge('request')).configuration.model, 'toString');
});

test('configuration schema rejects missing, extra, nonmaximal, unbounded, or malformed option distributions', () => {
  const changes = [
    body => { delete body.answers.configuration; },
    body => { body.answers.configuration.extra = SECRET; },
    body => { body.answers.configuration.type = 'noul'; },
    body => { body.answers.configuration.choice = 'baseline'; },
    body => { body.answers.configuration.choice = 'c2'; },
    body => { body.answers.configuration.choice = 'c0'; },
    body => { body.answers.configuration.probabilities = {}; },
    body => { body.answers.configuration.probabilities = [0.1, 0.9]; },
    body => { delete body.answers.configuration.probabilities.c0; },
    body => { body.answers.configuration.probabilities.c2 = 0; },
    body => { body.answers.configuration.probabilities.c1 = '0.9'; },
    body => { body.answers.configuration.probabilities.c1 = NaN; },
    body => { body.answers.configuration.probabilities.c0 = -0.1; },
    body => { body.answers.configuration.probabilities.c1 = 1.1; },
    body => { body.answers.configuration.probabilities.c1 = 0.8; },
    ...[undefined, null, '0.7', NaN, Infinity, -0.1, 1.1].map(confidence => body => { body.answers.configuration.confidence = confidence; }),
  ];
  for (const change of changes) {
    const body = selectionFixture(); change(body);
    assert.throws(() => validateJudgment(body, { configurations }), safeError('invalid-response'));
  }
  assert.throws(() => validateJudgment(selectionFixture()), safeError('invalid-response'));
  assert.throws(() => validateJudgment(fixture(), { configurations }), safeError('invalid-response'));
});

test('configuration sum tolerance, ties, and null effort preserve measured values', () => {
  const supplied = [{ ...configurations[0], effort: null }, configurations[1]];
  const body = selectionFixture();
  body.answers.configuration = { type: 'choice', choice: 'c0', confidence: 0, probabilities: { c0: 0.5, c1: 0.5 } };
  const result = validateJudgment(body, { configurations: supplied });
  assert.equal(result.configuration.effort, null);
  body.answers.configuration.probabilities.c0 += 5e-7;
  assert.equal(validateJudgment(body, { configurations: supplied }).configuration.probabilities.c0, 0.5000005);
  body.answers.configuration.probabilities.c0 += 2e-6;
  assert.throws(() => validateJudgment(body, { configurations: supplied }), safeError('invalid-response'));
});

test('Gateway and direct TypeSafe configuration cent rounding preserve raw measurements with an invariant winner', () => {
  for (const [provider, model] of [['gateway', 'typesafe-ai/jev'], ['typesafe', 'jev-1.13.0']]) {
    for (const probabilities of [{ c0: 0.04, c1: 0.95 }, { c0: 0.06, c1: 0.95 }]) {
      const body = selectionFixture(model); body.answers.configuration.probabilities = probabilities;
      const result = validateJudgment(body, { provider, configurations });
      assert.deepEqual(result.configuration.probabilities, probabilities);
      assert.equal(result.configuration.choice, 'c1');
    }
  }
});

test('rounded configuration acceptance never weakens capability, malformed sums, or ambiguous winners', () => {
  for (const [provider, model] of [['gateway', 'typesafe-ai/jev'], ['typesafe', 'jev-1.13.0']]) {
    for (const probabilities of [{ c0: 0.49, c1: 0.5 }, { c0: 0.1, c1: 0.8 }, { c0: 0.041, c1: 0.95 }]) {
      const body = selectionFixture(model); body.answers.configuration.probabilities = probabilities;
      assert.throws(() => validateJudgment(body, { provider, configurations }), safeError('invalid-response'));
    }
    const body = selectionFixture(model); body.answers.required_capability.probabilities.open_problem = 0.69;
    assert.throws(() => validateJudgment(body, { provider, configurations }), safeError('invalid-response'));
  }
});

test('32-option rounded configuration for both providers is validated once without renormalization or retry', async () => {
  for (const [provider, model] of [['gateway', 'typesafe-ai/jev'], ['typesafe', 'jev-1.13.0']]) {
    const supplied = Array.from({ length: 32 }, (_, i) => ({ model: `native-${i}`, effort: 'low', description: 'Native bounded work.' }));
    const probabilities = Object.fromEntries(supplied.map((_, i) => [`c${i}`, i === 12 ? 0.74 : i < 26 ? 0.01 : 0]));
    const body = selectionFixture(model);
    body.answers.configuration = { type: 'choice', choice: 'c12', confidence: 0.7, probabilities };
    const f = mocked(() => jsonResponse(body), { provider, configurations: supplied });
    const result = await f.judge('Self-contained bounded request');
    assert.equal(f.calls(), 1);
    assert.equal(result.configuration.model, 'native-12');
    assert.deepEqual(result.configuration.probabilities, probabilities);
    assert.ok(Math.abs(Object.values(probabilities).reduce((a, b) => a + b, 0) - 0.99) < 1e-12);

    body.answers.configuration.probabilities.c12 = 0.4;
    assert.throws(() => validateJudgment(body, { provider, configurations: supplied }), safeError('invalid-response'));
    body.answers.configuration.probabilities.c12 = 0.97;
    assert.throws(() => validateJudgment(body, { provider, configurations: supplied }), safeError('invalid-response'));
  }
});

test('configuration inputs are finite, unique, bounded descriptors with fixed option keys', () => {
  for (const value of [null, {}, [], Array.from({ length: 33 }, (_, i) => ({ ...configurations[0], model: `m${i}` })),
    [configurations[0], configurations[0]], [{ ...configurations[0], key: SECRET }],
    [{ ...configurations[0], description: '한'.repeat(342) }], [{ ...configurations[0], model: SECRET + '\n' }],
    [{ ...configurations[0], effort: undefined }], [{ ...configurations[0], effort: {} }],
    Array.from({ length: 32 }, (_, i) => ({ ...configurations[0], model: `m${i}`, description: 'x'.repeat(1024) })),
  ]) {
    assert.throws(() => createJevClient({ ...OPTIONS, configurations: value }), safeError('invalid-config'));
    assert.throws(() => validateJudgment(selectionFixture(), { configurations: value }), safeError('invalid-config'));
  }
});

test('the complete four-question JSON has an exact UTF-8 byte boundary and default 32 KiB ceiling', async () => {
  let serialized;
  const probe = mocked((_, init) => { serialized = init.body; return jsonResponse(selectionFixture()); }, { configurations });
  const state = { currentInput: '한글 🐈\n"\\', baseline: { model: 'baseline', effort: 'max' } };
  await probe.judge(state);
  const bytes = Buffer.byteLength(serialized);
  const accepted = mocked(() => jsonResponse(selectionFixture()), { configurations, maxRequestBytes: bytes });
  assert.equal((await accepted.judge(state)).configuration.model, 'toString');
  const denied = mocked(() => jsonResponse(selectionFixture()), { configurations, maxRequestBytes: bytes - 1 });
  await assert.rejects(denied.judge(state), safeError('request-too-large')); assert.equal(denied.calls(), 0);
  const oversized = mocked(() => jsonResponse(selectionFixture()), { configurations });
  await assert.rejects(oversized.judge({ currentInput: '\\"한글'.repeat(10_000) }), safeError('request-too-large'));
  assert.equal(oversized.calls(), 0);
});

test('full 8 KiB snapshot plus full 8 KiB configuration criteria and complete rubric fit the 24 KiB request bound', async () => {
  const supplied = Array.from({ length: 9 }, (_, i) => ({ model: `native-${i}`, effort: 'high', description: '' }));
  const criteria = () => Object.fromEntries(supplied.map((value, i) => [`c${i}`, value]));
  let remaining = 8192 - Buffer.byteLength(JSON.stringify(criteria()));
  for (const value of supplied) {
    const length = Math.min(1024, remaining);
    value.description = 'x'.repeat(length);
    remaining -= length;
  }
  assert.equal(remaining, 0);
  assert.equal(Buffer.byteLength(JSON.stringify(criteria())), 8192);
  const state = buildJudgmentSnapshot({ input: 'x'.repeat(100_000), baseline: { model: 'native-0', effort: 'high' },
    previousDecision: { model: 'native-0', effort: 'high', capability: 'open_problem',
      contextSufficient: 0.5, highConsequence: 0.5, turnOutcome: 'unknown' } });
  assert.equal(Buffer.byteLength(JSON.stringify(state)), 8192);
  const body = selectionFixture();
  body.answers.configuration.probabilities = Object.fromEntries(supplied.map((_, i) => [`c${i}`, i === 0 ? 0.1 : i === 1 ? 0.9 : 0]));
  const f = mocked((_, init) => {
    assert.ok(Buffer.byteLength(init.body) <= 24 * 1024);
    const request = JSON.parse(init.body);
    assert.deepEqual(request.state, state);
    assert.equal(request.questions.configuration.instructions, CONFIGURATION_INSTRUCTIONS);
    assert.deepEqual(request.questions.configuration.criteria, criteria());
    return jsonResponse(body);
  }, { configurations: supplied, maxRequestBytes: 24 * 1024 });
  assert.equal((await f.judge(state)).configuration.model, 'native-1');
  assert.equal(f.calls(), 1);
});
