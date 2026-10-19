// Test-only preload: exercise the installed router without changing its wire request.
// Offline Jev responses and the companion Codex server are synthetic; using
// current-family IDs does not prove genuine model inference or native effort.
import { appendFileSync, chmodSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { basename, dirname, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { validateJudgment } from './jev.mjs';
import { loadJevCredentials } from './service.mjs';
import { buildConfigurationInstructions, RUBRIC_VERSION } from './rubric.mjs';

const endpoint = 'https://api.typesafe.ai/v1/systemone';
const gatewayEndpoint = 'https://ai-gateway.vercel.sh/typesafe/v1/systemone';
const canary = 'mindplayer-smoke-typesafe-canary';

// Gate only the synthetic backend's known turn messages. Production router
// traffic and the real-provider fixture keep their original transport.
function installLifecycleGates(markerDir) {
  const output = process.stdout.write.bind(process.stdout);
  const turns = new Map();
  process.stdout.write = function (chunk, encoding, callback) {
    let message;
    try { message = JSON.parse(chunk.toString()); } catch { return output(chunk, encoding, callback); }
    const ack = message.id !== undefined && message.result?.turn;
    const completed = message.method === 'turn/completed' && message.params?.turn;
    const label = (ack || completed)?.id?.match(/^fixture-turn-([ABC])$/)?.[1];
    if (!label) return output(chunk, encoding, callback);
    const turn = turns.get(label) ?? { emitted: new Set() };
    if (ack) turn.ack = chunk;
    else {
      turn.completed = chunk;
      turn.started = `${JSON.stringify({ method: 'turn/started', params: {
        threadId: message.params.threadId, turn: { id: completed.id, status: 'inProgress' },
      } })}\n`;
    }
    turns.set(label, turn);
    const done = typeof encoding === 'function' ? encoding : callback;
    if (done) queueMicrotask(done);
    return true;
  };
  setInterval(() => {
    for (const [label, turn] of turns) {
      for (const event of ['ack', 'started', 'completed']) {
        if (turn[event] && !turn.emitted.has(event) && existsSync(join(markerDir, `${label}.${event}`))) {
          output(turn[event]);
          turn.emitted.add(event);
        }
      }
      if (turn.emitted.size === 3) turns.delete(label);
    }
  }, 10).unref();
}

// The parent selects exactly the credential production would use, including a
// saved credential. Only its provider crosses stdout; the isolated copy is
// private, exclusively created, and removed by the Rust fixture's Drop guard.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href
    && process.argv[2] === '--prepare-credentials') {
  try {
    const credential = loadJevCredentials();
    if (!credential || credential.apiKey === canary) throw new Error();
    const path = process.argv[3];
    if (!path) throw new Error();
    mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
    chmodSync(dirname(path), 0o700);
    writeFileSync(path, JSON.stringify({ schemaVersion: 1, consentVersion: 'jev-per-prompt-v1', ...credential }), { mode: 0o600, flag: 'wx' });
    process.stdout.write(`${JSON.stringify({ provider: credential.provider })}\n`);
  } catch {
    process.stderr.write('Jev fixture credential staging failed\n');
    process.exitCode = 1;
  }
} else {
const markerDir = process.env.MINDPLAYER_ROUTER_MARKER_DIR;
if (!markerDir) throw new Error('Jev fixture marker directory is required');
mkdirSync(markerDir, { recursive: true, mode: 0o700 });
chmodSync(markerDir, 0o700);
const marker = join(markerDir, 'jev.jsonl');
const originalFetch = globalThis.fetch;
const real = process.env.MINDPLAYER_TEST_REAL_JEV === '1';
const record = event => appendFileSync(marker, `${JSON.stringify(event)}\n`, { mode: 0o600 });
if (!real && process.env.MINDPLAYER_ROUTER_LIFECYCLE_GATES === '1'
    && basename(process.argv[1] ?? '') === 'mindplayer-fixture.mjs'
    && process.argv.slice(2).includes('app-server')) installLifecycleGates(markerDir);

globalThis.fetch = async function (...args) {
  const [input, init] = args;
  const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input?.url;
  if (![endpoint, gatewayEndpoint].includes(url)) return Reflect.apply(originalFetch, this, args);
  const provider = url === endpoint ? 'typesafe' : 'gateway';
  if (!real && provider !== 'typesafe') throw new Error('Offline Jev fixture requires the TypeSafe canary');

  let request;
  try {
    request = JSON.parse(init?.body ?? (input instanceof Request ? await input.clone().text() : ''));
  } catch { /* Unknown input stays bootstrap; request contents are never logged. */ }
  const text = typeof request?.state === 'string' ? request.state : request?.state?.currentInput;
  const label = text === 'A' ? 'A' : text === 'analyze B' ? 'B' : text === 'C' ? 'C' : 'bootstrap';
  const state = request?.state;
  const prior = state?.previousDecision;
  const previousDecision = prior == null ? null : Object.fromEntries(
    ['model', 'effort', 'capability', 'contextSufficient', 'highConsequence', 'turnOutcome'].map(key => [key, prior[key]]));
  record({ event: 'start', label, provider, rubricVersion: state?.rubricVersion ?? null,
    // Exact snapshot evidence is confined to offline synthetic prompts. The
    // opt-in authenticated fixture retains metadata-only audit records.
    ...(!real ? { state, stateJson: JSON.stringify(state) } : {}),
    stateKeys: state && typeof state === 'object' ? Object.keys(state).sort() : [],
    stateBytes: Buffer.byteLength(JSON.stringify(state ?? null)),
    recentCount: Array.isArray(state?.recent) ? state.recent.length : null,
    configurationMenu: Object.values(request?.questions?.configuration?.criteria ?? {})
      .map(value => ({ model: value?.model, effort: value?.effort })),
    previousDecision, previousDecisionKeys: prior == null ? [] : Object.keys(prior).sort(),
    configurationRubricMatches: request?.questions?.configuration?.instructions === buildConfigurationInstructions(Object.values(request?.questions?.configuration?.criteria ?? {})),
    questionsHaveRubric: Object.values(request?.questions ?? {}).every(question => question.instructions?.includes(`Rubric ${RUBRIC_VERSION}.`)),
  });

  let response;
  if (real) {
    if (provider !== process.env.MINDPLAYER_TEST_JEV_PROVIDER) throw new Error('Jev fixture provider mismatch');
    response = await Reflect.apply(originalFetch, this, args);
  } else if (existsSync(join(markerDir, 'jev-rate-limited'))) {
    response = new Response('raw-upstream-secret-canary', { status: 429 });
  } else {
    const capability = ['A', 'C'].includes(label) ? 'bounded_routine' : label === 'B' ? 'open_problem' : 'multistep_coordination';
    const criteria = request?.questions?.configuration?.criteria;
    const entries = Object.entries(criteria ?? {});
    const chosen = entries.find(([, value]) => value.model === (label === 'B' ? 'gpt-6-astra' : 'gpt-6-luna')
      && value.effort === (label === 'B' ? 'high' : 'low'))?.[0];
    response = Response.json({
      model: 'jev-1.13.0',
      answers: {
        required_capability: { type: 'choice', choice: capability, confidence: 1,
          probabilities: Object.fromEntries(['bounded_routine', 'multistep_coordination', 'open_problem', 'unknown'].map(value => [value, Number(value === capability)])) },
        context_sufficient: { type: 'noul', noul: 1 },
        high_consequence: { type: 'noul', noul: 0 },
        ...(criteria && chosen ? { configuration: { type: 'choice', choice: chosen, confidence: 1,
          probabilities: Object.fromEntries(entries.map(([key]) => [key, Number(key === chosen)])) } } : {}),
      },
      usage: { input_tokens: 698, output_tokens: 101 },
    });
  }

  const metadata = { event: 'response', label, provider, status: response.status, model: null,
    capability: null, usage: null, contextSufficient: null, highConsequence: null, configuration: null };
  if (response.ok) {
    try {
      const body = await response.clone().json();
      const configurations = request?.questions?.configuration ? Object.values(request.questions.configuration.criteria) : undefined;
      const judgment = validateJudgment(body, { provider, configurations });
      Object.assign(metadata, { model: body.model, capability: judgment.capability, usage: judgment.usage,
        contextSufficient: judgment.contextSufficient, highConsequence: judgment.highConsequence,
        configuration: judgment.configuration ?? null });
    } catch { /* Do not record malformed upstream data or error bodies. */ }
  }
  record(metadata);
  return response;
};
}
