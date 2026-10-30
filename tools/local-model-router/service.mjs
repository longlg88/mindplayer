import { createHash, randomUUID } from 'node:crypto';
import { closeSync, fstatSync, openSync, readFileSync, writeFileSync, constants } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { createJevClient } from './jev.mjs';
import { buildSnapshot, filterConfigurations, chooseConfiguration } from './policy.mjs';
import { CONFIGURATION_ABSTAIN, RUBRIC_VERSION, buildJudgmentSnapshot } from './rubric.mjs';
import { snapshotFamilyRegistry, reviewFamilyRegistry } from './current-family.mjs';

export function childEnvironment(env) {
  const clean = { ...env };
  for (const name of ['AI_GATEWAY_API_KEY', 'TYPESAFE_API_KEY', 'VERCEL_OIDC_TOKEN', 'MINDPLAYER_ROUTER_AVAILABILITY', 'MINDPLAYER_ROUTER_STATUS',
    'MINDPLAYER_CLAUDE_JEV_URL', 'MINDPLAYER_CLAUDE_JEV_TOKEN', 'MINDPLAYER_PER_PROMPT_CHOICE', 'MINDPLAYER_JEV_DEFAULT']) delete clean[name];
  return clean;
}

function privateJson(path, maxBytes, invalidCode) {
  let fd;
  try {
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size > maxBytes || (stat.mode & 0o077) || (process.getuid && stat.uid !== process.getuid())) throw new Error();
    const body = readFileSync(fd, 'utf8');
    if (Buffer.byteLength(body) > maxBytes) throw new Error();
    return { value: JSON.parse(body), digest: createHash('sha256').update(body).digest('hex') };
  } catch (error) {
    if (invalidCode && error?.code !== 'ENOENT') throw fail(invalidCode);
    return null;
  }
  finally { if (fd !== undefined) closeSync(fd); }
}

export function loadPolicy(path = join(homedir(), '.mindplayer', 'jev-policy.json')) {
  const loaded = privateJson(path, 128 * 1024, 'policy-invalid');
  if (!loaded) return null;
  const policy = loaded.value;
  if (policy?.schemaVersion !== 1 || typeof policy.version !== 'string' || !policy.version.trim() || !Array.isArray(policy.profiles)) throw fail('policy-invalid');
  return { policy, digest: loaded.digest };
}

const credentialPath = () => join(homedir(), '.mindplayer', 'jev-credentials.json');
const validKey = key => typeof key === 'string' && key.length <= 8192 && /^[A-Za-z0-9._~+/-]+=*$/.test(key);

/** A dedicated key supplied to this service authorizes its documented bounded
 * prompt transfer. Never forward it to Codex or include it in an audit event. */
export function loadJevCredentials({ env = process.env, path = credentialPath() } = {}) {
  for (const [name, provider] of [['AI_GATEWAY_API_KEY', 'gateway'], ['TYPESAFE_API_KEY', 'typesafe']]) {
    if (validKey(env[name])) return { apiKey: env[name], provider, externalPromptTransfer: true };
  }
  const value = privateJson(path, 16 * 1024)?.value;
  return value?.schemaVersion === 1 && ['gateway', 'typesafe'].includes(value.provider)
    && value.externalPromptTransfer === true && value.consentVersion === 'jev-per-prompt-v1' && validKey(value.apiKey)
    ? { apiKey: value.apiKey, provider: value.provider, externalPromptTransfer: true } : null;
}

/** Explicit local setup only, not called automatically on session launch.
 * Exclusive create preserves existing credentials; the key never enters argv. */
