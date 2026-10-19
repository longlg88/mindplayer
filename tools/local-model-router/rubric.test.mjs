import test from 'node:test';
import assert from 'node:assert/strict';
import { PolicyError } from './policy.mjs';
import { RUBRIC_VERSION, RUBRIC_INSTRUCTIONS, CONFIGURATION_INSTRUCTIONS, buildJudgmentSnapshot } from './rubric.mjs';
import * as rubric from './rubric.mjs';

const baseline = { model: 'native', effort: 'high' };
const prior = { ...baseline, capability: 'open_problem', contextSufficient: 0.93, highConsequence: 0.72, turnOutcome: 'interrupted' };
const snapshot = options => buildJudgmentSnapshot({ input: 'continue', baseline, ...options });
const bytes = value => Buffer.byteLength(JSON.stringify(value), 'utf8');
const error = code => value => value instanceof PolicyError && value.code === code;

test('versioned rubric keeps general judgment separate from candidate selection', async () => {
  assert.deepEqual(Object.keys(await import('./rubric.mjs')).sort(),
    ['RUBRIC_VERSION', 'RUBRIC_INSTRUCTIONS', 'CONFIGURATION_INSTRUCTIONS', 'buildConfigurationInstructions', 'buildJudgmentSnapshot'].sort());
  assert.equal(RUBRIC_VERSION, 'mindplayer-capacity-v2');
  assert.ok(CONFIGURATION_INSTRUCTIONS.startsWith(RUBRIC_INSTRUCTIONS));
  assert.ok(RUBRIC_INSTRUCTIONS.includes(RUBRIC_VERSION));
  assert.doesNotMatch(RUBRIC_INSTRUCTIONS, /\bc0\b|select exact model/);
  assert.match(CONFIGURATION_INSTRUCTIONS, /c0 is the exact caller-validated policy fallback/);
  assert.match(CONFIGURATION_INSTRUCTIONS, /may differ from state.baseline/);
  assert.match(CONFIGURATION_INSTRUCTIONS, /When currentFamily is supplied.*current family.*native eligibility/);
  assert.match(CONFIGURATION_INSTRUCTIONS, /Without currentFamily.*no latest-family guarantee/);
  assert.match(CONFIGURATION_INSTRUCTIONS, /Preserve c0.*ambiguous.*unclear.*truncated.*attachments/);
});

test('all five dimensions and independent prior-judgment semantics reach the rubric', () => {
  for (const pattern of [/Solution space:.*known procedure.*unresolved/, /Coordination burden:.*steps\/components/,
    /Failure consequences:.*production.*recovery/, /Task continuity:.*short.*complex.*not mean simple or cheap/,
    /Missing information:.*task references/, /Current explicit task overrides previousDecision/,
    /prior judgment, not task truth/, /turnOutcome.*not task completion/, /independently.*never other answers/,
    /contextKnown=false.*self-contained/, /truncated means cut input.*attachments means omitted non-text/]) {
    assert.match(RUBRIC_INSTRUCTIONS, pattern);
  }
});

const contrasts = [
  ['design', /Create a design:.*existing template.*new requirements\/tradeoffs.*no grade/],
  ['line correction', /Fix this incorrect line:.*known correction.*concurrency.*security.*data integrity/],
  ['development and PR', /Finish development and create a PR:.*development\/verification\/coordination.*PR creation only.*does not establish completed development/],
  ['release and deployment', /Check merge, then release\/deploy:.*known procedure.*external impact\/recovery.*no deployment permission/],
  ['daily report', /Write a daily report:.*collected material.*gathering sources.*checking conflicts.*interpreting causes.*not automatically routine/],
  ['incident analysis', /Analyze an incident:.*unknown causes.*competing hypotheses.*impact\/uncertainty.*established analysis summary.*no known cause/],
];
for (const [name, pattern] of contrasts) {
  test(`configuration rubric retains the approved ${name} contrast`, () => assert.match(CONFIGURATION_INSTRUCTIONS, pattern));
}

