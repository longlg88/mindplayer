import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, chmodSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setImmediate as tick } from 'node:timers/promises';
import { childEnvironment, loadPolicy, loadJevCredentials, provisionJevCredentials, loginFingerprint, JevService } from './service.mjs';
import { ModelRouter } from './router.mjs';
import { createJevClient } from './jev.mjs';
import { createServer } from 'node:http';

const catalog = [{ model: 'native', hidden: false, inputModalities: ['text'], supportedReasoningEfforts: [{ reasoningEffort: 'max' }] }];
const account = { type: 'chatgpt', email: 'one@example.invalid' };
const native = (id = 2, threadId = 'main') => ({ id, method: 'turn/start', params: { threadId, model: 'native', effort: 'max', input: [{ type: 'text', text: '그거 마저 해줘 / private-canary' }], cwd: '/same-cwd', approvalPolicy: 'never' } });
function harness(options = {}) {
  let calls = 0;
  const audit = [], sent = [], received = [];
  const service = new JevService({ catalog, accountHome: '/account-one', account, expectedThreadId: 'main', apiKey: 'gateway-secret-canary', policyReader: () => null,
    externalPromptTransfer: true, judgmentLimits: { cooldownMs: 0 },
    requirementsForTurn: () => ({ provider: 'codex', inputModalities: ['text'], tools: [], contextTokens: 4000 }), // synthetic fixture evidence only
    judgeFactory: ({ configurations }) => async () => { calls++; return withConfiguration(judgment('bounded_routine'), configurations); }, audit: e => audit.push(e), ...options });
  const router = new ModelRouter({ catalog: options.catalog ?? catalog, service, audit: e => audit.push(e), toServer: e => sent.push(e), toClient: e => received.push(e) });
  const lifecycle = (method = 'thread/start', ephemeral = false, source = 'cli') => {
    router.client({ id: 1, method, params: { threadId: 'main', ephemeral, sessionStartSource: 'startup' } });
    router.server({ id: 1, result: { thread: { id: 'main', source, ephemeral }, model: 'native', reasoningEffort: 'max' } });
  };
  return { router, service, audit, sent, received, lifecycle, calls: () => calls };
}

test('managed native message metadata is required on every request, including repeated session modes', async () => {
  const f = harness({ requirePromptChoice: true }); f.lifecycle();
  for (const [index, mode] of ['native', 'native', 'jev', 'jev', 'native', 'jev', null].entries()) {
    const request = native(index + 10);
    if (mode) request.params.mindplayerRoutingChoice = { mode, includeRecent: false };
    f.router.client(request); await tick();
    const forwarded = f.sent.find(row => row.id === request.id && row.method === 'turn/start');
    assert.equal(!!forwarded, mode !== null);
    if (forwarded) {
      assert.equal(Object.hasOwn(forwarded.params, 'mindplayerRoutingChoice'), false);
      f.router.server({ id: request.id, result: { turn: { id: `choice-${index}`, status: 'inProgress' } } });
      f.router.server({ method: 'turn/completed', params: { threadId: 'main', turn: { id: `choice-${index}`, status: 'completed' } } });
    }
  }
  assert.equal(f.calls(), 3);
  f.router.close();
});

test('explicit native prompt bypasses key/policy/judge and preserves exact native configuration', async () => {
  const f = harness({ requirePromptChoice: true, apiKey: undefined,
    policyReader: () => { throw new Error('native must not load Jev policy'); } });
  f.lifecycle();
  const request = native();
  request.params.mindplayerRoutingChoice = { mode: 'native', includeRecent: false };
  f.router.client(request); await tick();
  const expected = structuredClone(request); delete expected.params.mindplayerRoutingChoice;
  assert.deepEqual(f.sent.at(-1), expected);
  assert.equal(f.calls(), 0);
  f.router.close();
});

test('OFF resume ignores malformed Jev policy; a later ON turn fails closed without submission', async t => {
  const dir = mkdtempSync(join(tmpdir(), 'mindplayer-off-resume-'));
  const path = join(dir, 'policy.json');
  writeFileSync(path, '{invalid', { mode: 0o600 });
  let policyReads = 0;
  const f = harness({ requirePromptChoice: true,
    policyReader: () => { policyReads++; return loadPolicy(path); } });
  t.after(() => { f.router.close(); rmSync(dir, { recursive: true, force: true }); });
  assert.doesNotThrow(() => f.lifecycle('thread/resume'));
  assert.equal(policyReads, 0);
  assert.ok(f.received.some(row => row.id === 1 && row.result?.thread?.id === 'main'));
  const off = native(10);
  off.params.mindplayerRoutingChoice = { mode: 'native', includeRecent: false };
  f.router.client(off); await tick();
  const expected = structuredClone(off); delete expected.params.mindplayerRoutingChoice;
  assert.deepEqual(f.sent.at(-1), expected);
  assert.equal(policyReads, 0);
  assert.equal(f.calls(), 0);
  assert.equal(f.sent.some(row => row.method === 'thread/turns/list'), false);
  f.router.server({ id: 10, result: { turn: { id: 'off-turn' } } });
  f.router.server({ method: 'turn/completed', params: { threadId: 'main', turn: { id: 'off-turn', status: 'completed' } } });
  const on = native(11);
  on.params.mindplayerRoutingChoice = { mode: 'jev', includeRecent: false };
  f.router.client(on); await tick();
  assert.equal(policyReads, 1);
  assert.equal(f.calls(), 0);
  assert.equal(f.sent.some(row => row.id === 11 && row.method === 'turn/start'), false);
  assert.ok(f.received.some(row => row.id === 11 && row.error));
  assert.ok(f.audit.some(row => row.event === 'route-failed' && row.reason === 'selector-error'));
});

test('retired preview RPC is rejected locally without preparing context, judging or forwarding', async t => {
  let policyReads = 0;
  const f = harness({ requirePromptChoice: true, policyReader: () => { policyReads++; return null; } });
  t.after(() => f.router.close()); f.lifecycle();
  const request = native(20);
  const sent = structuredClone(f.sent);
  f.router.client({ id: 19, method: 'mindplayer/prompt/preview', params: request.params }); await tick();
  assert.deepEqual(f.received.find(row => row.id === 19), {
    id: 19, error: { code: -32601, message: 'Unsupported local method; nothing was sent' },
  });
  assert.deepEqual(f.sent, sent);
  assert.equal(policyReads, 0);
  assert.equal(f.calls(), 0);
  assert.equal(f.audit.some(row => row.event.startsWith('jev-')), false);
  assert.equal(f.router.seen.has('number:19'), false);
});

test('malformed prompt choices cannot authorize judgment or a native fallback', async () => {
  for (const choice of [undefined, null, {}, 'native', { mode: 'native' },
    { mode: 'jev', includeRecent: true }, { mode: 'native', includeRecent: false, inherited: true }]) {
    const f = harness({ requirePromptChoice: true }); f.lifecycle();
    const request = native(); request.params.mindplayerRoutingChoice = choice;
    f.router.client(request); await tick();
    assert.equal(f.calls(), 0);
    assert.equal(f.sent.some(row => row.method === 'turn/start'), false);
    assert.match(f.received.at(-1).error.message, /라우팅 정보/);
    f.router.close();
  }
});

test('judgment snapshots and service state contain no preview fields or storage', async t => {
  const snapshots = [];
  const f = harness({ requirePromptChoice: true, judgeFactory: ({ configurations }) => async snapshot => {
    snapshots.push(structuredClone(snapshot)); return withConfiguration(judgment('bounded_routine'), configurations);
  } }); t.after(() => f.router.close()); f.lifecycle();
  const request = native(30);
  request.params.mindplayerRoutingChoice = { mode: 'jev', includeRecent: false };
  const decision = await f.service.decide(request);
  assert.equal(snapshots.length, 1);
  assert.deepEqual(Object.keys(snapshots[0]).sort(), [
    'schemaVersion', 'currentInput', 'recent', 'baseline', 'contextKnown', 'truncated', 'attachments', 'rubricVersion', 'previousDecision',
  ].sort());
  assert.equal(snapshots[0].currentInput, request.params.input[0].text);
  assert.deepEqual(snapshots[0].baseline, { model: 'native', effort: 'max' });
  assert.deepEqual(snapshots[0].recent, []);
  assert.equal(snapshots[0].previousDecision, null);
  assert.ok(Buffer.byteLength(JSON.stringify(snapshots[0])) <= 8192);
  assert.equal(Object.hasOwn(decision, 'snapshot'), false);
  assert.equal(Object.hasOwn(decision, 'previewId'), false);
  assert.equal(Object.hasOwn(f.service, 'previews'), false);
  assert.equal(f.service.prunePreviews, undefined);
  f.service.invalidate(); f.service.close();
  assert.equal(Object.hasOwn(f.service, 'previews'), false);
});

test('unexpected preview IDs reject both Jev and native metadata before policy, judgment or submission', async t => {
  for (const mode of ['native', 'jev']) for (const previewId of ['01234567-89ab-cdef-0123-456789abcdef', undefined, null, '', {}]) {
    let policyReads = 0;
    const f = harness({ requirePromptChoice: true, policyReader: () => { policyReads++; return null; } });
    t.after(() => f.router.close()); f.lifecycle();
    const request = native(40);
    request.params.mindplayerRoutingChoice = { mode, includeRecent: false, previewId };
    await assert.rejects(f.service.decide(request), { code: 'prompt-choice-required' });
    f.router.client(request); await tick();
    assert.equal(f.calls(), 0);
    assert.equal(policyReads, 0);
    assert.equal(f.sent.some(row => row.method === 'turn/start'), false);
    assert.match(f.received.at(-1).error.message, /라우팅 정보/);
    assert.equal(f.audit.find(row => row.event === 'route-failed').reason, 'prompt-choice-required');
  }
});