export function provisionJevCredentials({ env = process.env, path = credentialPath() } = {}) {
  const credential = loadJevCredentials({ env, path: '' });
  if (!credential) throw fail('jev-key-unavailable');
  const fd = openSync(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  try { writeFileSync(fd, JSON.stringify({ schemaVersion: 1, consentVersion: 'jev-per-prompt-v1', ...credential })); }
  finally { closeSync(fd); }
  return { provider: credential.provider, stored: true };
}

export function loginFingerprint(account, accountHome) {
  // A workspace ID alone cannot distinguish two people in one Business seat.
  if (!account || !accountHome || typeof account.type !== 'string') return null;
  const person = account.type === 'chatgpt' && typeof account.email === 'string' ? account.email.trim().toLowerCase() : null;
  if (!person) return null; // API-key identities need an independently verified adapter.
  return createHash('sha256').update(JSON.stringify([accountHome, account.type, person])).digest('hex');
}

const fail = code => Object.assign(new Error(code), { code });
const integer = (value, min, max) => Number.isSafeInteger(value) && value >= min && value <= max;
const textItems = input => Array.isArray(input) ? input.filter(i => i?.type === 'text' && typeof i.text === 'string').map(i => i.text).join('\n') : '';
const knownText = input => Array.isArray(input) && input.every(i => i?.type === 'text' && typeof i.text === 'string');
const safeModel = value => typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$/.test(value);
const nativeEfforts = new Set(['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra']);
const sameConfiguration = (a, b) => a?.model === b?.model && a?.effort === b?.effort;
const instant = value => typeof value === 'number' ? value : typeof value === 'string' ? Date.parse(value) : NaN;

function catalogConfigurations(catalog, baseline, { accountFingerprint, modelProvider, now, input, requirements = {},
  baselineOnly = false, baselineError = 'baseline-ineligible', effortError = baselineError, criteriaReserveBytes = 0 }) {
  if (!Array.isArray(catalog) || catalog.length > 1000) throw fail('catalog-invalid');
  const strings = value => Array.isArray(value) && value.every(item => typeof item === 'string' && item.trim());
  if (!requirements || typeof requirements !== 'object' || Array.isArray(requirements)
    || (requirements.inputModalities !== undefined && (!strings(requirements.inputModalities) || !requirements.inputModalities.length))
    || (requirements.tools !== undefined && !strings(requirements.tools))
    || (requirements.contextTokens !== undefined && !integer(requirements.contextTokens, 0, Number.MAX_SAFE_INTEGER))) throw fail('requirements-invalid');
  if (requirements.ordinaryUsageAllowed === false || requirements.usageBlocked === true) throw fail('ordinary-usage-blocked');
  const modalities = ['text'];
  if (Array.isArray(input) && input.some(item => ['image', 'localImage'].includes(item?.type))) modalities.push('image');
  for (const modality of requirements.inputModalities ?? []) if (!modalities.includes(modality)) modalities.push(modality);
  const unique = new Map();
  for (const row of catalog) unique.set(row?.model, unique.has(row?.model) ? null : row);
  const live = [...unique.values()].filter(row => row && safeModel(row.model)
    && !row.hidden && !row.accessDenied && row.available !== false
    && !['denied', 'access_denied', 'blocked', 'unavailable'].includes(row.availability?.status ?? row.availability)
    && (row.accountFingerprint === undefined || row.accountFingerprint === accountFingerprint)
    && (modelProvider == null || (row.modelProvider ?? row.provider ?? modelProvider) === modelProvider)
    && Array.isArray(row.inputModalities) && modalities.every(modality => row.inputModalities.includes(modality))
    && (requirements.tools === undefined || (row.supportedTools ?? row.tools) === undefined
      || (strings(row.supportedTools ?? row.tools) && requirements.tools.every(tool => (row.supportedTools ?? row.tools).includes(tool))))
    && (requirements.contextTokens === undefined || row.contextWindow === undefined
      || (integer(row.contextWindow, 1, Number.MAX_SAFE_INTEGER) && row.contextWindow >= requirements.contextTokens))
    && Array.isArray(row.supportedReasoningEfforts)
    && [row.expiresAt, row.availability?.expiresAt].every(expiry => expiry === undefined || instant(expiry) > now)
    && (row.availability?.verifiedAt === undefined || instant(row.availability.verifiedAt) <= now));
  const supports = (row, effort) => effort === null
    ? row.supportedReasoningEfforts.length === 0 && row.defaultReasoningEffort === null
    : nativeEfforts.has(effort) && row.supportedReasoningEfforts.some(value => value?.reasoningEffort === effort);
  const base = live.find(row => row.model === baseline.model);
  if (!base) throw fail(baselineError);
  if (!supports(base, baseline.effort)) throw fail(effortError);
  const descriptor = (row, effort) => {
    const native = row.supportedReasoningEfforts.find(value => value?.reasoningEffort === effort);
    const text = [row.displayName, row.description, native?.description].filter(value => typeof value === 'string').join('\n');
    let description = '';
    for (const character of text) {
      if (Buffer.byteLength(description + character) > 1024) break;
      description += character;
    }
    return { model: row.model, effort, description };
  };
  const configurations = [descriptor(base, baseline.effort)];
  // Ultra explicitly permits delegation. Automatic selection cannot add or
  // remove that scope from a user-selected ultra baseline.
  if (baselineOnly || baseline.effort === 'ultra') return configurations;
  const add = configuration => {
    if (configurations.length >= 32 || configurations.some(value => sameConfiguration(value, configuration))) return;
    const criteria = Object.fromEntries([...configurations, configuration].map((value, index) => [`c${index}`, value]));
    if (Buffer.byteLength(JSON.stringify(criteria)) + criteriaReserveBytes <= 8 * 1024) configurations.push(configuration);
  };
  // Represent distinct models before additional efforts, so a model with many
  // efforts cannot consume the bounded menu. No ordering implies a ranking.
  const rows = [base, ...live.filter(row => row !== base)].map(row => {
    const efforts = [...nativeEfforts].filter(effort => effort !== 'ultra' && supports(row, effort));
    if (supports(row, null)) efforts.push(null);
    const preferred = row.defaultReasoningEffort;
    if (efforts.includes(preferred)) {
      efforts.splice(efforts.indexOf(preferred), 1);
      efforts.unshift(preferred);
    }
    return { row, efforts: efforts.filter(effort => row !== base || effort !== baseline.effort) };
  });
  for (let index = 0; index < nativeEfforts.size; index++) {
    for (const { row, efforts } of rows) if (index < efforts.length) add(descriptor(row, efforts[index]));
  }
  return configurations;
}

async function waitBounded(promise, milliseconds, signal) {
  const finished = new AbortController();
  const waiting = signal ? AbortSignal.any([signal, finished.signal]) : finished.signal;
  try { return await Promise.race([promise.then(() => true), delay(milliseconds, false, { signal: waiting })]); }
  finally { finished.abort(); }
}

/** A pane-local owner. No prompt, account email, or server error body is audited. */
export class JevService {
  constructor({ catalog, accountHome, account = null, baseline = null, availability = null, expectedThreadId = null, policyReader = loadPolicy,
    apiKey, provider = 'gateway', externalPromptTransfer = false, judgmentLimits = {}, latestFamily = null, requirePromptChoice = false,
    judgeFactory = createJevClient, requirementsForTurn = () => ({ provider: 'codex' }), audit = () => {}, now = Date.now } = {}) {
    Object.assign(this, { catalog, accountHome, baseline, availability, expectedThreadId, policyReader, apiKey, provider, externalPromptTransfer, judgmentLimits, judgeFactory, requirementsForTurn, audit, now, requirePromptChoice });
    this.catalog = structuredClone(catalog);
    Object.defineProperty(this, 'latestFamily', { enumerable: true,
      value: latestFamily === null ? null : snapshotFamilyRegistry(latestFamily) });
    this.catalogReady = true;
    this.catalogRefresh = null;
    this.accountFingerprint = loginFingerprint(account, accountHome);
    this.generation = randomUUID();
    this.revision = 0;
    this.catalogRevision = 0;
    this.threads = new Map();
    this.capacityBlocks = new Map();
    this.turnRequests = new WeakMap();
    this.lifecycleCount = 0;
    this.lifecycleEpoch = 0;
    this.lifecycleRequests = new WeakMap();
    this.lastCall = -Infinity;
    this.judgmentQueue = Promise.resolve();
    this.closed = false;
  }

  trackLifecycle(request) {
    if (['thread/start', 'thread/resume', 'thread/fork'].includes(request?.method)) {
      this.lifecycleRequests.set(request, this.lifecycleEpoch);
    }
  }

  capacityKey(thread, model) {
    return JSON.stringify([this.accountFingerprint, thread.modelProvider ?? null, model]);
  }

  trackSubmission(request) {
    const thread = this.threads.get(request.params?.threadId);
    if (thread?.owned) this.turnRequests.set(request, { thread, account: this.accountFingerprint });
  }

  capacityBlocked(thread, model) {
    const key = this.capacityKey(thread, model);
    const until = this.capacityBlocks.get(key);
    if (until === undefined) return false;
    if (until > this.now()) return true;
    this.capacityBlocks.delete(key);
    return false;
  }

  observeCapacity(thread, request, response) {
    if (!thread?.owned || !this.accountFingerprint) return;
    const ack = request?.method === 'turn/start' && !request.params?.toolOutput
      && Array.isArray(request.params?.input) && request.params.input.length && !response.error;
    const method = response.method;
    if (!ack && !['error', 'turn/completed', 'model/rerouted'].includes(method)) return;
    const turnId = ack ? response.result?.turn?.id : response.params?.turn?.id ?? response.params?.turnId;
    if (!safeModel(turnId)) return;
    thread.capacityTurns ??= new Map();
    let turn = thread.capacityTurns.get(turnId);
    if (!turn) {
      turn = { reroutes: [] };
      thread.capacityTurns.set(turnId, turn);
      // Metadata only; unacknowledged notifications cannot supply a model.
      if (thread.capacityTurns.size > 128) thread.capacityTurns.delete(thread.capacityTurns.keys().next().value);
    }
    if (ack) {
      const owner = this.turnRequests.get(request);
      this.turnRequests.delete(request);
      if (!owner || owner.thread !== thread || owner.account !== this.accountFingerprint) return;
      const model = request.params.collaborationMode?.settings?.model ?? request.params.model ?? thread.baseline.model;
      if (!safeModel(model)) return;
      if (turn.model !== undefined && turn.model !== model) turn.ambiguous = true;
      turn.model = model;
    } else if (method === 'model/rerouted') {
      if (turn.finished) return;
      const { fromModel, toModel } = response.params;
      const previous = turn.reroutes.at(-1);
      if (previous?.fromModel === fromModel && previous?.toModel === toModel) return;
      if (!safeModel(fromModel) || !safeModel(toModel) || turn.reroutes.length >= 8) turn.ambiguous = true;
      else turn.reroutes.push({ fromModel, toModel });
    } else if (method === 'error') {
      if (turn.finished || response.params.willRetry !== false) return;
      turn.overloaded = response.params.error?.codexErrorInfo === 'serverOverloaded';
    } else {
      if (turn.finished) return;
      turn.finished = true;
      turn.failed = response.params.turn.status === 'failed';
      turn.overloaded = response.params.turn.error?.codexErrorInfo !== undefined
        ? response.params.turn.error.codexErrorInfo === 'serverOverloaded' : turn.overloaded;
      turn.failedAt = this.now();
    }
    if (!turn.model || !turn.finished || !turn.failed || !turn.overloaded || turn.recorded || turn.ambiguous) return;
    let model = turn.model;
    for (const reroute of turn.reroutes) {
      if (reroute.fromModel !== model) return; // Never blame the requested model after an unexplained reroute.
      model = reroute.toModel;
    }
    turn.recorded = true;
    const until = turn.failedAt + 60_000;
    if (until <= this.now()) return;
    for (const [key, expiry] of this.capacityBlocks) if (expiry <= this.now()) this.capacityBlocks.delete(key);
    // Native catalogs are bounded to 1,000 models. Refuse extra attribution
    // rather than evicting a still-blocked model and silently selecting it.
    const key = this.capacityKey(thread, model);
    if (this.capacityBlocks.size >= 1000 && !this.capacityBlocks.has(key)) return;
    this.capacityBlocks.set(key, Math.max(this.capacityBlocks.get(key) ?? 0, until));
    this.revision++;
    this.audit({ event: 'model-capacity-blocked', threadId: response.params?.threadId ?? request.params.threadId,
      turnId, model, reason: 'server-overloaded', retryAfterMs: Math.max(0, until - this.now()) });
  }

  observe(request, response) {
    let lifecycleThreadId;
    // A close may precede the lifecycle ACK, before the thread is in our map.
    // Revoke in-flight bindings even for that notification-before-ACK order.
    if (response?.method === 'thread/closed') this.lifecycleEpoch++;
    if (request?.method === 'model/list') {
      // Replace only a complete, contiguous catalog refresh. Partial pages must
      // not silently retain a removed model or omit a newly entitled one.
      this.revision++; this.catalogRevision++; this.catalogReady = false;
      const cursor = request.params?.cursor ?? null;
      if (cursor === null) this.catalogRefresh = { cursor: null, rows: [] };
      const page = response.result;
      if (response.error || !this.catalogRefresh || cursor !== this.catalogRefresh.cursor || !Array.isArray(page?.data) ||
          page.data.some(row => !row || typeof row.model !== 'string') || this.catalogRefresh.rows.length + page.data.length > 1000 ||
          !(page.nextCursor == null || typeof page.nextCursor === 'string')) {
        this.catalogRefresh = null;
      } else {
        this.catalogRefresh.rows.push(...structuredClone(page.data));
        this.catalogRefresh.cursor = page.nextCursor ?? null;
        if (page.nextCursor == null) {
          this.catalog = this.catalogRefresh.rows; this.catalogReady = true; this.catalogRefresh = null;
        }
      }
    }
    if (request?.method === 'turn/start' && response.error) {
      // An RPC rejection can be transient or input-related. Only explicit
      // catalog access metadata establishes exclusion; never parse raw errors
      // into a durable permission decision or automatically replay this turn.
      this.revision++;
    }
    if (request && !response.error && ['thread/start', 'thread/resume', 'thread/fork'].includes(request.method)) {
      const epoch = this.lifecycleRequests.get(request);
      this.lifecycleRequests.delete(request);
      if (epoch !== this.lifecycleEpoch) {
        this.audit({ event: 'thread-ownership-stale', reason: 'stale-lifecycle' });
        return;
      }
      const firstLifecycle = this.lifecycleCount++ === 0;
      const thread = response.result?.thread;
      if (typeof thread?.id !== 'string') return;
      const helper = request.params?.ephemeral === true || thread.ephemeral === true;
      const userSource = thread.source === 'cli' || thread.source === 'vscode' || thread.source === 'appServer';
      const excluded = helper || thread.parentThreadId != null || (thread.threadSource != null && thread.threadSource !== 'user') || thread.source === 'subAgent' ||
        (thread.source && typeof thread.source === 'object' && Object.hasOwn(thread.source, 'subAgent'));
      const prior = this.threads.get(thread.id);
      // Ownership comes from the backend lifecycle, never a prompt prefix/title.
      // Native 0.162.1 --remote sends null, not "startup". Its first launch
      // was measured with zero inference: explicit persistent user request +
      // backend codex-tui/vscode origin. Do not promote later helper threads.
      const observedNativeStartup = firstLifecycle && this.expectedThreadId === null && request.params?.ephemeral === false &&
        request.params?.sessionStartSource == null && request.params?.threadSource === 'user' &&
        thread.source === 'vscode' && thread.originator === 'codex-tui';
      const selected = request.params?.mindplayerUserSelection === true && thread.threadSource === 'user' &&
        thread.source === 'vscode' && thread.originator === 'codex-tui';
      const startup = request.method === 'thread/start' && (['startup', 'clear'].includes(request.params?.sessionStartSource) || observedNativeStartup || selected);
      // A resume picker can change the pane's main thread after startup. The
      // private UI marks only that user action; background/helper resume calls
      // do not carry the local marker (the router strips it before forwarding).
      const resumed = request.method === 'thread/resume' && request.params?.threadId === thread.id &&
        ((firstLifecycle && thread.id === this.expectedThreadId) || prior?.owned === true || selected);
      const forked = request.method === 'thread/fork' && selected && this.threads.get(request.params?.threadId)?.owned === true;
      const owned = !excluded && userSource && (startup || resumed || forked);
      // Native /clear uses this source even when the private UI also marks a
      // user selection. Selection alone cannot establish clear continuity.
      const clear = owned && request.method === 'thread/start' && request.params?.sessionStartSource === 'clear';
      this.audit({ event: 'thread-ownership', threadId: thread.id, owned, ephemeral: helper,
        startup, clear, resumed, forked, userSource, policyVersion: 'mindplayer-private-ui-lifecycle-v3' });
      this.threads.set(thread.id, { owned, excluded, history: [], contextKnown: request.method === 'thread/start', version: (prior?.version ?? 0) + 1,
        resumeContextPending: request.method === 'thread/resume',
        modelProvider: response.result.modelProvider ?? prior?.modelProvider ?? null,
        baseline: { model: response.result.model ?? request.params?.model ?? this.baseline?.model,
          effort: response.result.reasoningEffort !== undefined ? response.result.reasoningEffort : request.params?.reasoningEffort !== undefined ? request.params.reasoningEffort : this.baseline?.effort } });
      lifecycleThreadId = thread.id;
      if (owned || prior?.owned) this.revision++;
    }
    if (request?.method === 'account/read' && !response.error) {
      const fingerprint = loginFingerprint(response.result?.account, this.accountHome);
      if (fingerprint !== this.accountFingerprint) { this.accountFingerprint = fingerprint; this.invalidate(); }
    }
    if (['account/updated', 'account/login/completed'].includes(response.method)) {
      this.accountFingerprint = null;
      this.invalidate();
    }
    const thread = this.threads.get(response.params?.threadId ?? request?.params?.threadId);
    this.observeCapacity(thread, request, response);
    if (thread?.owned && response.method === 'item/completed') {
      const item = response.params.item;
      if (item?.type === 'userMessage') {
        if (!knownText(item.content)) thread.contextKnown = false;
        this.addHistory(thread, { role: 'user', text: textItems(item.content), ...(!knownText(item.content) ? { attachments: true } : {}) });
      }
      if (item?.type === 'agentMessage' && item.phase === 'final_answer') this.addHistory(thread, { role: 'assistant', text: item.text, final: true });
    }
    if (thread?.owned && ['thread/compacted', 'thread/closed'].includes(response.method)) {
      thread.contextKnown = false; thread.version++; this.revision++;
      delete thread.pendingDecision;
      if (response.method === 'thread/closed') { thread.owned = false; delete thread.previousDecision; }
    }
    if (thread?.owned && response.method === 'turn/completed' && typeof response.params.turn?.id === 'string') {
      const turn = response.params.turn;
      // A native turn outcome is not proof that the user's goal is complete.
      // Remember one outcome for notification-before-ACK ordering.
      const turnOutcome = ['completed', 'interrupted', 'failed'].includes(turn.status) ? turn.status : 'unknown';
      thread.lastCompletedTurn = { id: turn.id, turnOutcome };
      if (thread.previousDecision?.turnId === turn.id) {
        thread.previousDecision.summary.turnOutcome = turnOutcome;
        thread.version++; this.revision++;
      }
    }
    if (thread && request?.method === 'turn/start' && !response.error && response.result?.turn?.id) {
      thread.baseline = { model: request.params.model ?? thread.baseline.model,
        effort: request.params.effort !== undefined ? request.params.effort : thread.baseline.effort };
    }
    if (thread && request?.method === 'turn/start' && thread.pendingDecision?.requestId === request.id) {
      const pending = thread.pendingDecision;
      delete thread.pendingDecision;
      if (thread.owned && !response.error && typeof response.result?.turn?.id === 'string'
        && sameConfiguration(pending.summary, thread.baseline)) {
        const turnId = response.result.turn.id;
        thread.previousDecision = { ...pending, turnId,
          summary: { ...pending.summary, turnOutcome: thread.lastCompletedTurn?.id === turnId ? thread.lastCompletedTurn.turnOutcome : 'unknown' } };
      }
    }
    // Router tombstones must use this exact ACK's freshness result, not a
    // thread's prior ownership (which could outlive an unrelated stale ACK).
    return lifecycleThreadId === undefined ? undefined : { lifecycleThreadId };
  }

  async readContext(threadId) {
    const thread = this.threads.get(threadId);
    const revision = this.revision;
    const contextReadVersion = thread.contextReadVersion = (thread.contextReadVersion ?? 0) + 1;
    try {
      const page = await this.rpc('thread/turns/list', { threadId, limit: 4, sortDirection: 'desc', itemsView: 'full' });
      if (this.closed || this.revision !== revision || this.threads.get(threadId) !== thread || thread.contextReadVersion !== contextReadVersion || !Array.isArray(page.data) || page.data.length > 4) return;
      thread.history = [];
      thread.contextKnown = !page.nextCursor && page.data.every(t => t.itemsView === 'full' && Array.isArray(t.items));
      for (const turn of [...page.data].reverse()) for (const item of turn.items ?? []) {
        if (item.type === 'userMessage') {
          if (!knownText(item.content)) thread.contextKnown = false;
          this.addHistory(thread, { role: 'user', text: textItems(item.content), ...(!knownText(item.content) ? { attachments: true } : {}) });
        }
        if (item.type === 'agentMessage' && item.phase === 'final_answer') this.addHistory(thread, { role: 'assistant', text: item.text, final: true });
      }
      thread.version++; this.revision++;
    } catch { /* unsupported pagination/network is unknown context, not empty */ }
  }

  addHistory(thread, message) {
    if (typeof message.text !== 'string') return;
    // Bound storage as well as the request snapshot. Tool/reasoning items never enter.
    if (Buffer.byteLength(message.text) > 16 * 1024) { thread.contextKnown = false; thread.history = []; }
    else { thread.history.push(message); if (thread.history.length > 8) { thread.history.splice(0, thread.history.length - 8); thread.contextKnown = false; } }
    thread.version++; this.revision++;
  }

  invalidate() {
    this.revision++; this.lifecycleEpoch++;
    this.capacityBlocks.clear();
    for (const thread of this.threads.values()) { thread.owned = false; delete thread.previousDecision; delete thread.pendingDecision; }
  }
  close() { this.closed = true; this.invalidate(); this.threads.clear(); }

  cancelDecision(request) {
    const thread = this.threads.get(request.params.threadId);
    if (thread?.pendingDecision?.requestId === request.id) delete thread.pendingDecision;
  }

  transferAllowed(policy) {
    return policy?.consent?.externalPromptTransfer !== false &&
      (this.externalPromptTransfer === true || (policy?.consent?.externalPromptTransfer === true && !!policy.consent.version));
  }

  async decide(request, { signal } = {}) {
    const thread = this.threads.get(request.params.threadId);
    const skip = reason => ({ passthrough: true, reason });
    if (!thread?.owned) {
      if (thread?.excluded) return skip('non-user-thread');
      // Missing ownership is not permission to bypass per-prompt judgment.
      // Fail before inference/submission; only verified helpers may bypass.
      throw fail('main-ownership-unverified');
    }
    if (this.requirePromptChoice) {
      const choice = request.params.mindplayerRoutingChoice;
      if (!choice || typeof choice !== 'object' || Array.isArray(choice)
        || !Object.keys(choice).every(key => ['mode', 'includeRecent'].includes(key))
        || choice.includeRecent !== false
        || !['jev', 'native'].includes(choice.mode)) throw fail('prompt-choice-required');
      // The native session copies its current toggle onto each message. OFF
      // must not inspect credentials/policy or rewind the native configuration.
      if (choice.mode === 'native') return skip('prompt-native');
    }
    const availability = this.availability;
    if (availability?.source === 'footer-live' && availability.codexHome === this.accountHome &&
        integer(availability.observedAt, 0, this.now()) && this.now() - availability.observedAt <= 60_000 && availability.blocked === true) throw fail('ordinary-usage-blocked');
    const loaded = this.policyReader(), policy = loaded?.policy;
    const transferUnset = () => {
      if (this.latestFamily) throw fail('external-transfer-policy-unset');
      return skip('external-transfer-policy-unset');
    };
    if (policy?.consent?.externalPromptTransfer === false) return transferUnset();
    if (!this.transferAllowed(policy) && this.apiKey) return transferUnset();
    if (!this.catalogReady) throw fail('catalog-refresh-incomplete');
    // Opening an OFF session must not read Jev configuration or history. Only
    // an authorized ON turn with explicit empirical policy needs hydration.
    if (thread.resumeContextPending && policy && this.rpc && this.apiKey && this.transferAllowed(policy)) {
      thread.resumeContextPending = false;
      thread.contextRead = this.readContext(request.params.threadId);
    }
    if (thread.contextRead) {
      try { await waitBounded(thread.contextRead, 1000, signal); }
      finally { thread.contextReadVersion = (thread.contextReadVersion ?? 0) + 1; delete thread.contextRead; }
    }
    if (!thread.owned || this.closed || signal?.aborted) throw fail('stale-decision');
    const settings = request.params.collaborationMode?.settings;
    const baseline = { model: settings?.model ?? request.params.model ?? thread.baseline.model,
      effort: settings && Object.hasOwn(settings, 'reasoning_effort') ? settings.reasoning_effort
        : Object.hasOwn(request.params, 'effort') ? request.params.effort : thread.baseline.effort };
    if (!this.accountFingerprint) throw fail('login-unverified');
    const requirements = this.requirementsForTurn(request, { contextVersion: thread.version });
    const catalogOptions = { accountFingerprint: this.accountFingerprint, modelProvider: thread.modelProvider, input: request.params.input, requirements };
    // Validate the exact incoming configuration against the full native catalog
    // before the registry can recognize or migrate it. A denied/unknown model
    // cannot acquire eligibility by borrowing the current anchor.
    if (this.latestFamily) catalogConfigurations(this.catalog, baseline, { ...catalogOptions, now: this.now(), baselineOnly: true });
    const family = this.latestFamily ? reviewFamilyRegistry(this.latestFamily, { catalog: this.catalog, now: this.now() }) : null;
    const baselineOnly = family?.baselineOnlyModels.includes(baseline.model) ?? false;
    const migrating = family && !baselineOnly && !family.currentModels.includes(baseline.model);
    if (migrating && !family.legacyModels.includes(baseline.model)) throw fail('registry-review-required');
    if (migrating && policy) throw fail('latest-family-policy-conflict');
    const fallback = migrating ? { model: family.frontier, effort: baseline.effort } : baseline;
    const capacityRecovery = this.capacityBlocked(thread, fallback.model);
    // An unranked reviewed model is eligible only when the user selected it;
    // never offer it as a general alternative or migrate other models to it.
    const allowed = model => family.currentModels.includes(model) || (baselineOnly && model === baseline.model);
    const familyCatalog = () => family ? this.catalog.filter(row => allowed(row?.model)) : this.catalog;
    const nativeEligible = value => {
      if (this.capacityBlocked(thread, value.model)) return false;
      if (!family) return true;
      if (!allowed(value.model) || (baseline.effort === 'ultra'
        ? !sameConfiguration(value, fallback) : value.effort === 'ultra')) return false;
      try { catalogConfigurations(familyCatalog(), value, { ...catalogOptions, now: this.now(), baselineOnly: true }); return true; }
      catch { return false; }
    };
    const candidates = policy ? filterConfigurations({ catalog: familyCatalog(), profiles: policy.profiles, accountFingerprint: this.accountFingerprint, now: this.now(),
      // Deployment JSON cannot establish this turn's full context/tool needs.
      // No verified native token-count adapter exists yet: leave unknown fields
      // omitted, never estimate them from prompt length or prior-turn usage.
      requirements }).filter(nativeEligible) : null;
    const choiceOptions = { ...catalogOptions,
      // Reserve the exact extra JSON member before filling the bounded menu.
      criteriaReserveBytes: capacityRecovery ? Buffer.byteLength(`,"abstain":${JSON.stringify(CONFIGURATION_ABSTAIN)}`) : 0,
      ...(migrating ? { baselineError: 'latest-family-anchor-unavailable', effortError: 'latest-family-effort-unsupported' } : {}) };
    const configurations = policy ? undefined : Object.freeze(catalogConfigurations(familyCatalog(), fallback, { ...choiceOptions, now: this.now() })
      .filter(value => !this.capacityBlocked(thread, value.model)).map(Object.freeze));
    // Empirical comparisons require their exact baseline. Do not replace that
    // evidence with a heuristic when its baseline is temporarily blocked.
    if ((policy && capacityRecovery) || (configurations && !configurations.length)) throw fail('capacity-no-safe-alternative');
    const binding = Object.freeze({ generation: this.generation, accountFingerprint: this.accountFingerprint,
      catalogRevision: this.catalogRevision, policyDigest: loaded?.digest ?? null, rubricVersion: RUBRIC_VERSION,
      ...(family ? { registryVersion: family.version, registryDigest: family.digest } : {}) });
    const prior = thread.previousDecision;
    const priorIsCurrent = prior && Object.keys(binding).every(key => prior.binding[key] === binding[key])
      && configurations?.some(value => sameConfiguration(value, prior.summary));
    const context = configurations
      ? buildJudgmentSnapshot({ input: request.params.input, baseline, previousDecision: priorIsCurrent ? prior.summary : null,
        currentFamily: family ? { version: family.version, digest: family.digest } : null })
      : buildSnapshot({ input: request.params.input, history: thread.history, baseline, contextKnown: thread.contextKnown });
    if (!this.apiKey) {
      this.audit({ event: 'jev-failed', threadId: request.params.threadId, requestId: request.id, provider: this.provider, reason: 'jev-key-unavailable', latencyMs: 0 });
      throw fail('jev-key-unavailable');
    }
    if (!this.transferAllowed(policy)) return transferUnset();
    const revision = this.revision;
    const current = () => !this.closed && !signal?.aborted && thread.owned && this.threads.get(request.params.threadId) === thread
      && revision === this.revision && this.catalogRevision === binding.catalogRevision
      && this.accountFingerprint === binding.accountFingerprint && this.policyReader()?.digest === loaded?.digest;
    const select = judgment => {
      if (family) {
        const reviewed = reviewFamilyRegistry(this.latestFamily, { catalog: this.catalog, now: this.now() });
        if (reviewed.version !== binding.registryVersion || reviewed.digest !== binding.registryDigest) throw fail('registry-review-required');
      }
      const guarded = selected => {
        if (family && !allowed(selected.model)) throw fail('latest-family-selection-invalid');
        if (!nativeEligible(selected)) throw fail('selected-configuration-unavailable');
        return selected;
      };
      if (candidates) return guarded(chooseConfiguration({ candidates, baseline, judgment, thresholds: policy.thresholds, context, now: this.now() }));
      const live = catalogConfigurations(familyCatalog(), fallback, { ...choiceOptions, now: this.now() });
      const keep = reason => {
        if (capacityRecovery) throw fail('capacity-no-safe-alternative');
        return guarded({ ...fallback, changed: !sameConfiguration(fallback, baseline), reason, executionCostUsd: null });
      };
      if (!judgment) return capacityRecovery ? null : keep('selection-unavailable');
      // No confidence cutoff: explicit abstention or a certain missing-context
      // answer cannot authorize switching away from an unavailable fallback.
      if (capacityRecovery && (judgment.configuration?.choice === 'abstain' || judgment.contextSufficient === 0)) throw fail('capacity-no-safe-alternative');
      const selected = configurations.find((value, index) => sameConfiguration(value, judgment.configuration) && judgment.configuration?.choice === `c${index}`);
      if (!selected) throw fail('invalid-response');
      if (!live.some(value => sameConfiguration(value, selected))) throw fail('selected-configuration-unavailable');
      if (context.truncated || context.attachments) return keep('context-incomplete');
      if (judgment.capability === 'unknown') return keep('capability-unknown');
      return guarded({ model: selected.model, effort: selected.effort, changed: !sameConfiguration(selected, baseline), reason: 'catalog-guided-jev', executionCostUsd: null });
    };
    select(null); // Enforce explicit empirical policy even when comparison evidence is incomplete.
    const limits = this.judgmentLimits;
    // Allow normal gateway latency without approaching the native route's 31s hard deadline.
    const timeoutMs = integer(limits.timeoutMs, 1, 30_000) ? limits.timeoutMs : 15_000;
    const cooldownMs = integer(limits.cooldownMs, 0, 5000) ? limits.cooldownMs : 250;
    const maxRequestBytes = integer(limits.maxRequestBytes, 24_576, 32_768) ? limits.maxRequestBytes : 32_768;
    const maxResponseBytes = integer(limits.maxResponseBytes, 1024, 65_536) ? limits.maxResponseBytes : 65_536;
    const owner = JSON.stringify([this.generation, randomUUID(), this.accountFingerprint, request.params.threadId, typeof request.id, request.id,
      thread.version, this.catalogRevision, RUBRIC_VERSION, loaded?.digest ?? null, binding.registryVersion ?? null, binding.registryDigest ?? null]);
    let started = this.now();
    const auditId = { threadId: request.params.threadId, requestId: request.id, provider: this.provider };
    let judgment;
    const previous = this.judgmentQueue;
    let release;
    this.judgmentQueue = new Promise(resolve => release = resolve);
    try {
      if (!await waitBounded(previous, 10_000, signal)) throw fail('timeout');
      const waitMs = Math.max(0, cooldownMs - (this.now() - this.lastCall));
      if (waitMs) await delay(waitMs, undefined, { signal });
      if (!current()) throw fail('stale-decision');
      started = this.now(); this.lastCall = started;
      this.audit({ event: 'jev-started', ...auditId, rubricVersion: RUBRIC_VERSION,
        catalogRevision: this.catalogRevision, previousDecisionKnown: context.previousDecision != null });
      const judge = this.judgeFactory({ apiKey: this.apiKey, provider: this.provider, timeoutMs, maxRequestBytes, maxResponseBytes,
        ...(configurations ? { configurations, capacityRecovery } : {}) });
      judgment = await judge(context, { signal });
    }
    catch (error) {
      if (!current()) throw fail('stale-decision');
      const failureCodes = new Set(['unauthorized', 'payment-required', 'rate-limited', 'http-error', 'tls-error', 'network-error', 'timeout', 'aborted', 'invalid-response', 'request-too-large', 'response-too-large', 'invalid-config', 'invalid-request']);
      const reason = failureCodes.has(error?.code) ? error.code : 'jev-unavailable';
      this.audit({ event: 'jev-failed', ...auditId, reason, latencyMs: this.now() - started });
      throw fail(reason);
    }
    finally { void previous.then(release, release); }
    if (!current()) throw fail('stale-decision');
    this.audit({ event: 'jev-judged', ...auditId, capability: judgment.capability, confidence: judgment.confidence,
      contextSufficient: judgment.contextSufficient, highConsequence: judgment.highConsequence, usage: judgment.usage,
      gatewayCostUsd: judgment.gatewayCostUsd, latencyMs: this.now() - started, policyVersion: policy?.version ?? null, rubricVersion: RUBRIC_VERSION,
      executionBilling: 'subscription', executionCostUsd: null });
    const selected = select(judgment);
    this.audit({ event: 'model-selected', ...auditId, model: selected.model, effort: selected.effort, changed: selected.changed, reason: selected.reason, executionCostUsd: null });
    if (!current()) throw fail('stale-decision');
    thread.pendingDecision = { requestId: request.id, binding,
      summary: { model: selected.model, effort: selected.effort, capability: judgment.capability,
        contextSufficient: judgment.contextSufficient, highConsequence: judgment.highConsequence, turnOutcome: 'unknown' } };
    return { ...selected, owner, isCurrent: () => {
      if (!current()) return false;
      try { return sameConfiguration(select(judgment), selected); } catch { return false; }
    } };
  }
}
