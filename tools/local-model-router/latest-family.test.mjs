import test from 'node:test';
import assert from 'node:assert/strict';
import { setImmediate as tick } from 'node:timers/promises';
import { JevService, loginFingerprint } from './service.mjs';
import { ModelRouter } from './router.mjs';

const NOW = Date.parse('2026-10-18T12:00:00Z');
const FRONTIER = 'gpt-6-astra', STANDARD = 'gpt-6.1-sol', LIGHT = 'gpt-6-luna', LEGACY = 'gpt-5.6-sol';
// RED before implementation: 23 tests, 16 pass / 7 fail, including legacy c0,
// null effort, consent passthrough and outbound model/collaboration parameters.
// Injected policy also verifies the constructor owns its immutable snapshot.
const registry = () => ({ schemaVersion: 1, version: 'codex-native-0.162.1-2026-10-18', provider: 'codex',
  reviewedAt: '2026-10-17T15:00:00Z', reviewDeadline: '2026-11-18T00:00:00Z',
  provenance: { source: 'native-private-cli-catalog', cliVersion: '0.162.1', observedOn: '2026-10-18',
    basis: 'Runtime model/list descriptions; no explicit generation metadata.',
    limitation: 'Runtime evidence, not a public release guarantee. Manual review required by reviewDeadline.' },
  current: { frontier: FRONTIER, standard: STANDARD, light: LIGHT },
  legacy: ['gpt-6-sol', LEGACY, 'gpt-5.6-terra', 'gpt-5.6-luna'] });
const row = (model, efforts = ['low', 'high', 'max', 'ultra']) => ({ model, inputModalities: ['text', 'image'],
  description: `Native description for ${model}`, defaultReasoningEffort: 'high',
  supportedReasoningEfforts: efforts.map(reasoningEffort => ({ reasoningEffort, description: `Native ${reasoningEffort}` })) });
const catalog = () => [row(LEGACY), row(LIGHT), row('gpt-99-unreviewed'), row(STANDARD), row(FRONTIER)];
const actual = (model = LEGACY, effort = 'max') => ({ model, effort });
const request = (baseline = actual(), input = [{ type: 'text', text: 'Apply the specified line correction.' }]) => ({
  id: 2, method: 'turn/start', params: { threadId: 'main', ...baseline, input,
    cwd: '/fixture-workspace', approvalPolicy: 'on-request', sandboxPolicy: { type: 'readOnly' } } });
const judgment = (configurations, index = 0, capability = 'bounded_routine') => ({ capability, confidence: 0.97,
  probabilities: Object.fromEntries(['bounded_routine', 'multistep_coordination', 'open_problem', 'unknown'].map(key => [key, key === capability ? 0.97 : 0.01])),
  contextSufficient: 0.99, highConsequence: 0.01,
  ...(configurations ? { configuration: { ...configurations[index], choice: `c${index}` } } : {}) });
function harness(options = {}) {
  let calls = 0, configurations, state;
  const sent = [], received = [];
  const service = new JevService({ catalog: catalog(), account: { type: 'chatgpt', email: 'fixture@example.invalid' },
    accountHome: '/fixture-account', apiKey: 'fixture-key', externalPromptTransfer: true,
    latestFamily: registry(), now: () => NOW, policyReader: () => null, judgmentLimits: { cooldownMs: 0 },
    judgeFactory: settings => async snapshot => { calls++; configurations = settings.configurations; state = snapshot;
      return judgment(configurations, 0); }, ...options });
  const router = new ModelRouter({ catalog: service.catalog, service, toServer: message => sent.push(message), toClient: message => received.push(message) });
  router.client({ id: 1, method: 'thread/start', params: { sessionStartSource: 'startup' } });
  router.server({ id: 1, result: { thread: { id: 'main', source: 'cli' }, model: LEGACY, reasoningEffort: 'max' } });
  return { service, router, sent, received, calls: () => calls, configurations: () => configurations, state: () => state };
}

test('recognized legacy uses the same-effort current frontier c0 while state keeps the actual baseline', async t => {
  const f = harness(); t.after(() => f.router.close());
  const selected = await f.service.decide(request());
  assert.equal(selected.model, FRONTIER); assert.equal(selected.effort, 'max'); assert.equal(selected.changed, true);
  assert.deepEqual(f.state().baseline, actual());
  assert.ok(f.configurations().every(value => [FRONTIER, STANDARD, LIGHT].includes(value.model)));
  assert.ok(f.configurations().every(value => Object.keys(value).join(',') === 'model,effort,description'));
});