test('rubric contains no preset model grades, cost assumptions or numeric confidence thresholds', () => {
  assert.doesNotMatch(CONFIGURATION_INSTRUCTIONS, /\bgpt[-\d]|\bclaude[-\d]|\b0\.\d+|confidence\s*(?:>=|<=|>|<|=)/i);
  assert.match(CONFIGURATION_INSTRUCTIONS, /Model IDs\/option order are not capacity or price rankings/);
  assert.match(CONFIGURATION_INSTRUCTIONS, /Never infer prices, measured quality, quota savings or a guaranteed cost optimum/);
  assert.match(CONFIGURATION_INSTRUCTIONS, /invent no thresholds.*Never bypass the caller's evaluated quality\/cost policy/);
});

const pricedInstructions = models => rubric.buildConfigurationInstructions(models.map(model => ({ model, effort: 'high', description: 'native' })),
  { now: Date.parse('2026-10-18T12:00:00Z') });
const priceReference = text => JSON.parse(text.split('\nAPI_PRICE_REFERENCE=')[1]);

test('reference prices are exact-ID, offered-only, deduplicated and never billing estimates', () => {
  const instructions = pricedInstructions(['gpt-6-luna', 'gpt-6-luna', 'gpt-6.1-sol', 'gpt-6-astra', 'unknown-native']);
  assert.ok(instructions.startsWith(CONFIGURATION_INSTRUCTIONS));
  assert.match(instructions, /quality and native requirements first/i);
  assert.match(instructions, /subscription quota.*actual native bill/i);
  assert.match(instructions, /full native prompt.*unknown/i);
  assert.match(instructions, /effort.*not a fixed price multiplier/i);
  const ref = priceReference(instructions);
  assert.equal(ref.unit, 'USD per 1000000 tokens');
  assert.equal(ref.verifiedAt, '2026-10-18');
  assert.deepEqual(Object.keys(ref.models), ['gpt-6-luna', 'gpt-6.1-sol', 'gpt-6-astra']);
  assert.deepEqual(ref.unknownModels, ['unknown-native']);
  assert.deepEqual(ref.models['gpt-6-luna'].tiers, [
    { promptTokensMax: 272000, input: 0.1, output: 0.5, cacheRead: 0.01, cacheWrite: 0.125 },
    { promptTokensMin: 272001, input: 0.2, output: 0.75, cacheRead: 0.02, cacheWrite: 0.25 },
  ]);
  assert.equal(ref.models['gpt-6.1-sol'].tiers[0].cacheRead, 0.1);
  assert.equal(ref.models['gpt-6-astra'].tiers[0].input, 10);
  assert.equal(ref.models['gpt-6-astra'].tiers[0].output, 50);
  assert.match(ref.models['gpt-6-luna'].source, /^https:\/\/developers\.openai\.com\//);
});

test('removed Claude integration contributes no model pricing hints', () => {
  for (const model of ['claude-haiku-5-5', 'claude-sonnet-5-5', 'claude-opus-5-5', 'claude-mythos-5-1']) {
    assert.equal(pricedInstructions([model]), CONFIGURATION_INSTRUCTIONS);
  }
});

test('reference freshness fails closed without changing the fallback or guessing aliases', () => {
  const configurations = [{ model: 'gpt-6-luna', effort: 'high', description: 'native' }];
  const before = structuredClone(configurations);
  for (const now of [Date.parse('2026-10-17T23:59:59Z'), Date.parse('2026-11-18T00:00:00Z'), NaN, Infinity, '2026-10-18']) {
    assert.equal(rubric.buildConfigurationInstructions(configurations, { now }), CONFIGURATION_INSTRUCTIONS);
  }
  assert.match(rubric.buildConfigurationInstructions(configurations, { now: Date.parse('2026-11-17T23:59:59Z') }), /API_PRICE_REFERENCE=/);
  assert.deepEqual(configurations, before);
  for (const models of [[], ['gpt-6-luna-future'], ['anthropic/claude-opus-5.5'], ['claude-fable-5-1']]) {
    assert.equal(pricedInstructions(models), CONFIGURATION_INSTRUCTIONS);
  }
});

test('complete reviewed price context is bounded and has no token-use or expected-dollar claims', () => {
  const instructions = pricedInstructions(['gpt-6-luna', 'gpt-6.1-sol', 'gpt-6-astra']);
  const reference = priceReference(instructions);
  assert.ok(Buffer.byteLength(instructions) - Buffer.byteLength(CONFIGURATION_INSTRUCTIONS) < 5000);
  assert.equal(Object.hasOwn(reference, 'estimatedCostUsd'), false);
  assert.equal(Object.hasOwn(reference, 'inputTokens'), false);
  assert.match(instructions, /missing or expired prices are unknown/i);
});

test('only bounded current text and allowlisted baseline enter a snapshot, never history or metadata', () => {
  const options = { input: [{ type: 'text', text: 'current request', metadata: 'private-input-canary' }],
    baseline: { ...baseline, provider: 'private-provider-canary', account: 'private-account-canary' }, contextKnown: true };
  Object.defineProperty(options, 'history', { get() { throw new Error('history must not be read'); } });
  assert.deepEqual(snapshot(options), { schemaVersion: 1, currentInput: 'current request', recent: [], baseline,
    contextKnown: false, truncated: false, attachments: false, rubricVersion: RUBRIC_VERSION, previousDecision: null });
});

test('unknown history differs from cut input and omitted attachments', () => {
  const knownInput = snapshot({ input: 'A self-contained request' });
  assert.equal(knownInput.contextKnown, false);
  assert.equal(knownInput.truncated, false);
  assert.equal(knownInput.attachments, false);
  const attached = snapshot({ input: [{ type: 'text', text: 'explain' }, { type: 'image', url: 'private-image-canary' },
    { type: 'text', text: 'this' }] });
  assert.equal(attached.currentInput, 'explain\nthis');
  assert.equal(attached.attachments, true);
  assert.equal(attached.truncated, false);
  assert.ok(!JSON.stringify(attached).includes('canary'));
  assert.equal(snapshot({ input: {} }).attachments, true);
  assert.equal(snapshot({ input: null }).attachments, false);
  assert.equal(snapshot({ input: 'x'.repeat(10_000) }).truncated, true);
});

test('prior judgment is a fresh exact allowlisted object; arbitrary extra metadata is never inspected or copied', () => {
  const supplied = { ...prior, transcript: ['private-transcript-canary'], account: 'private-account-canary',
    taskCompleted: true, confidence: 1, probabilities: { unknown: 1 }, rubricVersion: 'wrong', schemaVersion: 999 };
  supplied.circular = supplied;
  Object.defineProperty(supplied, 'toJSON', { get() { throw new Error('metadata must not be serialized'); } });
  const state = snapshot({ previousDecision: supplied });
  assert.deepEqual(state.previousDecision, prior);
  assert.notEqual(state.previousDecision, supplied);
  assert.ok(!JSON.stringify(state).includes('canary'));
  assert.equal(state.rubricVersion, RUBRIC_VERSION);
  state.previousDecision.model = 'changed';
  assert.equal(supplied.model, 'native');
});

test('every supported capability/outcome/effort and probability endpoints survive without thresholding', () => {
  for (const capability of ['bounded_routine', 'multistep_coordination', 'open_problem', 'unknown']) {
    for (const turnOutcome of ['completed', 'interrupted', 'failed', 'unknown']) {
      for (const effort of [null, 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra']) {
        for (const probability of [0, Number.MIN_VALUE, 0.49, 0.5, 0.51, 1]) {
          const previousDecision = { ...prior, model: 'caller-checks/eligibility:1.0', effort, capability,
            contextSufficient: probability, highConsequence: probability, turnOutcome };
          assert.deepEqual(snapshot({ previousDecision }).previousDecision, previousDecision);
        }
      }
    }
  }
});

test('unknown, incomplete, inherited or malformed priors become null rather than invented defaults', () => {
  const invalid = [undefined, null, 'unknown', 'completed', 1, [], {}, new Date(), Object.create(prior),
    new Proxy({}, { getOwnPropertyDescriptor() { throw new Error('private-error-canary'); } })];
  for (const key of Object.keys(prior)) {
    const missing = { ...prior }; delete missing[key]; invalid.push(missing);
  }
  for (const model of ['', 'x'.repeat(129), 'native\n', 'native\r', 'native model', '\uD800', {}, 1]) invalid.push({ ...prior, model });
  for (const effort of [undefined, '', 'invented', 'HIGH', 'high\n', 'x'.repeat(33), {}, 1]) invalid.push({ ...prior, effort });
  for (const capability of [undefined, 'routine', 1, {}, ['open_problem']]) invalid.push({ ...prior, capability });
  for (const turnOutcome of [undefined, 'running', 'success', 1, {}]) invalid.push({ ...prior, turnOutcome });
  for (const key of ['contextSufficient', 'highConsequence']) {
    for (const value of [undefined, null, false, '0.5', NaN, Infinity, -Infinity, -0.01, 1.01, {}]) invalid.push({ ...prior, [key]: value });
  }
  for (const previousDecision of invalid) assert.equal(snapshot({ previousDecision }).previousDecision, null);
  const throwing = { ...prior };
  Object.defineProperty(throwing, 'model', { get() { throw new Error('private-error-canary'); } });
  assert.equal(snapshot({ previousDecision: throwing }).previousDecision, null);
  assert.equal(snapshot({ previousDecision: { ...prior, model: 'x'.repeat(128) } }).previousDecision.model.length, 128);
});

test('full UTF-8 JSON including escaping, version and valid prior fits every tested byte boundary', () => {
  const input = '한글🐈\n"\\\u0000\b\t\r🙂e\u0301'.repeat(50);
  for (const previousDecision of [null, { ...prior, model: 'x'.repeat(128), contextSufficient: Number.MIN_VALUE }]) {
    const full = snapshot({ input, previousDecision, maxBytes: 20_000 });
    assert.equal(full.currentInput, input);
    assert.equal(full.truncated, false);
    const minimum = bytes({ ...full, currentInput: '', truncated: true });
    for (let maxBytes = minimum; maxBytes <= bytes(full) + 1; maxBytes++) {
      const result = snapshot({ input, previousDecision, maxBytes });
      assert.ok(bytes(result) <= maxBytes, `serialized snapshot fits ${maxBytes} bytes`);
      assert.ok(input.startsWith(result.currentInput));
      assert.equal(Buffer.from(result.currentInput).toString('utf8'), result.currentInput);
      assert.equal(result.truncated, result.currentInput !== input);
      assert.deepEqual(result.previousDecision, previousDecision);
      assert.deepEqual(result.recent, []);
      assert.equal(result.contextKnown, false);
    }
    assert.throws(() => snapshot({ input, previousDecision, maxBytes: minimum - 1 }), error('SNAPSHOT_LIMIT_TOO_SMALL'));
  }
});

test('a malformed surrogate ends the valid prefix, while valid pairs remain intact', () => {
  for (const [input, prefix] of [['a🐈🙂z', 'a🐈🙂z'], ['\uD800x', ''], ['\uDC00x', ''],
    ['a\uD800x', 'a'], ['a\uDC00x', 'a'], ['a🐈\uDC00tail', 'a🐈'], ['a🐈\uD800tail', 'a🐈']]) {
    const result = snapshot({ input });
    assert.equal(result.currentInput, prefix);
    assert.equal(result.truncated, input !== prefix);
    assert.equal(Buffer.from(prefix).toString('utf8'), prefix);
  }
});

test('default whole-state limit is 8192 and invalid limits/baselines retain policy errors', () => {
  const result = snapshot({ input: 'x'.repeat(100_000), previousDecision: prior });
  assert.equal(bytes(result), 8192);
  assert.equal(result.truncated, true);
  assert.deepEqual(result.previousDecision, prior);
  for (const maxBytes of [-1, NaN, Infinity, 1.5, '8192']) assert.throws(() => snapshot({ maxBytes }), error('INVALID_SNAPSHOT_LIMIT'));
  assert.throws(() => snapshot({ maxBytes: 0 }), error('SNAPSHOT_LIMIT_TOO_SMALL'));
  assert.throws(() => snapshot({ baseline: null }), error('INVALID_BASELINE'));
});