test('retired preview option cannot bypass native per-message metadata validation', async t => {
  const f = harness({ requirePromptChoice: true });
  t.after(() => f.router.close()); f.lifecycle();
  const request = native(50);
  await assert.rejects(f.service.decide(request, { preview: true }), { code: 'prompt-choice-required' });
  assert.equal(f.calls(), 0);
  request.params.mindplayerRoutingChoice = { mode: 'jev', includeRecent: false };
  const decision = await f.service.decide(request, { preview: true });
  assert.equal(f.calls(), 1);
  assert.equal(Object.hasOwn(decision, 'snapshot'), false);
  assert.equal(Object.hasOwn(decision, 'previewId'), false);
  assert.equal(f.sent.some(row => row.method === 'turn/start'), false);
});

test('per-prompt control env is stripped from native backend and tool environment', () => {
  assert.deepEqual(childEnvironment({ PATH: '/same', MINDPLAYER_PER_PROMPT_CHOICE: '1' }), { PATH: '/same' });
});

test('Gateway credentials are removed from every child environment without mutating parent', () => {
  const parent = { AI_GATEWAY_API_KEY: 'canary', TYPESAFE_API_KEY: 'canary', VERCEL_OIDC_TOKEN: 'canary', MINDPLAYER_ROUTER_AVAILABILITY: '{}', MINDPLAYER_ROUTER_STATUS: '/private-status', MINDPLAYER_CLAUDE_JEV_URL: 'http://127.0.0.1:1', MINDPLAYER_CLAUDE_JEV_TOKEN: 'broker-canary', CODEX_HOME: '/same-account', PATH: '/same-path' };
  assert.deepEqual(childEnvironment(parent), { CODEX_HOME: '/same-account', PATH: '/same-path' });
  assert.equal(parent.AI_GATEWAY_API_KEY, 'canary');
});