test('a native-eligible unknown baseline is not silently migrated', async t => {
  const f = harness(); t.after(() => f.router.close());
  await assert.rejects(f.service.decide(request(actual('gpt-99-unreviewed'))), { code: 'registry-review-required' });
  assert.equal(f.calls(), 0);
});

test('legacy migration fails without its eligible frontier or exact supported effort', async t => {
  for (const [rows, code] of [[catalog().filter(value => value.model !== FRONTIER), 'latest-family-anchor-unavailable'],
    [catalog().map(value => value.model === FRONTIER ? row(FRONTIER, ['high']) : value), 'latest-family-effort-unsupported']]) {
    const f = harness({ catalog: rows }); t.after(() => f.router.close());
    await assert.rejects(f.service.decide(request()), { code }); assert.equal(f.calls(), 0);
  }
});

test('explicit null effort is preserved in top-level and collaboration settings, never replaced with thread max', async t => {
  for (const collaboration of [false, true]) {
    const f = harness(); t.after(() => f.router.close());
    const incoming = request();
    if (collaboration) incoming.params.collaborationMode = { mode: 'default', settings: { model: LEGACY, reasoning_effort: null } };
    else incoming.params.effort = null;
    await assert.rejects(f.service.decide(incoming), { code: 'baseline-ineligible' }); assert.equal(f.calls(), 0);
  }
  const rows = catalog(); rows[0] = { ...row(LEGACY, []), defaultReasoningEffort: null };
  const f = harness({ catalog: rows }); t.after(() => f.router.close());
  await assert.rejects(f.service.decide(request(actual(LEGACY, null))), { code: 'latest-family-effort-unsupported' });
  rows[rows.findIndex(value => value.model === FRONTIER)] = { ...row(FRONTIER, []), defaultReasoningEffort: null };
  const g = harness({ catalog: rows }); t.after(() => g.router.close());
  const selected = await g.service.decide(request(actual(LEGACY, null)));
  assert.equal(selected.model, FRONTIER); assert.equal(selected.effort, null);
});

test('owned latest-family consent veto and missing consent block instead of passing legacy through', async t => {
  for (const options of [{ policyReader: () => ({ policy: { consent: { externalPromptTransfer: false } } }) },
    { externalPromptTransfer: false }]) {
    const f = harness(options); t.after(() => f.router.close());
    await assert.rejects(f.service.decide(request()), { code: 'external-transfer-policy-unset' }); assert.equal(f.calls(), 0);
    f.router.client(request()); await tick();
    assert.equal(f.sent.some(value => value.method === 'turn/start'), false);
  }
});

test('outbound model and collaboration parameters use the current anchor without changing input or native controls', async t => {
  const f = harness(); t.after(() => f.router.close());
  const incoming = request();
  incoming.params.collaborationMode = { mode: 'default', settings: { model: LEGACY, reasoning_effort: 'max', developer_instructions: 'retain' } };
  const before = structuredClone(incoming);
  f.router.client(incoming); await tick();
  const expected = structuredClone(before);
  expected.params.model = FRONTIER; expected.params.collaborationMode.settings.model = FRONTIER;
  assert.deepEqual(f.sent.at(-1), expected); assert.deepEqual(incoming, before);
});

test('shuffled catalogs and all recognized legacy IDs produce only the reviewed current menu', async t => {
  for (const model of registry().legacy) {
    const rows = catalog().filter(value => value.model !== LEGACY); rows.unshift(row(model)); rows.reverse();
    const f = harness({ catalog: rows }); t.after(() => f.router.close());
    assert.equal((await f.service.decide(request(actual(model)))).model, FRONTIER);
    assert.deepEqual([...new Set(f.configurations().map(value => value.model))].sort(), [FRONTIER, STANDARD, LIGHT].sort());
  }
});

test('current baselines retain exact c0 even if the frontier is absent; clear tasks can choose current light', async t => {
  for (const model of [FRONTIER, STANDARD, LIGHT]) {
    const f = harness({ catalog: catalog().filter(value => model === FRONTIER || value.model !== FRONTIER) }); t.after(() => f.router.close());
    const selected = await f.service.decide(request(actual(model, 'high')));
    assert.equal(selected.model, model); assert.equal(selected.effort, 'high'); assert.equal(selected.changed, false);
  }
  let state;
  const f = harness({ judgeFactory: ({ configurations }) => async snapshot => { state = snapshot;
    return judgment(configurations, configurations.findIndex(value => value.model === LIGHT && value.effort === 'low')); } });
  t.after(() => f.router.close());
  assert.equal((await f.service.decide(request())).model, LIGHT); assert.deepEqual(state.baseline, actual());
});

