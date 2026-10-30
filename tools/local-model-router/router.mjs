const idKey = id => `${typeof id}:${id}`;
const errorReply = (id, message, code = -32002) => ({ id, error: { code, message } });
const JEV_FAILURES = Object.freeze({
  'prompt-choice-required': ['prompt-choice-required', 'Jev 라우팅 정보가 올바르지 않습니다. 아무 요청도 전송하지 않았습니다.'],
  'jev-key-unavailable': ['jev-key-unavailable', 'Jev credential is unavailable; nothing was sent'],
  'unauthorized': ['jev-unauthorized', 'Jev authentication failed; nothing was sent. Check the configured credential'],
  'payment-required': ['jev-payment-required', 'Jev billing authorization failed; nothing was sent'],
  'rate-limited': ['jev-rate-limited', 'Jev rate limit reached; nothing was sent. No automatic retry'],
  'http-error': ['jev-http-error', 'Jev service returned an HTTP error; nothing was sent. No automatic retry'],
  'tls-error': ['jev-tls-error', 'Jev TLS certificate verification failed; nothing was sent. Check system certificate trust'],
  'network-error': ['jev-network-error', 'Jev network request failed; nothing was sent. No automatic retry'],
  'timeout': ['jev-timeout', 'Jev judgment timed out; nothing was sent. No automatic retry'],
  'invalid-response': ['jev-invalid-response', 'Jev response validation failed; nothing was sent. No automatic retry'],
  'request-too-large': ['jev-request-too-large', 'Jev judgment request exceeded its size limit; nothing was sent'],
  'response-too-large': ['jev-response-too-large', 'Jev judgment response exceeded its size limit; nothing was sent'],
  'main-ownership-unverified': ['main-ownership-unverified', 'Main thread ownership is unverified; nothing was sent. Reopen the session from MindPlayer'],
  'registry-review-required': ['registry-review-required', 'Jev model registry requires review; nothing was sent. Turn F6 OFF to use the selected Codex model and submit again manually'],
  'latest-family-anchor-unavailable': ['latest-family-anchor-unavailable', 'Current model-family migration anchor is unavailable for this account; nothing was sent'],
  'latest-family-effort-unsupported': ['latest-family-effort-unsupported', 'Current model-family migration does not support the exact incoming effort; nothing was sent'],
  'latest-family-policy-conflict': ['latest-family-policy-conflict', 'Current model-family restriction conflicts with the empirical routing policy; nothing was sent'],
  'latest-family-selection-invalid': ['latest-family-selection-invalid', 'Selected configuration is outside the reviewed current model family; nothing was sent'],
  'baseline-ineligible': ['baseline-ineligible', 'Incoming model and effort are not eligible in the native catalog; nothing was sent'],
  'capacity-no-safe-alternative': ['capacity-no-safe-alternative', '모델이 일시적으로 혼잡하며 적합한 대체 모델을 확인하지 못했습니다. 요청은 전송하지 않았습니다. 잠시 후 다시 시도하세요.'],
  'external-transfer-policy-unset': ['external-transfer-policy-unset', 'Jev external prompt transfer is not authorized by the current policy; nothing was sent'],
});

export function chooseModels(catalog, { simpleModel, complexModel } = {}) {
  const visible = catalog.filter(m => !m.hidden && (!m.inputModalities || m.inputModalities.includes('text')));
  const find = name => visible.find(m => m.model === name || m.id === name);
  const simple = simpleModel ? find(simpleModel) : visible.find(m => m.isDefault) ?? visible[0];
  const complex = complexModel ? find(complexModel) : visible.find(m => m.model !== simple?.model);
  if (!simple || !complex || simple.model === complex.model) {
    throw new Error('서로 다른 텍스트 모델 두 개가 필요합니다. --list-models로 후보를 확인하세요.');
  }
  return { simple, complex };
}

// Deliberately not Jev, a quality ranking, or an entitlement check.
export function ruleDecision(input, pair) {
  const text = input.filter(i => i.type === 'text').map(i => i.text).join('\n');
  const complex = text.length > 400 || /설계|분석|원인|비교|리팩터|architecture|analy[sz]e|debug|compare|refactor/i.test(text);
  return { model: (complex ? pair.complex : pair.simple).model, reason: complex ? 'test-rule-complex' : 'test-rule-simple' };
}