test('policy file must be private, bounded, regular, versioned JSON, never sourced from prompt', () => {
  const dir = mkdtempSync(join(tmpdir(), 'mindplayer-policy-test-'));
  const path = join(dir, 'policy.json');
  try {
    assert.equal(loadPolicy(path), null);
    writeFileSync(path, JSON.stringify({ schemaVersion: 1, version: 'synthetic', profiles: [] }), { mode: 0o600 });
    assert.equal(loadPolicy(path).policy.version, 'synthetic');
    chmodSync(path, 0o644); assert.throws(() => loadPolicy(path), { code: 'policy-invalid' });
    chmodSync(path, 0o600); writeFileSync(path, 'x'.repeat(129 * 1024)); assert.throws(() => loadPolicy(path), { code: 'policy-invalid' });
    for (const body of ['{invalid', 'null', '{}', JSON.stringify({ schemaVersion: 1, version: ' ', profiles: [] })]) {
      writeFileSync(path, body); assert.throws(() => loadPolicy(path), { code: 'policy-invalid' });
    }
    assert.throws(() => loadPolicy(dir), { code: 'policy-invalid' });
    const link = join(dir, 'link.json'); symlinkSync(path, link);
    assert.throws(() => loadPolicy(link), { code: 'policy-invalid' });
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('credential setup reads environment in process, creates a private file exclusively, and never overwrites it', () => {
  const dir = mkdtempSync(join(tmpdir(), 'mindplayer-key-test-')), path = join(dir, 'key.json');
  try {
    const env = { TYPESAFE_API_KEY: 'typesafe-credential-canary' };
    assert.equal(loadJevCredentials({ env: {}, path }), null);
    assert.deepEqual(provisionJevCredentials({ env, path }), { provider: 'typesafe', stored: true });
    const loaded = loadJevCredentials({ env: {}, path });
    assert.equal(loaded.apiKey, env.TYPESAFE_API_KEY); assert.equal(loaded.provider, 'typesafe');
    assert.equal(loaded.externalPromptTransfer, true);
    assert.throws(() => provisionJevCredentials({ env: { TYPESAFE_API_KEY: 'replacement' }, path }));
    assert.equal(loadJevCredentials({ env: {}, path }).apiKey, env.TYPESAFE_API_KEY);
    chmodSync(path, 0o644); assert.equal(loadJevCredentials({ env: {}, path }), null);
    assert.equal(loadJevCredentials({ env, path }).provider, 'typesafe');
    assert.equal(loadJevCredentials({ env: { AI_GATEWAY_API_KEY: 'gateway-canary', ...env }, path }).provider, 'gateway');
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('invalid keys, provider names, or missing transfer consent are not loaded', () => {
  const dir = mkdtempSync(join(tmpdir(), 'mindplayer-key-test-')), path = join(dir, 'key.json');
  try {
    for (const value of [
      { schemaVersion: 1, provider: 'typesafe', apiKey: 'key' },
      { schemaVersion: 1, provider: 'arbitrary-host', apiKey: 'key', externalPromptTransfer: true, consentVersion: 'jev-per-prompt-v1' },
      { schemaVersion: 1, provider: 'typesafe', apiKey: 'bad\nkey', externalPromptTransfer: true, consentVersion: 'jev-per-prompt-v1' },
    ]) {
      writeFileSync(path, JSON.stringify(value), { mode: 0o600 });
      assert.equal(loadJevCredentials({ env: {}, path }), null);
    }
    assert.equal(loadJevCredentials({ env: { TYPESAFE_API_KEY: 'bad\nkey' }, path }), null);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('login fingerprints distinguish same-workspace people and separate account homes', () => {
  assert.equal(loginFingerprint(account, '/a'), loginFingerprint({ ...account, email: ' ONE@example.invalid ' }, '/a'));
  assert.notEqual(loginFingerprint(account, '/a'), loginFingerprint({ ...account, email: 'two@example.invalid' }, '/a'));
  assert.notEqual(loginFingerprint(account, '/a'), loginFingerprint(account, '/b'));
  assert.equal(loginFingerprint({ type: 'apiKey' }, '/a'), null);
});

test('without a key, owned main new/resume fail before submission with a safe unavailable audit', async () => {
  for (const method of ['thread/start', 'thread/resume']) {
    const f = harness({ apiKey: undefined }); f.lifecycle(method);
    const request = native(); f.router.client(request); await tick();
    assert.equal(f.sent.some(e => e.method === 'turn/start'), false); assert.equal(f.calls(), 0);
    assert.match(f.received.at(-1).error.message, /nothing was sent/);
    assert.ok(f.audit.some(e => e.reason === 'jev-key-unavailable'));
    assert.ok(!JSON.stringify(f.audit).includes('private-canary'));
    f.router.close();
  }
});

test('unknown provenance fails closed; verified ephemeral and subagent helpers never transfer prompts', async () => {
  for (const [known, ephemeral, source] of [[false, false, 'cli'], [true, true, 'cli'], [true, false, 'subAgent'], [true, false, undefined]]) {
    const f = harness();
    if (known) f.lifecycle('thread/start', ephemeral, source ?? 'unknown');
    const request = native(); f.router.client(request); await tick();
    assert.equal(f.calls(), 0);
    const excluded = known && (ephemeral || source === 'subAgent');
    if (excluded) {
      assert.deepEqual(f.sent.at(-1), request);
      assert.ok(f.audit.some(e => e.reason === 'non-user-thread'));
    } else {
      assert.equal(f.sent.some(e => e.method === 'turn/start'), false);
      assert.ok(f.received.some(e => e.id === request.id && e.error));
    }
    f.router.close();
  }
});

test('fresh correctly scoped explicit usage block stops before backend; stale/other home/spend caps do not', async () => {
  for (const [availability, blocked] of [
    [{ source: 'footer-live', codexHome: '/account-one', observedAt: 100_000, blocked: true }, true],
    [{ source: 'footer-live', codexHome: '/other', observedAt: 100_000, blocked: true }, false],
    [{ source: 'footer-live', codexHome: '/account-one', observedAt: 1, blocked: true }, false],
    [{ source: 'footer-live', codexHome: '/account-one', observedAt: 100_000, blocked: false, spend_cap: true }, false],
  ]) {
    const f = harness({ availability, now: () => 100_000 }); f.lifecycle();
    f.router.client(native()); await tick();
    assert.equal(f.sent.filter(e => e.method === 'turn/start').length, blocked ? 0 : 1);
    if (blocked) assert.ok(f.received.at(-1).error);
    f.router.close();
  }
});

test('same-thread history includes only user/final assistant; other threads/tools/reasoning do not enter', () => {
  const f = harness(); f.lifecycle();
  for (const item of [
    { type: 'userMessage', content: [{ type: 'text', text: 'user' }] },
    { type: 'agentMessage', phase: 'final_answer', text: 'final' },
    { type: 'agentMessage', phase: 'commentary', text: 'hidden-canary' },
    { type: 'reasoning', text: 'hidden-canary' },
    { type: 'commandExecution', output: 'hidden-canary' },
  ]) f.router.server({ method: 'item/completed', params: { threadId: 'main', item } });
  f.router.server({ method: 'item/completed', params: { threadId: 'other', item: { type: 'userMessage', content: [{ type: 'text', text: 'other-canary' }] } } });
  assert.deepEqual(f.service.threads.get('main').history, [{ role: 'user', text: 'user' }, { role: 'assistant', text: 'final', final: true }]);
  f.router.close();
});

test('active new turns wait for completion; native steers bypass selection and telemetry is allowlisted', async () => {
  const f = harness(); f.lifecycle();
  f.router.server({ method: 'turn/started', params: { threadId: 'main', turn: { id: 'running' } } });
  const request = native(); f.router.client(request); assert.equal(f.sent.some(e => e.method === 'turn/start'), false);
  const steer = { id: 3, method: 'turn/steer', params: { threadId: 'main', expectedTurnId: 'running', input: [{ type: 'text', text: 'extra' }] } };
  f.router.client(steer); assert.deepEqual(f.sent.at(-1), steer);
  f.router.server({ method: 'model/verification', params: { model: 'native', private: 'private-canary', key: 'gateway-secret-canary' } });
  assert.equal(f.calls(), 0); assert.ok(!JSON.stringify(f.audit).includes('canary'));
  f.router.server({ method: 'turn/completed', params: { threadId: 'main', turn: { id: 'running', status: 'completed' } } });
  await tick(); assert.equal(f.calls(), 1); assert.deepEqual(f.sent.at(-1), request);
  f.router.close();
});

// This is evaluation plumbing with invented model IDs, not production quality/cost evidence.
function deployment(now = 100_000) {
  const fingerprint = loginFingerprint(account, '/account-one');
  const times = { measuredAt: now - 1, expiresAt: now + 60_000 };
  const tasks = ['bounded_routine', 'multistep_coordination', 'open_problem'];
  const thresholds = { schemaVersion: 1, version: 'synthetic', evidence: 'synthetic-only', evaluatedAt: now - 1, expiresAt: times.expiresAt,
    sampleSize: 100, minSamples: 20, groups: ['ko', 'en', 'short-followup', 'ordinary', 'high-consequence'], qualityVersion: 'synthetic',
    capabilityConfidence: 0.8, contextSufficient: 0.8, highConsequence: 0.2, qualityFloor: 0.9, highRiskQualityFloor: 0.97, nonInferiorityMargin: 0.01 };
  const profiles = ['native', 'strong'].map((model, index) => ({
    schemaVersion: 1, version: 'synthetic', provider: 'codex', accountFingerprint: fingerprint, model, effort: 'max', allowed: true,
    availability: { status: 'available', source: 'synthetic-only', verifiedAt: now - 1, expiresAt: times.expiresAt },
    capabilities: { inputModalities: ['text'], tools: [], contextWindow: 16_000 }, billing: { regime: 'subscription', pool: 'synthetic' },
    quality: { version: 'synthetic', evidence: 'synthetic-only', evaluatedAt: now - 1, expiresAt: times.expiresAt, sampleSize: 100,
      tasks: Object.fromEntries(tasks.map(t => [t, { lowerBound: index || t === 'bounded_routine' ? 0.99 : 0.6, sampleSize: 100 }])) },
    cost: { regime: 'subscription', unit: 'relative-quota', accountFingerprint: fingerprint, pool: 'synthetic', comparisonKey: 'synthetic', source: 'synthetic-only',
      ...times, scope: 'all-in-turn', totals: Object.fromEntries(tasks.map(t => [t, index ? 3 : 1])) },
  }));
  return { policy: { schemaVersion: 1, version: 'synthetic', profiles, thresholds, consent: { externalPromptTransfer: true, version: 'synthetic' },
    requirements: { provider: 'codex', inputModalities: ['text'], tools: [], contextTokens: 4000 },
    gateway: { dedicatedKeyBudgetVerified: true, budgetEvidence: 'synthetic-only', timeoutMs: 200, cooldownMs: 1, maxRequestBytes: 32768, maxResponseBytes: 65536 } }, digest: 'synthetic-digest' };
}
const fullCatalog = [...catalog, { ...catalog[0], model: 'strong' }];
function wireJudgment(capability) {
  return { model: 'typesafe-ai/jev', answers: {
    required_capability: { type: 'choice', choice: capability, confidence: 0.97, probabilities: Object.fromEntries(['bounded_routine', 'multistep_coordination', 'open_problem', 'unknown'].map(t => [t, t === capability ? 0.97 : 0.01])) },
    context_sufficient: { type: 'noul', noul: 0.99 }, high_consequence: { type: 'noul', noul: 0.01 },
  }, usage: { input_tokens: 20, output_tokens: 10 } };
}
function judgment(capability) {
  const body = wireJudgment(capability);
  return { capability, confidence: 0.97, probabilities: body.answers.required_capability.probabilities,
    contextSufficient: 0.99, highConsequence: 0.01, usage: { inputTokens: 20, outputTokens: 10 }, gatewayCostUsd: null };
}
function withConfiguration(value, configurations, index = 0) {
  if (!configurations) return value;
  return { ...value, configuration: { model: configurations[index].model, effort: configurations[index].effort, choice: `c${index}`, confidence: 0.9,
    probabilities: Object.fromEntries(configurations.map((_, i) => [`c${i}`, i === index ? 1 : 0])) } };
}

test('every owned prompt calls Jev without a switching-policy file, including rapid consecutive submissions', async () => {
  const f = harness(); f.lifecycle();
  for (let id = 10; id < 13; id++) {
    const request = native(id); f.router.client(request); await tick();
    assert.deepEqual(f.sent.at(-1), request);
    f.router.server({ id, result: { turn: { id: `turn-${id}` } } });
    f.router.server({ method: 'turn/completed', params: { threadId: 'main', turn: { id: `turn-${id}`, status: 'completed' } } });
  }
  assert.equal(f.calls(), 3);
  assert.equal(f.audit.filter(e => e.event === 'jev-judged').length, 3);
  assert.equal(f.sent.filter(e => e.method === 'turn/start').length, 3);
  assert.ok(f.audit.filter(e => e.event === 'jev-judged').every(e => e.threadId === 'main' && Number.isInteger(e.requestId)));
  assert.ok(!JSON.stringify(f.audit).includes('canary'));
  f.router.close();
});

test('identical attempts with a reused RPC ID have distinct judgment ownership', async t => {
  const f = harness(); f.lifecycle(); t.after(() => f.router.close());
  const first = await f.service.decide(native());
  const second = await f.service.decide(native());
  assert.equal(f.calls(), 2);
  assert.notEqual(first.owner, second.owner);
});

test('cancellation at selection publication cannot leave stale decision ownership', async t => {
  const controller = new AbortController();
  const f = harness({ audit: event => { if (event.event === 'model-selected') controller.abort(); } });
  f.lifecycle(); t.after(() => f.router.close());
  await assert.rejects(f.service.decide(native(), { signal: controller.signal }), { code: 'stale-decision' });
  assert.equal(f.service.threads.get('main').pendingDecision, undefined);
  assert.equal(f.sent.filter(row => row.method === 'turn/start').length, 0);
});

test('cancelling a delayed Jev transport allows a fresh identical judgment and discards its late result', async t => {
  const judgments = [];
  const f = harness({ judgeFactory: options => createJevClient({ ...options, fetchImpl: (url, init) => new Promise(resolve => {
    const body = JSON.parse(init.body);
    const wire = wireJudgment('bounded_routine');
    const criteria = body.questions.configuration.criteria;
    wire.answers.configuration = { type: 'choice', choice: 'c0', confidence: 1,
      probabilities: Object.fromEntries(Object.keys(criteria).map(key => [key, key === 'c0' ? 1 : 0])) };
    judgments.push({ init, body, resolve: () => resolve(new Response(JSON.stringify(wire))) });
  }) }) });
  f.lifecycle(); t.after(() => f.router.close());
  const first = native(2); first.params.clientUserMessageId = 'first-attempt';
  f.router.client(first); await tick();
  assert.equal(judgments.length, 1);
  f.router.client({ id: 3, method: 'mindplayer/turn/cancel', params: { threadId: 'main', clientUserMessageId: 'first-attempt' } });
  const second = native(2); second.params.clientUserMessageId = 'second-attempt';
  f.router.client(second); await tick();
  assert.equal(judgments[0].init.signal.aborted, true);
  assert.equal(judgments.length, 2);
  assert.equal(f.service.threads.get('main').pendingDecision, undefined);
  assert.deepEqual(judgments[0].body.state, judgments[1].body.state);
  judgments[0].resolve(); await tick();
  assert.equal(f.sent.filter(row => row.method === 'turn/start').length, 0);
  judgments[1].resolve(); await tick();
  const starts = f.sent.filter(row => row.method === 'turn/start');
  assert.equal(starts.length, 1);
  assert.equal(starts[0].params.clientUserMessageId, 'second-attempt');
  assert.equal(f.audit.filter(row => row.event === 'jev-started').length, 2);
  assert.equal(f.audit.filter(row => row.event === 'jev-judged').length, 1);
  assert.equal(f.service.threads.get('main').previousDecision, undefined);
  f.router.server({ id: 2, result: { turn: { id: 'second-turn' } } });
  assert.equal(f.service.threads.get('main').previousDecision.turnId, 'second-turn');
});

test('cooldown waits and still judges the next prompt; missing transfer authorization never sends it', async () => {
  const f = harness({ judgmentLimits: { cooldownMs: 15 } }); f.lifecycle();
  const first = native(10); f.router.client(first); await tick();
  f.router.server({ id: 10, result: { turn: { id: 'first' } } });
  f.router.server({ method: 'turn/completed', params: { threadId: 'main', turn: { id: 'first', status: 'completed' } } });
  f.router.client(native(11));
  await new Promise(resolve => setTimeout(resolve, 35));
  assert.equal(f.calls(), 2); assert.equal(f.sent.at(-1).id, 11);
  f.router.close();
  const denied = harness({ externalPromptTransfer: false }); denied.lifecycle();
  denied.router.client(native()); await tick();
  assert.equal(denied.calls(), 0); assert.ok(denied.audit.some(e => e.reason === 'external-transfer-policy-unset'));
  denied.router.close();
});

test('explicit external-transfer denial is honored even with a key', async () => {
  const loaded = deployment(); loaded.policy.consent.externalPromptTransfer = false;
  const f = harness({ policyReader: () => loaded }); f.lifecycle();
  const request = native(); f.router.client(request); await tick();
  assert.deepEqual(f.sent.at(-1), request); assert.equal(f.calls(), 0);
  f.router.close();
});

test('an unrelated helper lifecycle or output cannot cancel the main prompt judgment', async () => {
  let resolve, configurations;
  const f = harness({ judgeFactory: options => { configurations = options.configurations; return () => new Promise(yes => resolve = yes); } }); f.lifecycle();
  f.router.client(native()); await tick();
  f.router.client({ id: 66, method: 'thread/start', params: { ephemeral: true } });
  f.router.server({ id: 66, result: { thread: { id: 'title-helper', source: 'cli', ephemeral: true } } });
  f.router.server({ method: 'item/completed', params: { threadId: 'title-helper', item: { type: 'userMessage', content: [{ type: 'text', text: 'helper-private-canary' }] } } });
  resolve(withConfiguration(judgment('bounded_routine'), configurations)); await tick();
  assert.equal(f.sent.filter(e => e.method === 'turn/start' && e.id === 2).length, 1);
  assert.equal(f.audit.filter(e => e.event === 'jev-judged').length, 1);
  assert.ok(!JSON.stringify(f.audit).includes('helper-private-canary'));
  f.router.close();
});

test('aborting a pending resume-context read promptly cancels before any Jev call', async () => {
  const f = harness(); f.lifecycle();
  f.service.threads.get('main').contextRead = new Promise(() => {});
  const controller = new AbortController();
  const decision = f.service.decide(native(), { signal: controller.signal });
  controller.abort();
  await assert.rejects(decision, { name: 'AbortError' });
  assert.equal(f.calls(), 0);
  f.router.close();
});

test('concurrent main decisions serialize actual calls and respect their start-time interval', async () => {
  const started = []; let inFlight = 0, maximum = 0;
  const f = harness({ judgmentLimits: { cooldownMs: 25 }, judgeFactory: ({ configurations }) => async () => {
    started.push(performance.now()); inFlight++; maximum = Math.max(maximum, inFlight);
    await new Promise(resolve => setTimeout(resolve, 5)); inFlight--;
    return withConfiguration(judgment('bounded_routine'), configurations);
  } }); f.lifecycle();
  const results = await Promise.all([f.service.decide(native(20)), f.service.decide(native(21))]);
  assert.equal(results.length, 2); assert.equal(started.length, 2); assert.equal(maximum, 1);
  assert.ok(started[1] - started[0] >= 20, `start spacing ${started[1] - started[0]}`);
  f.router.close();
});

test('real loopback HTTP fixture drives A→B→A through Jev schema/policy/router; one call/start per turn', async () => {
  let calls = 0, now = 100_000;
  const states = [];
  const gateway = createServer((req, res) => {
    let body = ''; req.on('data', chunk => body += chunk);
    req.on('end', () => {
      assert.equal(req.headers.authorization, 'Bearer gateway-secret-canary');
      const request = JSON.parse(body); states.push(request.state); calls++;
      const capability = request.state.currentInput.includes('복합') ? 'open_problem' : 'bounded_routine';
      res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(wireJudgment(capability)));
    });
  });
  await new Promise(resolve => gateway.listen(0, '127.0.0.1', resolve));
  const loaded = deployment();
  const f = harness({ catalog: fullCatalog, now: () => now, policyReader: () => loaded,
    judgeFactory: options => createJevClient({ ...options, fetchImpl: (_, init) => fetch(`http://127.0.0.1:${gateway.address().port}`, init) }) });
  f.lifecycle();
  try {
    for (const [index, text] of ['정리해줘 nonce-42', '복합 원인을 찾아줘', '짧게 정리해줘'].entries()) {
      const request = native(index + 10); request.params.input[0].text = text;
      const before = f.sent.length; f.router.client(request);
      const deadline = Date.now() + 2000;
      while (f.sent.length === before && !f.received.some(e => e.id === request.id) && Date.now() < deadline) await new Promise(r => setTimeout(r, 5));
      const forwarded = f.sent.at(-1); assert.equal(forwarded.id, request.id);
      assert.equal(forwarded.params.threadId, 'main'); assert.deepEqual(forwarded.params.input, request.params.input);
      assert.equal(forwarded.params.cwd, '/same-cwd'); assert.equal(forwarded.params.approvalPolicy, 'never');
      f.router.server({ id: request.id, result: { turn: { id: `t-${index}` } } });
      f.router.server({ method: 'item/completed', params: { threadId: 'main', item: { type: 'userMessage', content: request.params.input } } });
      f.router.server({ method: 'item/completed', params: { threadId: 'main', item: { type: 'agentMessage', phase: 'final_answer', text: 'nonce-42' } } });
      f.router.server({ method: 'turn/completed', params: { threadId: 'main', turn: { id: `t-${index}`, status: 'completed' } } });
      now += 2;
    }
    assert.deepEqual(f.sent.filter(e => e.method === 'turn/start').map(e => e.params.model), ['native', 'strong', 'native']);
    assert.equal(calls, 3); assert.ok(states[2].recent.some(m => m.text.includes('nonce-42')));
    assert.ok(!JSON.stringify(states).includes('gateway-secret-canary'));
    assert.ok(!JSON.stringify(f.audit).includes('nonce-42'));
    assert.ok(f.audit.filter(e => e.event === 'jev-judged').every(e => e.executionCostUsd === null && e.gatewayCostUsd === null));
  } finally { f.router.close(); await new Promise(resolve => gateway.close(resolve)); }
});

test('Jev failures stop submission; policy change/account update/close discard late results', async () => {
  for (const mutation of ['failure', 'policy', 'account', 'close']) {
    let resolve, reject; let loaded = deployment();
    const f = harness({ catalog: fullCatalog, now: () => 100_000, policyReader: () => loaded,
      judgeFactory: () => () => new Promise((yes, no) => { resolve = yes; reject = no; }) });
    f.lifecycle(); f.router.client(native()); await tick();
    assert.equal(typeof resolve, 'function');
    if (mutation === 'policy') loaded = { ...loaded, digest: 'changed' };
    if (mutation === 'account') f.router.server({ method: 'account/updated', params: { authMode: 'chatgpt', planType: 'business' } });
    if (mutation === 'close') f.router.close();
    if (mutation === 'failure') reject(Object.assign(new Error('secret-canary'), { code: 'timeout' }));
    else resolve({ capability: 'open_problem', confidence: 0.97, probabilities: { bounded_routine: 0.01, multistep_coordination: 0.01, open_problem: 0.97, unknown: 0.01 }, contextSufficient: 0.99, highConsequence: 0.01, usage: { inputTokens: 1, outputTokens: 1 }, gatewayCostUsd: null });
    await tick();
    const starts = f.sent.filter(e => e.method === 'turn/start');
    assert.equal(starts.length, 0);
    if (mutation === 'failure') assert.match(f.received.at(-1).error.message, /nothing was sent/);
    assert.ok(!JSON.stringify(f.audit).includes('secret-canary'));
    f.router.close();
  }
});

test('no candidates fail closed; single/unknown switching evidence still gets a Jev judgment; pending duplicate is explicit error', async () => {
  for (const mode of ['none', 'single', 'unknown']) {
    const loaded = deployment();
    if (mode === 'none') loaded.policy.profiles.forEach(p => p.allowed = false);
    if (mode === 'single') loaded.policy.profiles.splice(1);
    if (mode === 'unknown') loaded.policy.profiles.forEach(p => delete p.cost);
    const f = harness({ catalog: fullCatalog, now: () => 100_000, policyReader: () => loaded }); f.lifecycle();
    f.router.client(native()); await tick();
    assert.equal(f.calls(), mode === 'none' ? 0 : 1);
    assert.equal(f.sent.filter(e => e.method === 'turn/start').length, mode === 'none' ? 0 : 1);
    f.router.close();
  }
  let resolve;
  const f = harness({ catalog: fullCatalog, now: () => 100_000, policyReader: () => deployment(), judgeFactory: () => () => new Promise(r => resolve = r) });
  f.lifecycle(); f.router.client(native()); await tick(); f.router.client(native(3));
  assert.ok(f.received.some(e => e.id === 3 && e.error)); assert.equal(f.sent.filter(e => e.method === 'turn/start').length, 0);
  f.router.close(); resolve(wireJudgment('bounded_routine')); await tick();
  assert.equal(f.sent.filter(e => e.method === 'turn/start').length, 0);
});

test('resume defers empirical context until ON and hydrates same-thread user/final pairs only once', async t => {
  const loaded = deployment();
  let policyReads = 0;
  const f = harness({ requirePromptChoice: true, catalog: fullCatalog, now: () => 100_000,
    policyReader: () => { policyReads++; return loaded; } });
  t.after(() => f.router.close());
  f.lifecycle('thread/resume');
  assert.equal(policyReads, 0);
  assert.equal(f.sent.some(e => e.method === 'thread/turns/list'), false);
  const off = native(10);
  off.params.mindplayerRoutingChoice = { mode: 'native', includeRecent: false };
  assert.equal((await f.service.decide(off)).reason, 'prompt-native');
  assert.equal(policyReads, 0);
  const on = native(11);
  on.params.mindplayerRoutingChoice = { mode: 'jev', includeRecent: false };
  const decision = f.service.decide(on);
  const read = f.sent.find(e => e.method === 'thread/turns/list');
  assert.deepEqual(read.params, { threadId: 'main', limit: 4, sortDirection: 'desc', itemsView: 'full' });
  f.router.server({ id: read.id, result: { data: [{ itemsView: 'full', items: [
    { type: 'userMessage', content: [{ type: 'text', text: 'remember nonce' }] },
    { type: 'reasoning', text: 'reasoning-canary' },
    { type: 'commandExecution', output: 'tool-canary' },
    { type: 'agentMessage', phase: 'final_answer', text: 'nonce' },
  ] }], nextCursor: null } });
  await decision;
  assert.equal(f.received.some(e => e.id === read.id), false);
  assert.equal(f.service.threads.get('main').contextKnown, true);
  assert.ok(!JSON.stringify(f.service.threads.get('main').history).includes('canary'));
  await f.service.decide({ ...on, id: 12 });
  assert.equal(f.sent.filter(e => e.method === 'thread/turns/list').length, 1);
  assert.equal(f.calls(), 2);
});

test('excluded configurations are rejected before waiting or consuming a judgment', async () => {
  const loaded = deployment();
  loaded.policy.profiles[0].allowed = false;
  const f = harness({ catalog: fullCatalog, now: () => 100_000, policyReader: () => loaded });
  f.lifecycle(); f.service.lastCall = 100_000;
  f.router.client(native()); await tick();
  assert.equal(f.sent.filter(e => e.method === 'turn/start').length, 0);
  assert.ok(f.received.at(-1).error); assert.equal(f.calls(), 0);
  f.router.close();
});

test('historical attachments leave context unknown without suppressing judgment; closed threads lose ownership', async () => {
  const f = harness({ catalog: fullCatalog, now: () => 100_000, policyReader: () => deployment() });
  f.lifecycle();
  f.router.server({ method: 'item/completed', params: { threadId: 'main', item: { type: 'userMessage', content: [
    { type: 'text', text: 'see attachment' }, { type: 'image', url: 'attachment-canary' }, null,
  ] } } });
  assert.equal(f.service.threads.get('main').contextKnown, false);
  f.router.client(native()); await tick();
  assert.equal(f.calls(), 1); assert.equal(f.sent.at(-1).params.model, 'native');
  assert.ok(!JSON.stringify(f.service.threads.get('main').history).includes('attachment-canary'));
  f.router.server({ method: 'thread/closed', params: { threadId: 'main' } });
  assert.equal(f.service.threads.get('main').owned, false);
  f.router.close();
});

test('legacy switching-budget attestations do not silently disable every-prompt classification', async () => {
  for (const gateway of [{ dedicatedKeyBudgetVerified: 'true' }, { budgetEvidence: {} }, { budgetEvidence: '  ' }]) {
    const loaded = deployment(); Object.assign(loaded.policy.gateway, gateway);
    const f = harness({ catalog: fullCatalog, now: () => 100_000, policyReader: () => loaded }); f.lifecycle();
    const request = native(); f.router.client(request); await tick();
    assert.equal(f.sent.at(-1).params.model, request.params.model); assert.equal(f.calls(), 1);
    assert.ok(f.audit.some(e => e.event === 'jev-judged'));
    f.router.close();
  }
});

test('missing Gateway key or empty profiles never bypass configured baseline eligibility', async () => {
  for (const mode of ['excluded-no-key', 'empty', 'eligible-no-key', 'identity-unknown']) {
    const loaded = deployment();
    if (mode === 'excluded-no-key') loaded.policy.profiles[0].allowed = false;
    if (mode === 'empty') loaded.policy.profiles = [];
    const f = harness({ catalog: fullCatalog, now: () => 100_000, policyReader: () => loaded,
      apiKey: undefined, ...(mode === 'identity-unknown' ? { account: null } : {}) });
    f.lifecycle(); f.router.client(native()); await tick();
    assert.equal(f.sent.filter(e => e.method === 'turn/start').length, 0);
    assert.equal(f.calls(), 0); f.router.close();
  }
});

test('static deployment requirements cannot pretend to measure a future full thread', async () => {
  const f = harness({ catalog: fullCatalog, now: () => 100_000, policyReader: () => deployment(),
    requirementsForTurn: undefined });
  f.lifecycle(); f.router.client(native()); await tick();
  assert.equal(f.calls(), 1); assert.equal(f.sent.at(-1).params.model, 'native');
  assert.ok(f.audit.some(e => e.reason === 'context-incomplete'));
  f.router.close();
});

test('persistent helpers/forks and unrelated resume cannot acquire main ownership', async () => {
  for (const [method, params] of [
    ['thread/start', { ephemeral: false }],
    ['thread/fork', { threadId: 'main', ephemeral: false }],
    ['thread/resume', { threadId: 'other' }],
  ]) {
    const f = harness({ catalog: fullCatalog, now: () => 100_000, policyReader: () => deployment() });
    f.router.client({ id: 1, method, params });
    f.router.server({ id: 1, result: { thread: { id: 'main', source: 'cli', ephemeral: false }, model: 'native', reasoningEffort: 'max' } });
    f.router.client(native()); await tick();
    assert.equal(f.calls(), 0);
    assert.ok(f.received.some(e => e.id === 2 && e.error));
    assert.equal(f.sent.some(e => e.method === 'turn/start'), false);
    f.router.close();
  }
});

test('same verified main resume retains Jev ownership without an initial resume pin', async () => {
  const f = harness({ expectedThreadId: null });
  f.lifecycle(); f.lifecycle('thread/resume');
  f.router.client(native()); await tick();
  assert.equal(f.calls(), 1);
  assert.ok(f.sent.some(e => e.method === 'turn/start'));
  f.router.close();
});

test('explicit private UI selection binds a different main resume and strips its local marker', async () => {
  const f = harness(); f.lifecycle();
  const request = { id: 3, method: 'thread/resume', params: { threadId: 'selected', mindplayerUserSelection: true } };
  const before = structuredClone(request);
  f.router.client(request);
  assert.deepEqual(f.sent.at(-1), { id: 3, method: 'thread/resume', params: { threadId: 'selected' } });
  assert.deepEqual(request, before);
  f.router.server({ id: 3, result: { thread: { id: 'selected', source: 'vscode', originator: 'codex-tui', threadSource: 'user', ephemeral: false }, model: 'native', reasoningEffort: 'max' } });
  f.router.client(native(4, 'selected')); await tick();
  assert.equal(f.calls(), 1);
  assert.ok(f.sent.some(e => e.id === 4 && e.method === 'turn/start'));
  f.router.close();
});

test('selection marker cannot override helper, parent, source or response identity exclusions', async () => {
  for (const thread of [
    { id: 'selected', source: 'vscode', originator: 'codex-tui', ephemeral: true },
    { id: 'selected', source: 'vscode', originator: 'codex-tui', parentThreadId: 'parent' },
    { id: 'selected', source: 'subAgent', originator: 'codex-tui' },
    { id: 'selected', source: 'vscode', originator: 'codex-tui', threadSource: 'thread_title' },
    { id: 'selected', source: 'vscode', originator: 'codex-tui', threadSource: null },
    { id: 'selected', source: 'vscode', originator: 'codex-tui' },
    { id: 'selected', source: 'vscode', originator: 'foreign' },
    { id: 'different', source: 'vscode', originator: 'codex-tui' },
  ]) {
    const f = harness(); f.lifecycle();
    f.router.client({ id: 3, method: 'thread/resume', params: { threadId: 'selected', mindplayerUserSelection: true } });
    f.router.server({ id: 3, result: { thread, model: 'native', reasoningEffort: 'max' } });
    assert.equal(f.service.threads.get(thread.id).owned, false);
    assert.equal(f.calls(), 0); f.router.close();
  }
});

test('account invalidation cannot silently pass subsequent main prompts through', async () => {
  const f = harness(); f.lifecycle(); f.service.invalidate();
  f.router.client(native()); await tick();
  assert.equal(f.calls(), 0);
  assert.equal(f.sent.some(e => e.method === 'turn/start'), false);
  assert.ok(f.received.some(e => e.id === 2 && e.error));
  f.router.close();
});

test('late lifecycle replies cannot reacquire ownership after account invalidation or closure', async () => {
  for (const revoke of ['account', 'closed']) {
    const f = harness(); f.lifecycle();
    f.router.client({ id: 3, method: 'thread/resume', params: { threadId: 'main', mindplayerUserSelection: true } });
    if (revoke === 'account') f.service.invalidate();
    else f.router.server({ method: 'thread/closed', params: { threadId: 'main' } });
    f.router.server({ id: 3, result: { thread: { id: 'main', source: 'vscode', originator: 'codex-tui', threadSource: 'user', ephemeral: false }, model: 'native', reasoningEffort: 'max' } });
    f.router.client(native(4)); await tick();
    assert.equal(f.calls(), 0);
    assert.equal(f.sent.some(e => e.method === 'turn/start'), false);
    assert.ok(f.received.some(e => e.id === 4 && e.error));
    f.router.close();
  }
});

test('stale lifecycle ACK cannot clear closure or leave ghost activity across fresh resume and retry', async () => {
  for (const withStaleAck of [true, false]) {
    const f = harness(); f.router.requireFreshTurn = true; f.lifecycle();
    const resume = id => ({ id, method: 'thread/resume', params: { threadId: 'main', mindplayerUserSelection: true } });
    const ack = id => ({ id, result: { thread: { id: 'main', source: 'vscode', originator: 'codex-tui', threadSource: 'user' }, model: 'native', reasoningEffort: 'max' } });
    f.router.client(resume(3));
    f.router.server({ method: 'thread/closed', params: { threadId: 'main' } });
    if (withStaleAck) f.router.server(ack(3));
    assert.ok(f.router.closedThreads.has('main'));
    f.router.server({ method: 'turn/started', params: { threadId: 'main', turn: { id: 'ghost' } } });
    assert.equal(f.router.active.has('main'), false);
    f.router.client(resume(4)); f.router.server(ack(4));
    assert.equal(f.router.closedThreads.has('main'), false);
    f.router.client(native(5)); await tick();
    assert.equal(f.calls(), 1);
    assert.equal(f.sent.filter(row => row.method === 'turn/start').length, 1);
    f.router.server({ id: 5, error: { code: -32603, message: 'synthetic native rejection' } });
    f.router.client(native(6)); await tick();
    assert.equal(f.calls(), 2);
    assert.equal(f.sent.filter(row => row.method === 'turn/start').length, 2);
    assert.equal(f.router.active.has('main'), false);
    assert.ok(!f.received.some(row => row.error?.message.includes('already active')));
    f.router.close();
  }
});

test('legacy expected resume bootstraps only once; explicit non-user classification always excludes it', async () => {
  for (const threadSource of [null, undefined, 'automation', 'subagent', 'guardian_review']) {
    const f = harness();
    const resume = id => {
      f.router.client({ id, method: 'thread/resume', params: { threadId: 'main' } });
      f.router.server({ id, result: { thread: { id: 'main', source: 'cli', threadSource, ephemeral: false }, model: 'native', reasoningEffort: 'max' } });
    };
    resume(3);
    assert.equal(f.service.threads.get('main').owned, threadSource == null);
    f.service.invalidate(); resume(4);
    assert.equal(f.service.threads.get('main').owned, false);
    f.router.close();
  }
});

test('close before the first lifecycle ACK cannot manufacture a fresh main binding', async () => {
  const f = harness();
  f.router.client({ id: 3, method: 'thread/resume', params: { threadId: 'main', mindplayerUserSelection: true } });
  f.router.server({ method: 'thread/closed', params: { threadId: 'main' } });
  f.router.server({ id: 3, result: { thread: { id: 'main', source: 'vscode', originator: 'codex-tui', threadSource: 'user' }, model: 'native', reasoningEffort: 'max' } });
  f.router.client(native(4)); await tick();
  assert.equal(f.calls(), 0);
  assert.equal(f.sent.some(e => e.method === 'turn/start'), false);
  assert.ok(f.received.some(e => e.id === 4 && e.error));
  f.router.close();
});

test('marked user new/fork lifecycles route independently while background lookalikes stay unowned', async () => {
  for (const method of ['thread/start', 'thread/fork']) {
    const f = harness(); f.lifecycle();
    const params = { threadId: 'main', ephemeral: false, threadSource: 'user', mindplayerUserSelection: true };
    f.router.client({ id: 3, method, params });
    assert.ok(!Object.hasOwn(f.sent.at(-1).params, 'mindplayerUserSelection'));
    f.router.server({ id: 3, result: { thread: { id: 'selected', source: 'vscode', originator: 'codex-tui', threadSource: 'user', ephemeral: false }, model: 'native', reasoningEffort: 'max' } });
    f.router.client(native(4, 'selected')); await tick();
    assert.equal(f.calls(), 1);
    assert.equal(f.sent.filter(e => e.id === 4 && e.method === 'turn/start').length, 1);
    f.router.close();
  }
});

test('measured native 0.162.1 first startup tuple owns main; later lookalikes remain unowned', () => {
  const f = harness({ expectedThreadId: null });
  const observe = (id, threadId, params = {}, thread = {}) => {
    f.router.client({ id, method: 'thread/start', params: { ephemeral: false, sessionStartSource: null, threadSource: 'user', ...params } });
    f.router.server({ id, result: { thread: { id: threadId, source: 'vscode', originator: 'codex-tui', ...thread }, model: 'native', reasoningEffort: 'max' } });
  };
  observe(1, 'native-main'); assert.equal(f.service.threads.get('native-main').owned, true);
  observe(2, 'helper'); assert.equal(f.service.threads.get('helper').owned, false);
  f.router.close();
  for (const [params, thread] of [[{ ephemeral: true }, {}], [{ threadSource: 'other' }, {}], [{}, { originator: 'unknown' }], [{}, { parentThreadId: 'parent' }]]) {
    const other = harness({ expectedThreadId: null });
    const lifecycle = { method: 'thread/start', params: { ephemeral: false, sessionStartSource: null, threadSource: 'user', ...params } };
    other.service.trackLifecycle(lifecycle);
    other.service.observe(lifecycle,
      { result: { thread: { id: 'helper', source: 'vscode', originator: 'codex-tui', ...thread } } });
    assert.equal(other.service.threads.get('helper').owned, false); other.router.close();
  }
});

test('paginated catalog refresh handles removals/additions; incomplete refresh cannot route', async () => {
  const f = harness({ catalog: fullCatalog, now: () => 100_000, policyReader: () => deployment() }); f.lifecycle();
  f.router.client({ id: 20, method: 'model/list', params: { cursor: null } });
  f.router.server({ id: 20, result: { data: [catalog[0]], nextCursor: 'page-two' } });
  f.router.client(native()); await tick();
  assert.ok(f.received.some(e => e.id === 2 && e.error)); assert.equal(f.calls(), 0);
  f.router.client({ id: 21, method: 'model/list', params: { cursor: 'page-two' } });
  f.router.server({ id: 21, result: { data: [{ ...catalog[0], model: 'new-model' }], nextCursor: null } });
  assert.deepEqual(f.service.catalog.map(row => row.model), ['native', 'new-model']);
  const request = native(3); f.router.client(request); await tick();
  assert.equal(f.sent.at(-1).params.model, 'native'); assert.equal(f.calls(), 1);
  f.router.close();
});

test('concurrent panes bind account, key, snapshot and late response independently', async () => {
  const panes = ['one', 'two'].map(name => {
    const loaded = deployment(), accountHome = `/account-${name}`;
    const fingerprint = loginFingerprint(account, accountHome);
    loaded.policy.profiles.forEach(profile => { profile.accountFingerprint = fingerprint; profile.cost.accountFingerprint = fingerprint; });
    let resolve, snapshot;
    const f = harness({ catalog: fullCatalog, accountHome, apiKey: `key-canary-${name}`, now: () => 100_000, policyReader: () => loaded,
      judgeFactory: ({ apiKey }) => state => {
        assert.equal(apiKey, `key-canary-${name}`); snapshot = state;
        return new Promise(yes => resolve = yes);
      } });
    f.lifecycle();
    const request = native(); request.params.input[0].text = `private-pane-${name}`;
    f.router.client(request);
    return { f, resolve: () => resolve, snapshot: () => snapshot };
  });
  await tick();
  for (const [index, pane] of [...panes.entries()].reverse()) {
    const capability = index === 0 ? 'open_problem' : 'bounded_routine';
    const raw = wireJudgment(capability);
    pane.resolve()({ capability, confidence: 0.97, probabilities: raw.answers.required_capability.probabilities,
      contextSufficient: 0.99, highConsequence: 0.01, usage: { inputTokens: 1, outputTokens: 1 }, gatewayCostUsd: null });
  }
  await tick();
  assert.equal(panes[0].f.sent.at(-1).params.model, 'strong');
  assert.equal(panes[1].f.sent.at(-1).params.model, 'native');
  assert.ok(!JSON.stringify(panes[0].snapshot()).includes('private-pane-two'));
  assert.ok(!JSON.stringify(panes[1].snapshot()).includes('private-pane-one'));
  for (const pane of panes) {
    assert.ok(!JSON.stringify(pane.f.audit).includes('canary'));
    assert.ok(!JSON.stringify(pane.snapshot()).includes('key-canary'));
    pane.f.router.close();
  }
});

const descriptorCatalog = [
  { ...catalog[0], description: 'Native fast model for bounded routine work.', defaultReasoningEffort: 'max',
    supportedReasoningEfforts: [{ reasoningEffort: 'low', description: 'Less reasoning for straightforward work.' }, { reasoningEffort: 'max', description: 'Careful reasoning.' }] },
  { ...catalog[0], model: 'strong', description: 'Native model for unresolved investigation and complex problems.', defaultReasoningEffort: 'high',
    supportedReasoningEfforts: [{ reasoningEffort: 'high', description: 'Careful reasoning for difficult tasks.' }, { reasoningEffort: 'max', description: 'Maximum reasoning.' }, { reasoningEffort: 'ultra', description: 'Automatic delegation.' }] },
];

test('absent policy routes A→B→A through real HTTP/client Choice and router, preserving exact effort, context and native parameters', async () => {
  const bodies = [];
  const gateway = createServer((req, res) => {
    let body = ''; req.on('data', chunk => body += chunk);
    req.on('end', () => {
      assert.ok(Buffer.byteLength(body) <= 32768);
      assert.equal(req.headers.authorization, 'Bearer gateway-secret-canary');
      const request = JSON.parse(body); bodies.push(request);
      const complex = request.state.currentInput.includes('복합');
      const capability = complex ? 'open_problem' : 'bounded_routine';
      const entries = Object.entries(request.questions.configuration.criteria);
      const selected = entries.find(([, value]) => value.model === (complex ? 'strong' : 'native') && value.effort === (complex ? 'high' : 'low'));
      assert.ok(selected);
      const response = wireJudgment(capability);
      response.answers.configuration = { type: 'choice', choice: selected[0], confidence: 0.95,
        probabilities: Object.fromEntries(entries.map(([key]) => [key, key === selected[0] ? 1 : 0])) };
      res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(response));
    });
  });
  await new Promise(resolve => gateway.listen(0, '127.0.0.1', resolve));
  const f = harness({ catalog: descriptorCatalog,
    judgeFactory: options => createJevClient({ ...options, fetchImpl: (_, init) => fetch(`http://127.0.0.1:${gateway.address().port}`, init) }) });
  f.lifecycle();
  try {
    for (const [index, text] of ['정리 nonce-42', '복합 원인 조사', '요약해줘'].entries()) {
      const request = native(index + 10); request.params.input[0].text = text;
      if (index) { delete request.params.model; delete request.params.effort; }
      else request.params.collaborationMode = { mode: 'default', settings: { model: 'native', reasoning_effort: 'max', developer_instructions: 'preserve-native' } };
      const before = structuredClone(request);
      const sentBefore = f.sent.length; f.router.client(request);
      const deadline = Date.now() + 2000;
      while (f.sent.length === sentBefore && !f.received.some(e => e.id === request.id) && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 5));
      const forwarded = f.sent.at(-1); assert.equal(forwarded.id, request.id);
      const expected = structuredClone(before);
      expected.params.model = index === 1 ? 'strong' : 'native'; expected.params.effort = index === 1 ? 'high' : 'low';
      if (expected.params.collaborationMode) Object.assign(expected.params.collaborationMode.settings, { model: expected.params.model, reasoning_effort: expected.params.effort });
      assert.deepEqual(forwarded, expected); assert.deepEqual(request, before);
      f.router.server({ id: request.id, result: { turn: { id: `selected-${index}` } } });
      f.router.server({ method: 'item/completed', params: { threadId: 'main', item: { type: 'userMessage', content: request.params.input } } });
      f.router.server({ method: 'item/completed', params: { threadId: 'main', item: { type: 'agentMessage', phase: 'final_answer', text: 'nonce-42' } } });
      f.router.server({ method: 'turn/completed', params: { threadId: 'main', turn: { id: `selected-${index}`, status: 'completed' } } });
    }
    assert.deepEqual(f.sent.filter(e => e.method === 'turn/start').map(e => [e.params.model, e.params.effort]), [['native', 'low'], ['strong', 'high'], ['native', 'low']]);
    assert.equal(bodies.length, 3);
    assert.deepEqual(bodies.map(body => body.state.baseline), [{ model: 'native', effort: 'max' }, { model: 'native', effort: 'low' }, { model: 'strong', effort: 'high' }]);
    assert.deepEqual(bodies[2].state.recent, []);
    assert.deepEqual(bodies[2].state.previousDecision, { model: 'strong', effort: 'high', capability: 'open_problem',
      contextSufficient: 0.99, highConsequence: 0.01, turnOutcome: 'completed' });
    assert.ok(!JSON.stringify(bodies[2].state).includes('nonce-42'));
    assert.ok(bodies.every(body => Object.keys(body.questions).length === 4));
    assert.ok(f.audit.filter(e => e.event === 'model-selected').every(e => e.reason === 'catalog-guided-jev' && e.executionCostUsd === null));
    assert.ok(!JSON.stringify(f.audit).includes('canary')); assert.ok(!JSON.stringify(f.audit).includes('nonce-42'));
    assert.ok(!JSON.stringify(bodies).includes(account.email)); assert.ok(!JSON.stringify(bodies).includes('gateway-secret-canary'));
  } finally { f.router.close(); await new Promise(resolve => gateway.close(resolve)); }
});

test('catalog selection excludes hidden, denied, other-account/provider, expired, duplicate and unsupported configurations', async () => {
  const row = descriptorCatalog[1];
  const excluded = [
    { ...row, model: 'hidden', hidden: true }, { ...row, model: 'denied', accessDenied: true },
    { ...row, model: 'unavailable', available: false }, { ...row, model: 'blocked', availability: { status: 'blocked' } },
    { ...row, model: 'other-account', accountFingerprint: 'not-the-login' }, { ...row, model: 'other-provider', modelProvider: 'foreign-backend' },
    { ...row, model: 'image-only', inputModalities: ['image'] }, { ...row, model: 'unknown-modalities', inputModalities: undefined },
    { ...row, model: 'unknown-efforts', supportedReasoningEfforts: undefined }, { ...row, model: 'string-efforts', supportedReasoningEfforts: ['max'] },
    { ...row, model: 'invented-effort', supportedReasoningEfforts: [{ reasoningEffort: 'invented' }] },
    { ...row, model: 'ultra-only', supportedReasoningEfforts: [{ reasoningEffort: 'ultra' }] },
    { ...row, model: 'expired', expiresAt: 99_999 }, { ...row, model: 'invalid-expiry', availability: { expiresAt: 'never' } },
    { ...row, model: 'future-observation', availability: { verifiedAt: 100_001, expiresAt: 200_000 } },
    { ...row, model: 'duplicate' }, { ...row, model: 'duplicate', hidden: true }, { ...row, model: 'invalid\nmodel' },
  ];
  const noReasoning = { ...row, model: 'no-reasoning', supportedReasoningEfforts: [], defaultReasoningEffort: null };
  let configurations;
  const f = harness({ catalog: [...descriptorCatalog, noReasoning, ...excluded], now: () => 100_000,
    judgeFactory: options => { configurations = options.configurations; return async () => withConfiguration(judgment('bounded_routine'), configurations); } });
  f.lifecycle(); f.service.threads.get('main').modelProvider = 'native-backend';
  const result = await f.service.decide(native());
  assert.equal(result.model, 'native'); assert.equal(result.effort, 'max');
  assert.deepEqual(configurations[0], { model: 'native', effort: 'max', description: 'Native fast model for bounded routine work.\nCareful reasoning.' });
  assert.deepEqual([...new Set(configurations.map(value => value.model))].sort(), ['native', 'no-reasoning', 'strong']);
  assert.ok(configurations.some(value => value.model === 'no-reasoning' && value.effort === null));
  assert.ok(configurations.every(value => value.effort !== 'ultra' && Object.keys(value).join(',') === 'model,effort,description'));
  f.router.close();
});

test('invalid exact baselines fail without selecting a catalog default or consuming Jev', async () => {
  const changes = [
    rows => rows.shift(), rows => { rows[0].hidden = true; }, rows => { rows[0].accessDenied = true; },
    rows => { rows[0].inputModalities = undefined; }, rows => { rows[0].supportedReasoningEfforts = [{ reasoningEffort: 'low' }]; },
    rows => { rows[0].supportedReasoningEfforts = []; rows[0].defaultReasoningEffort = 'max'; },
    rows => rows.push({ ...rows[0] }), rows => { rows[0].accountFingerprint = 'foreign'; },
    rows => { rows[0].expiresAt = 100_000; },
  ];
  for (const change of changes) {
    const rows = structuredClone(descriptorCatalog); change(rows);
    const f = harness({ catalog: rows, now: () => 100_000 }); f.lifecycle();
    await assert.rejects(f.service.decide(native()), { code: 'baseline-ineligible' }); assert.equal(f.calls(), 0);
    f.router.close();
  }
});

test('candidate breadth and serialized criteria are finite and baseline is first regardless of catalog order', async () => {
  for (const description of ['native descriptor', '\\"한글\n'.repeat(500)]) {
    let configurations, body;
    const rows = Array.from({ length: 60 }, (_, i) => ({ ...descriptorCatalog[1], model: `candidate-${i}`, description }));
    const f = harness({ catalog: [...rows, descriptorCatalog[0]], judgmentLimits: { cooldownMs: 0, maxRequestBytes: 24576 },
      judgeFactory: options => {
        configurations = options.configurations;
        return createJevClient({ ...options, fetchImpl: (_, init) => {
          body = JSON.parse(init.body); assert.ok(Buffer.byteLength(init.body) <= 24576);
          const response = wireJudgment('bounded_routine');
          response.answers.configuration = { type: 'choice', choice: 'c0', confidence: 1,
            probabilities: Object.fromEntries(configurations.map((_, i) => [`c${i}`, i === 0 ? 1 : 0])) };
          return Response.json(response);
        } });
      } });
    f.lifecycle(); const request = native(); request.params.input[0].text = '\\"한글\n'.repeat(10_000);
    const selected = await f.service.decide(request);
    assert.equal(selected.model, 'native'); assert.equal(selected.reason, 'context-incomplete');
    assert.deepEqual(body.state.baseline, { model: 'native', effort: 'max' });
    assert.equal(body.state.truncated, true); assert.equal(body.state.contextKnown, false);
    assert.ok(configurations.length <= 32 && configurations.length > 1);
    assert.ok(Buffer.byteLength(JSON.stringify(body.questions.configuration.criteria)) <= 8192);
    assert.ok(Buffer.byteLength(JSON.stringify(body.state)) <= 8192);
    assert.ok(configurations.every(value => Buffer.byteLength(value.description) <= 1024));
    if (description === 'native descriptor') assert.ok(new Set(configurations.map(value => value.model)).size >= 30);
    f.router.close();
  }
});

test('explicit ultra baseline stays exact while automatically offered efforts never enable delegation', async () => {
  let configurations;
  const rows = structuredClone(descriptorCatalog); rows[0].supportedReasoningEfforts.push({ reasoningEffort: 'ultra', description: 'Delegation scope.' });
  const f = harness({ catalog: rows, judgeFactory: options => {
    configurations = options.configurations; return async () => withConfiguration(judgment('open_problem'), configurations);
  } }); f.lifecycle();
  const request = native(); request.params.effort = 'ultra';
  const selected = await f.service.decide(request);
  assert.equal(configurations.length, 1); assert.equal(selected.effort, 'ultra'); assert.equal(selected.changed, false);
  f.router.close();
});

test('null-effort native models retain their exact verified configuration', async () => {
  const rows = [{ ...descriptorCatalog[0], supportedReasoningEfforts: [], defaultReasoningEffort: null }];
  const f = harness({ catalog: rows });
  const lifecycle = { method: 'thread/start', params: { sessionStartSource: 'startup' } };
  f.service.trackLifecycle(lifecycle);
  f.service.observe(lifecycle,
    { result: { thread: { id: 'main', source: 'cli' }, model: 'native', reasoningEffort: null } });
  const request = native(); delete request.params.effort;
  const selected = await f.service.decide(request);
  assert.equal(selected.effort, null); assert.equal(selected.model, 'native'); assert.equal(f.calls(), 1);
  f.router.close();
});

test('prompt-first resume skips history paging; missing references still preserve the exact baseline', async () => {
  for (const capability of ['bounded_routine', 'unknown']) {
    let snapshot;
    const f = harness({ catalog: descriptorCatalog, judgeFactory: ({ configurations }) => async state => {
      snapshot = state; return withConfiguration(judgment(capability), configurations, configurations.findIndex(value => value.model === 'strong' && value.effort === 'high'));
    } }); f.lifecycle('thread/resume');
    assert.ok(!f.sent.some(e => e.method === 'thread/turns/list'));
    const selected = await f.service.decide(native());
    assert.equal(snapshot.contextKnown, false); assert.equal(snapshot.truncated, false);
    assert.deepEqual(snapshot.recent, []); assert.equal(snapshot.previousDecision, null);
    assert.equal(selected.model, capability === 'unknown' ? 'native' : 'strong');
    assert.equal(selected.effort, capability === 'unknown' ? 'max' : 'high');
    f.router.close();
  }
});

test('high consequence Choice can select more capable native descriptors without invented numeric quality gates', async () => {
  const f = harness({ catalog: descriptorCatalog, judgeFactory: ({ configurations }) => async () => withConfiguration({ ...judgment('open_problem'), highConsequence: 0.99 },
    configurations, configurations.findIndex(value => value.model === 'strong' && value.effort === 'high')) });
  f.lifecycle(); const selected = await f.service.decide(native());
  assert.equal(selected.model, 'strong'); assert.equal(selected.effort, 'high'); assert.equal(selected.executionCostUsd, null);
  f.router.close();
});

test('current attachments and unknown inputs preserve baseline; historical attachments are not routed again', async () => {
  for (const mode of ['image', 'unknown', 'historical']) {
    let snapshot;
    const rows = structuredClone(descriptorCatalog); rows[0].inputModalities.push('image');
    const f = harness({ catalog: rows, judgeFactory: ({ configurations }) => async state => {
      snapshot = state; return withConfiguration(judgment('bounded_routine'), configurations, configurations.findIndex(value => value.model === 'native' && value.effort === 'low'));
    } }); f.lifecycle();
    const request = native();
    const input = { type: mode === 'unknown' ? 'future-input' : 'image', path: '/attachment-path-canary', url: 'private-url-canary' };
    if (mode === 'historical') f.router.server({ method: 'item/completed', params: { threadId: 'main', item: { type: 'userMessage', content: [{ type: 'text', text: 'prior' }, input] } } });
    else request.params.input.push(input);
    const selected = await f.service.decide(request);
    assert.equal(selected.model, 'native'); assert.equal(selected.effort, mode === 'historical' ? 'low' : 'max');
    assert.equal(selected.reason, mode === 'historical' ? 'catalog-guided-jev' : 'context-incomplete');
    assert.equal(snapshot.attachments, mode !== 'historical'); assert.equal(snapshot.contextKnown, false);
    assert.ok(!JSON.stringify(snapshot).includes('path-canary')); assert.ok(!JSON.stringify(snapshot).includes('url-canary'));
    f.router.close();
  }
});

test('catalog choice requires verified account identity and exact native backend provider', async () => {
  const missing = harness({ account: null, catalog: descriptorCatalog }); missing.lifecycle();
  await assert.rejects(missing.service.decide(native()), { code: 'login-unverified' }); assert.equal(missing.calls(), 0); missing.router.close();
  const f = harness({ catalog: descriptorCatalog.map(row => ({ ...row, modelProvider: 'backend-two' })) });
  const lifecycle = { method: 'thread/start', params: { sessionStartSource: 'startup' } };
  f.service.trackLifecycle(lifecycle);
  f.service.observe(lifecycle,
    { result: { thread: { id: 'main', source: 'cli' }, model: 'native', reasoningEffort: 'max', modelProvider: 'backend-one' } });
  assert.equal(f.service.threads.get('main').modelProvider, 'backend-one');
  await assert.rejects(f.service.decide(native()), { code: 'baseline-ineligible' }); assert.equal(f.calls(), 0); f.router.close();
});

test('catalog expiry and account/catalog changes discard held choices instead of submitting or falling back', async () => {
  for (const mutation of ['expiry', 'account', 'catalog']) {
    let now = 100_000, resolve, configurations;
    const rows = structuredClone(descriptorCatalog); rows[1].expiresAt = now + 1;
    const f = harness({ catalog: rows, now: () => now, judgeFactory: options => {
      configurations = options.configurations; return () => new Promise(yes => resolve = yes);
    } }); f.lifecycle();
    const decision = f.service.decide(native()); await tick();
    if (mutation === 'expiry') now += 2;
    if (mutation === 'account') f.service.observe({ method: 'account/read' }, { result: { account: { ...account, email: 'different@example.invalid' } } });
    if (mutation === 'catalog') f.service.observe({ method: 'model/list', params: {} }, { result: { data: [rows[0]], nextCursor: null } });
    resolve(withConfiguration(judgment('open_problem'), configurations, configurations.findIndex(value => value.model === 'strong' && value.effort === 'high')));
    await assert.rejects(decision, { code: mutation === 'expiry' ? 'selected-configuration-unavailable' : 'stale-decision' });
    assert.equal(f.sent.some(e => e.method === 'turn/start'), false); f.router.close();
  }
  let now = 100_000;
  const rows = structuredClone(descriptorCatalog); rows[0].expiresAt = now + 1;
  const f = harness({ catalog: rows, now: () => now }); f.lifecycle();
  const decision = await f.service.decide(native()); assert.equal(decision.isCurrent(), true);
  now += 2; assert.equal(decision.isCurrent(), false); f.router.close();
});

test('invalid present policy fails closed; only a genuinely absent file enables descriptor choices', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'mindplayer-invalid-policy-')), path = join(dir, 'policy.json');
  try {
    const absent = harness({ catalog: descriptorCatalog, policyReader: () => loadPolicy(path) }); absent.lifecycle();
    assert.equal((await absent.service.decide(native())).reason, 'catalog-guided-jev'); absent.router.close();
    writeFileSync(path, '{invalid-secret-canary', { mode: 0o600 });
    const present = harness({ catalog: descriptorCatalog, policyReader: () => loadPolicy(path) }); present.lifecycle();
    await assert.rejects(present.service.decide(native()), error => error.code === 'policy-invalid' && error.message === 'policy-invalid' && error.cause === undefined);
    assert.equal(present.calls(), 0); present.router.close();
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('missing Choice or failed Jev throws a safe code and sends no native-after-failure fallback', async () => {
  for (const mode of ['missing-choice', 'timeout', 'unknown-error', 'no-key']) {
    const f = harness({ catalog: descriptorCatalog, ...(mode === 'no-key' ? { apiKey: undefined } : {}),
      judgeFactory: () => async () => {
        if (mode === 'missing-choice') return judgment('bounded_routine');
        throw Object.assign(new Error('raw-secret-canary'), { code: mode === 'timeout' ? 'timeout' : 'raw-secret-canary' });
      } }); f.lifecycle();
    await assert.rejects(f.service.decide(native()), error => {
      assert.equal(error.code, mode === 'missing-choice' ? 'invalid-response' : mode === 'timeout' ? 'timeout' : mode === 'no-key' ? 'jev-key-unavailable' : 'jev-unavailable');
      assert.equal(error.message, error.code); assert.equal(error.cause, undefined); assert.ok(!error.stack.includes('raw-secret-canary')); return true;
    });
    assert.equal(f.sent.some(e => e.method === 'turn/start'), false); assert.ok(!JSON.stringify(f.audit).includes('canary'));
    f.router.close();
  }
});

test('empirical policy remains strict and does not acquire catalog heuristics when requirements or cost evidence are unknown', async () => {
  for (const mode of ['requirements', 'cost', 'thresholds']) {
    const loaded = deployment(); let configurations = 'unset';
    if (mode === 'cost') loaded.policy.profiles.forEach(profile => delete profile.cost);
    if (mode === 'thresholds') delete loaded.policy.thresholds;
    const f = harness({ catalog: descriptorCatalog, now: () => 100_000, policyReader: () => loaded,
      ...(mode === 'requirements' ? { requirementsForTurn: undefined } : {}),
      judgeFactory: options => { configurations = options.configurations; return async () => judgment('open_problem'); } });
    f.lifecycle();
    if (mode === 'cost') await assert.rejects(f.service.decide(native()), { code: 'QUALITY_FLOOR_UNMET' });
    else {
      const selected = await f.service.decide(native());
      assert.equal(selected.model, 'native'); assert.equal(selected.effort, 'max'); assert.notEqual(selected.reason, 'catalog-guided-jev');
    }
    assert.equal(configurations, undefined); f.router.close();
  }
});

test('a rejected native request invalidates pending decisions without permanently denying the model or replaying a turn', async () => {
  const f = harness({ catalog: descriptorCatalog }); f.lifecycle();
  f.router.client(native()); await tick(); assert.equal(f.calls(), 1);
  f.router.server({ id: 2, error: { code: -32602, message: 'access denied raw-secret-canary', data: { type: 'invalid-input' } } });
  assert.equal(f.service.catalog.find(row => row.model === 'native').accessDenied, undefined);
  assert.equal(f.sent.filter(e => e.method === 'turn/start').length, 1);
  f.router.client(native(3)); await tick();
  assert.equal(f.calls(), 2); assert.equal(f.sent.filter(e => e.method === 'turn/start').length, 2);
  assert.ok(!JSON.stringify(f.audit).includes('raw-secret-canary')); f.router.close();
});