test('legacy unknown, attached and truncated judgments fall back to current c0 and compare against the actual baseline', async t => {
  for (const [capability, input] of [['unknown', [{ type: 'text', text: 'continue that' }]],
    ['bounded_routine', [{ type: 'image', url: 'fixture-image' }]],
    ['bounded_routine', [{ type: 'text', text: 'x'.repeat(20_000) }]]]) {
    const f = harness({ judgeFactory: ({ configurations }) => async () => judgment(configurations,
      configurations.findIndex(value => value.model === LIGHT && value.effort === 'low'), capability) });
    t.after(() => f.router.close());
    const selected = await f.service.decide(request(actual(), input));
    assert.equal(selected.model, FRONTIER); assert.equal(selected.effort, 'max'); assert.equal(selected.changed, true);
  }
});

test('full native eligibility rejects a denied, expired, duplicate or unsupported actual baseline before migration', async t => {
  for (const mutate of [rows => { rows[0].accessDenied = true; }, rows => { rows[0].hidden = true; },
    rows => { rows[0].expiresAt = NOW; }, rows => { rows[0].accountFingerprint = 'foreign'; },
    rows => { rows[0].inputModalities = []; }, rows => { rows[0].supportedReasoningEfforts = []; },
    rows => rows.push({ ...rows[0] }), rows => rows.shift()]) {
    const rows = catalog(); mutate(rows);
    const f = harness({ catalog: rows }); t.after(() => f.router.close());
    await assert.rejects(f.service.decide(request()), { code: 'baseline-ineligible' }); assert.equal(f.calls(), 0);
  }
});

test('ultra migration offers only current anchor ultra; non-ultra never offers delegation', async t => {
  for (const model of [LEGACY, STANDARD]) {
    const f = harness(); t.after(() => f.router.close());
    const selected = await f.service.decide(request(actual(model, 'ultra')));
    assert.equal(f.configurations().length, 1); assert.equal(selected.effort, 'ultra');
    assert.equal(selected.model, model === LEGACY ? FRONTIER : STANDARD);
  }
  const f = harness(); t.after(() => f.router.close()); await f.service.decide(request());
  assert.ok(f.configurations().every(value => value.effort !== 'ultra'));
});

test('registry deadline and new native default or upgrade metadata block before judgment', async t => {
  for (const options of [{ now: () => Date.parse(registry().reviewDeadline) },
    { catalog: [...catalog(), { ...row('gpt-99-new'), isDefault: true }] },
    { catalog: [...catalog(), { ...row('gpt-99-new'), upgrade: 'gpt-99-new' }] }]) {
    const f = harness(options); t.after(() => f.router.close());
    await assert.rejects(f.service.decide(request()), { code: 'registry-review-required' }); assert.equal(f.calls(), 0);
  }
});

function empirical(models) {
  const accountFingerprint = loginFingerprint({ type: 'chatgpt', email: 'fixture@example.invalid' }, '/fixture-account');
  const times = { measuredAt: NOW - 1, expiresAt: NOW + 60_000 }, tasks = ['bounded_routine', 'multistep_coordination', 'open_problem'];
  return { digest: 'fixture-policy', policy: { version: 'fixture-policy', profiles: models.map((model, index) => ({
    schemaVersion: 1, version: 'fixture', provider: 'codex', accountFingerprint, model, effort: 'max', allowed: true,
    availability: { status: 'available', source: 'synthetic-only', verifiedAt: NOW - 1, expiresAt: times.expiresAt },
    capabilities: { inputModalities: ['text', 'image'], tools: [], contextWindow: 16_000 },
    billing: { regime: 'subscription', pool: 'fixture' },
    quality: { version: 'fixture-quality', evidence: 'synthetic-only', evaluatedAt: NOW - 1, expiresAt: times.expiresAt,
      sampleSize: 100, tasks: Object.fromEntries(tasks.map(task => [task, { lowerBound: 0.99, sampleSize: 100 }])) },
    cost: { regime: 'subscription', unit: 'relative-quota', pool: 'fixture', accountFingerprint,
      source: 'synthetic-only', comparisonKey: 'fixture', ...times, scope: 'all-in-turn',
      totals: Object.fromEntries(tasks.map(task => [task, index === 0 ? 10 : 1])) },
  })), thresholds: { schemaVersion: 1, version: 'fixture', evidence: 'synthetic-only', qualityVersion: 'fixture-quality',
    evaluatedAt: NOW - 1, expiresAt: times.expiresAt, sampleSize: 100, minSamples: 20,
    groups: ['ko', 'en', 'short-followup', 'ordinary', 'high-consequence'], capabilityConfidence: 0.8,
    contextSufficient: 0.8, highConsequence: 0.2, qualityFloor: 0.9, highRiskQualityFloor: 0.97, nonInferiorityMargin: 0.01 } } };
}