export class ModelRouter {
  constructor({ catalog, pair, toServer, toClient, audit = () => {}, decide = ruleDecision, decisionTimeoutMs = 1000, service, requireFreshTurn = false }) {
    Object.assign(this, { catalog, pair, toServer, toClient, audit, decide, decisionTimeoutMs, service, requireFreshTurn });
    this.pending = new Map();
    this.requests = new Map();
    this.seen = new Set();
    this.active = new Map();
    this.closedThreads = new Set();
    this.activity = new Map();
    this.closed = false;
    this.internalPrefix = `mindplayer-internal-${randomUUID()}-`;
    this.internal = new Map();
    if (this.service) this.service.rpc = (method, params) => this.internalRequest(method, params);
  }

  internalRequest(method, params) {
    const id = this.internalPrefix + randomUUID();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.internal.delete(id); reject(new Error('Context read timed out')); }, 1000);
      this.internal.set(id, { resolve, reject, timer });
      try { this.toServer({ id, method, params }); }
      catch { clearTimeout(timer); this.internal.delete(id); reject(new Error('Context read failed')); }
    });
  }

  record(event) { try { this.audit(event); } catch { /* audit must not resend a turn */ } }
  bumpActivity(threadId) { this.activity.set(threadId, (this.activity.get(threadId) ?? 0) + 1); }
  cancelPending(threadId, reason) {
    const state = this.pending.get(threadId);
    if (!state || !['waiting', 'deciding'].includes(state.phase)) return false;
    this.pending.delete(threadId);
    this.service?.cancelDecision?.(state.request);
    state.abort.abort();
    const cancelled = reason === 'user-cancelled';
    this.record({ event: cancelled ? 'route-cancelled' : 'route-failed', threadId, requestId: state.request.id, reason });
    this.reject(state.request.id, cancelled ? 'Model routing cancelled before submission; nothing was sent'
      : 'Routing context changed; nothing was sent', cancelled ? -32800 : -32002);
    return true;
  }
  reject(id, reason, code = -32002) {
    this.seen.delete(idKey(id));
    this.toClient(errorReply(id, reason, code));
  }
  flushInterrupts(state, turnId) {
    for (const interrupt of state.interrupts.splice(0)) {
      if (!turnId || interrupt.params.turnId !== turnId) {
        this.reject(interrupt.id, 'Interrupt target is not the acknowledged turn; nothing was interrupted');
      } else {
        this.requests.set(idKey(interrupt.id), interrupt);
        this.toServer(interrupt);
      }
    }
  }
  client(message) {
    if (this.closed) return;
    if (!message || typeof message !== 'object' || Array.isArray(message)) throw new Error('Invalid RPC object');
    // Replies to server-initiated requests have no method. IDs are direction-local.
    if (!message.method) { this.toServer(message); return; }
    if (message.id !== undefined) {
      if (typeof message.id === 'string' && message.id.startsWith(this.internalPrefix)) throw new Error('Reserved RPC ID');
      const key = idKey(message.id);
      if (this.seen.has(key) || this.seen.size >= 10_000) throw new Error('Duplicate/excessive client request ID');
      this.seen.add(key);
    }
    const thread = message.params?.threadId;
    if (message.method === 'mindplayer/turn/cancel') {
      if (message.id === undefined) throw new Error('Local cancellation requires an RPC ID');
      const start = this.pending.get(thread);
      const target = message.params;
      const clientId = start?.request.params.clientUserMessageId;
      // The private UI knows its own message identity before a real turn exists.
      // A stale cancellation must not target a later attempt reusing an RPC ID.
      const matches = start && (clientId != null
        ? typeof target.clientUserMessageId === 'string' && target.clientUserMessageId === clientId
        : target.requestId !== undefined && target.requestId === start.request.id)
        && (target.clientUserMessageId === undefined || target.clientUserMessageId === clientId)
        && (target.requestId === undefined || target.requestId === start.request.id);
      const cancelled = !!matches && this.cancelPending(thread, 'user-cancelled');
      this.seen.delete(idKey(message.id));
      this.toClient({ id: message.id, result: cancelled ? { cancelled: true, requestId: start.request.id } : { cancelled: false } });
      return;
    }
    // Retired/unknown local methods must never escape to an ordinary backend.
    if (typeof message.method === 'string' && message.method.startsWith('mindplayer/')) {
      if (message.id !== undefined) this.reject(message.id, 'Unsupported local method; nothing was sent', -32601);
      return;
    }
    if (message.method === 'turn/interrupt') {
      const start = this.pending.get(thread);
      if (start?.phase === 'deciding') {
        // turn/interrupt identifies a real turn, not a pending JSON-RPC request.
        // Do not cancel a different prompt or invent an acknowledgement.
        this.reject(message.id, 'No real turn exists yet; interrupt requires its actual turn ID');
        return;
      }
      if (start?.phase === 'sent' && !this.active.has(thread)) {
        // Hold the original target until it can be checked; never retarget it.
        start.interrupts.push(message);
        return;
      }
    }
    if (message.method === 'turn/start' && Array.isArray(message.params?.input) && message.params.input.length && !message.params.toolOutput) {
      if (!thread || message.id === undefined) throw new Error('turn/start requires threadId and id');
      if (this.pending.has(thread)) {
        this.reject(message.id, 'A turn submission is already pending; no duplicate sent');
        return;
      }
      if (this.requireFreshTurn && this.active.has(thread) && this.service?.threads?.get(thread)?.excluded !== true) {
        // The private composer owns the queue. Never hold its RPC/event loop
        // or release a collided prompt automatically after failure/interruption.
        this.reject(message.id, 'A turn is already active; nothing was sent. Recover the queued input explicitly');
        this.record({ event: 'route-failed', threadId: thread, requestId: message.id, reason: 'new-turn-active' });
        return;
      }
      // A turn/start is a new user turn, unlike turn/steer. Hold it until the
      // current turn completes in the standalone experiment only. Integrated
      // private main input must never enter this proxy waiting queue.
      // Tool output and native steering still bypass model selection.
      void this.route(message);
      return;
    }
    if (message.id !== undefined) {
      this.service?.trackLifecycle?.(message);
      this.requests.set(idKey(message.id), message);
    }
    // This private UI provenance extension is local to the authenticated pane.
    // Keep it for lifecycle binding, never send it to an ordinary Codex server.
    if (['thread/start', 'thread/resume', 'thread/fork'].includes(message.method) && message.params && Object.hasOwn(message.params, 'mindplayerUserSelection')) {
      const params = { ...message.params }; delete params.mindplayerUserSelection;
      this.toServer({ ...message, params });
    } else this.toServer(message);
  }

  async route(request) {
    const threadId = request.params.threadId;
    const state = { request, phase: this.active.has(threadId) ? 'waiting' : 'deciding', abort: new AbortController(), interrupts: [] };
    this.pending.set(threadId, state);
    let timer;
    let failure = 'selector-error';
    try {
      if (state.phase === 'waiting') {
        this.record({ event: 'route-queued', threadId, requestId: request.id });
        await new Promise((resolve, reject) => {
          state.ready = resolve;
          state.abort.signal.addEventListener('abort', () => reject(new Error('Cancelled')), { once: true });
        });
      }
      if (this.closed || state.abort.signal.aborted) return;
      state.phase = 'deciding';
      const activity = this.activity.get(threadId) ?? 0;
      this.record({ event: 'route-deciding', threadId, requestId: request.id });
      const decision = await Promise.race([
        Promise.resolve().then(() => {
          if (state.abort.signal.aborted) throw new Error('Cancelled');
          return this.service ? this.service.decide(request, { signal: state.abort.signal }) : this.decide(request.params.input, this.pair, state.abort.signal);
        }),
        new Promise((_, reject) => {
          timer = setTimeout(() => { failure = 'selector-timeout'; reject(new Error('Decision timeout')); }, this.decisionTimeoutMs);
          state.abort.signal.addEventListener('abort', () => reject(new Error('Cancelled')), { once: true });
        }),
      ]);
      if (this.closed || state.abort.signal.aborted || this.pending.get(threadId) !== state) return;
      if (this.active.has(threadId) || (this.activity.get(threadId) ?? 0) !== activity) throw new Error('Native turn changed during selection');
      if (decision.isCurrent && !decision.isCurrent()) throw new Error('Stale decision');
      failure = 'invalid-selection';
      if (decision.passthrough) {
        failure = 'submission-uncertain-no-replay';
        state.phase = 'sending';
        let forwarded = this.requireFreshTurn && this.service?.threads?.get(threadId)?.owned
          ? { ...request, params: { ...request.params, mindplayerRequireNewTurn: true } } : request;
        if (Object.hasOwn(forwarded.params, 'mindplayerRoutingChoice')) {
          const params = { ...forwarded.params }; delete params.mindplayerRoutingChoice;
          forwarded = { ...forwarded, params };
        }
        this.requests.set(idKey(request.id), forwarded);
        this.record({ event: 'route-skipped', threadId, requestId: request.id, reason: decision.reason,
          model: request.params.collaborationMode?.settings?.model ?? request.params.model,
          effort: request.params.collaborationMode?.settings?.reasoning_effort ?? request.params.effort ?? null });
        this.service?.trackSubmission?.(forwarded);
        this.toServer(forwarded);
        state.phase = 'sent';
        return;
      }
      const candidate = (this.service?.catalog ?? this.catalog).find(m => m.model === decision.model && !m.hidden);
      if (!candidate) throw new Error('Selected model is not in the current catalog');
      if (!this.service && ![this.pair.simple.model, this.pair.complex.model].includes(candidate.model)) throw new Error('Selected model is outside the configured pair');
      if (request.params.input.some(i => i.type === 'image' || i.type === 'localImage') && !candidate.inputModalities?.includes('image')) {
        throw new Error('Selected model does not advertise image input');
      }
      const params = structuredClone(request.params);
      delete params.mindplayerRoutingChoice;
      // The verified private backend enforces this atomically in Core. A later
      // active turn must reject this input, never absorb it as steering.
      if (this.requireFreshTurn) params.mindplayerRequireNewTurn = true;
      params.model = candidate.model;
      const supported = candidate.supportedReasoningEfforts?.map(e => e.reasoningEffort) ?? [];
      const wanted = this.service ? decision.effort : params.collaborationMode?.settings?.reasoning_effort ?? params.effort;
      if (this.service && !(supported.includes(wanted) || (wanted === null && supported.length === 0 && candidate.defaultReasoningEffort === null))) throw new Error('Selected effort is not supported');
      const effort = supported.includes(wanted) ? wanted : candidate.defaultReasoningEffort;
      if (effort || (this.service && effort === null)) params.effort = effort;
      else delete params.effort;
      if (params.collaborationMode?.settings) {
        params.collaborationMode.settings.model = candidate.model;
        params.collaborationMode.settings.reasoning_effort = effort ?? null;
      }
      failure = 'submission-uncertain-no-replay';
      state.phase = 'sending';
      state.model = candidate.model;
      const forwarded = { ...request, params };
      this.requests.set(idKey(request.id), forwarded);
      this.record({ event: 'route-requested', threadId, requestId: request.id, model: candidate.model, effort: effort ?? null, reason: decision.reason });
      this.service?.trackSubmission?.(forwarded);
      this.toServer(forwarded);
      state.phase = 'sent';
    } catch (error) {
      if (!this.closed && !state.abort.signal.aborted && this.pending.get(threadId) === state) {
        this.pending.delete(threadId);
        this.service?.cancelDecision?.(request);
        this.requests.delete(idKey(request.id));
        // Copy only a fixed diagnostic, never an upstream message/cause.
        const knownFailure = state.phase === 'deciding' && failure === 'selector-error'
          && Object.hasOwn(JEV_FAILURES, error?.code) ? JEV_FAILURES[error.code] : null;
        if (knownFailure) failure = knownFailure[0];
        this.record({ event: 'route-failed', threadId, requestId: request.id, reason: failure, errorType: error?.name ?? 'UnknownError' });
        this.reject(request.id, state.phase === 'sending'
          ? 'Submission outcome unknown; not retried. Check thread before sending again'
          : knownFailure ? knownFailure[1]
          : 'Model routing failed before submission; nothing was sent');
      }
    } finally { clearTimeout(timer); state.abort.abort(); }
  }

  server(message) {
    if (this.closed) return;
    if (!message.method && typeof message.id === 'string' && message.id.startsWith(this.internalPrefix)) {
      const waiting = this.internal.get(message.id);
      if (waiting) {
        clearTimeout(waiting.timer); this.internal.delete(message.id);
        if (message.error) waiting.reject(new Error('Context read rejected'));
        else waiting.resolve(message.result);
      }
      return;
    }
    const request = !message.method && message.id !== undefined ? this.requests.get(idKey(message.id)) : undefined;
    let observation;
    if (this.service) {
      const revision = this.service.revision;
      observation = this.service.observe(request, message);
      if (this.service.revision !== revision) {
        for (const [threadId, pending] of this.pending) {
          const lostOwner = this.service.threads instanceof Map && !this.service.threads.get(threadId)?.owned;
          if (pending.phase === 'deciding' || (pending.phase === 'waiting' && lostOwner)) this.cancelPending(threadId, 'stale-decision');
        }
      }
    }
    if (request) {
      this.requests.delete(idKey(message.id));
      this.seen.delete(idKey(message.id));
      if (!message.error && ['thread/start', 'thread/resume', 'thread/fork'].includes(request.method)
          && (!this.service || observation?.lifecycleThreadId === message.result?.thread?.id)) {
        this.closedThreads.delete(message.result?.thread?.id);
      }
      if (request.method === 'turn/start') {
        const threadId = request.params.threadId;
        const state = this.pending.get(threadId);
        const turnId = message.result?.turn?.id;
        const matches = state?.request.id === request.id && ['sending', 'sent'].includes(state.phase);
        const alreadyCompleted = matches && turnId && state.completedTurnIds?.has(turnId);
        // Only the matching routed submission may establish a turn from its
        // ACK. Native tool-output ACKs can arrive after that turn completed;
        // their lifecycle is owned by turn/started and turn/completed events.
        const threadClosed = this.closedThreads.has(threadId);
        if (matches && turnId && !alreadyCompleted && !threadClosed && this.active.get(threadId) !== turnId) {
          this.active.set(threadId, turnId); this.bumpActivity(threadId);
        }
        if (matches) {
          this.pending.delete(threadId);
          this.flushInterrupts(state, alreadyCompleted || threadClosed ? undefined : turnId);
        }
        this.record({ event: message.error ? 'start-rejected' : 'start-acknowledged', threadId, requestId: request.id, turnId: turnId ?? null });
      }
    }
    if (message.method === 'turn/started' && !this.closedThreads.has(message.params.threadId)) {
      if (this.active.get(message.params.threadId) !== message.params.turn.id) this.bumpActivity(message.params.threadId);
      this.active.set(message.params.threadId, message.params.turn.id);
      const state = this.pending.get(message.params.threadId);
      const sent = state && ['sending', 'sent'].includes(state.phase);
      this.record({ event: 'turn-started', threadId: message.params.threadId,
        ...(sent ? { requestId: state.request.id } : {}), turnId: message.params.turn.id });
      if (state && ['sending', 'sent'].includes(state.phase)) this.flushInterrupts(state, message.params.turn.id);
    }
    if (message.method === 'turn/completed' && this.active.get(message.params.threadId) === message.params.turn.id) {
      this.active.delete(message.params.threadId);
      this.bumpActivity(message.params.threadId);
      this.pending.get(message.params.threadId)?.ready?.();
    }
    if (message.method === 'turn/completed') {
      const state = this.pending.get(message.params.threadId);
      const sent = state && ['sending', 'sent'].includes(state.phase);
      if (sent) {
        // Notifications can precede the ACK, including stale completions from
        // other turns. Preserve membership instead of replacing the last ID.
        state.completedTurnIds ??= new Set();
        state.completedTurnIds.add(message.params.turn.id);
        if (state.completedTurnIds.size > 128) throw new Error('Excessive completions before turn acknowledgement');
      }
      this.record({ event: 'turn-completed', threadId: message.params.threadId,
        ...(sent ? { requestId: state.request.id } : {}), turnId: message.params.turn.id, status: message.params.turn.status });
    }
    if (message.method === 'thread/closed') {
      const threadId = message.params.threadId;
      this.closedThreads.add(threadId);
      if (this.closedThreads.size > 1000) throw new Error('Excessive closed threads');
      this.active.delete(threadId); this.bumpActivity(threadId);
      this.cancelPending(threadId, 'thread-closed');
      const state = this.pending.get(threadId);
      if (state) this.flushInterrupts(state, undefined);
      this.record({ event: 'thread-closed', threadId });
    }
    if (['model/rerouted', 'model/verification', 'model/safetyBuffering/updated'].includes(message.method)) {
      // No synthetic model telemetry. Only the actual server's event is evidence.
      const fields = {};
      for (const key of ['model', 'fromModel', 'toModel', 'threadId', 'turnId']) {
        const value = message.params?.[key];
        if (typeof value === 'string' && /^[a-zA-Z0-9._:/-]{1,128}$/.test(value)) fields[key] = value;
      }
      this.record({ event: message.method, ...fields });
    }
    this.toClient(message);
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.service?.close();
    for (const waiting of this.internal.values()) { clearTimeout(waiting.timer); waiting.reject(new Error('Connection closed')); }
    this.internal.clear();
    for (const state of this.pending.values()) state.abort.abort();
    this.pending.clear();
    this.record({ event: 'connection-closed-no-replay', activeTurns: this.active.size });
  }
}
import { randomUUID } from 'node:crypto';