test('empirical policy rejects legacy migration and cannot select a cheaper legacy, unknown or native-ineligible model', async t => {
  const loaded = empirical([FRONTIER, LEGACY, 'gpt-99-unreviewed', LIGHT]);
  const f = harness({ policyReader: () => loaded, catalog: catalog().map(value => value.model === LIGHT ? { ...value, expiresAt: NOW } : value),
    requirementsForTurn: () => ({ provider: 'codex', inputModalities: ['text'], tools: [], contextTokens: 4000 }) });
  t.after(() => f.router.close());
  await assert.rejects(f.service.decide(request()), { code: 'latest-family-policy-conflict' }); assert.equal(f.calls(), 0);
  const selected = await f.service.decide(request(actual(FRONTIER)));
  assert.equal(selected.model, FRONTIER); assert.equal(selected.changed, false);
});

test('late catalog removal and deadline expiry invalidate held choices and empirical fallbacks', async t => {
  for (const policy of [false, true]) {
    let now = NOW, resolve;
    const f = harness({ now: () => now, ...(policy ? { policyReader: () => empirical([FRONTIER, LIGHT]) } : {}),
      judgeFactory: ({ configurations }) => () => new Promise(yes => { resolve = () => yes(judgment(configurations)); }) });
    t.after(() => f.router.close());
    const pending = f.service.decide(request(actual(FRONTIER))); await tick();
    f.service.observe({ method: 'model/list', params: {} }, { result: { data: catalog().filter(value => value.model !== FRONTIER), nextCursor: null } });
    resolve(); await assert.rejects(pending, { code: 'stale-decision' });
    const g = harness({ now: () => now, ...(policy ? { policyReader: () => empirical([FRONTIER]) } : {}) }); t.after(() => g.router.close());
    const selected = await g.service.decide(request(actual(FRONTIER)));
    now = Date.parse(registry().reviewDeadline); assert.equal(selected.isCurrent(), false);
  }
});

test('registry snapshot and version/digest bind both the decision and prior judgment', async t => {
  const supplied = registry(), f = harness({ latestFamily: supplied }); t.after(() => f.router.close());
  supplied.current.frontier = 'gpt-99-mutated'; supplied.version = 'mutated';
  const incoming = request(), selected = await f.service.decide(incoming);
  assert.equal(selected.model, FRONTIER); assert.equal(f.service.latestFamily.version, registry().version);
  assert.ok(Object.isFrozen(f.service.latestFamily.current));
  const thread = f.service.threads.get('main'), binding = thread.pendingDecision.binding;
  assert.equal(binding.registryVersion, registry().version); assert.match(binding.registryDigest, /^[a-f0-9]{64}$/);
  assert.ok(JSON.parse(selected.owner).includes(binding.registryVersion));
  assert.ok(JSON.parse(selected.owner).includes(binding.registryDigest));
  assert.ok(Object.isFrozen(binding)); assert.equal(f.state().currentFamily.digest, binding.registryDigest);
  f.service.observe({ ...incoming, params: { ...incoming.params, model: selected.model, effort: selected.effort } }, { result: { turn: { id: 'turn-1' } } });
  await f.service.decide(request(actual(FRONTIER)));
  assert.equal(f.state().previousDecision.model, FRONTIER);
  thread.previousDecision = { ...thread.previousDecision, binding: { ...binding, registryDigest: '0'.repeat(64) } };
  await f.service.decide(request(actual(FRONTIER))); assert.equal(f.state().previousDecision, null);
});

test('Jev failure still submits nothing; null registry retains standalone consent passthrough', async t => {
  const f = harness({ judgeFactory: () => async () => { throw Object.assign(new Error('fixture'), { code: 'timeout' }); } });
  t.after(() => f.router.close()); f.router.client(request()); await tick();
  assert.equal(f.sent.some(value => value.method === 'turn/start'), false); assert.ok(f.received.at(-1).error);
  const g = harness({ latestFamily: null, externalPromptTransfer: false }); t.after(() => g.router.close());
  assert.equal((await g.service.decide(request())).passthrough, true);
});
