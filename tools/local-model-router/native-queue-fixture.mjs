import { EventEmitter } from 'node:events';
import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, existsSync, mkdtempSync, mkdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline';
import { StringDecoder } from 'node:string_decoder';
import { setTimeout as delay } from 'node:timers/promises';
import { createLocalProxy } from './proxy.mjs';
import { JevService } from './service.mjs';
import { TerminalScreenFixture } from './terminal-screen-fixture.mjs';

// Verified against rust-v0.162.1 / 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d.
// Experimental only: disabling submit makes idle !shell Enter incompatible.
// These overrides are confined to the isolated test process, never launch code.
export const QUEUE_CONFIG = ['tui.keymap.composer.submit=[]', 'tui.keymap.composer.queue=["enter","tab"]'];
export const NATIVE_VERSION = 'codex-cli 0.162.1';
export const NATIVE_CONTRACT = 'mindplayer-native-routing-v4';
export const MODEL = 'native-queue-model';
export const OTHER_MODEL = 'native-queue-other';
export const THREAD_ID = '0199ef00-0000-7000-8000-000000000001';
export const catalog = [MODEL, OTHER_MODEL].map((model, index) => ({
  id: model, model, displayName: model, description: 'Synthetic protocol fixture; no model inference.',
  hidden: false, isDefault: index === 0, inputModalities: ['text'], defaultReasoningEffort: 'low',
  supportedReasoningEfforts: ['low', 'high'].map(reasoningEffort => ({ reasoningEffort, description: reasoningEffort })),
}));

export function until(events, check, label, timeoutMs = 10000) {
  return new Promise((resolve, reject) => {
    let timer;
    const inspect = () => {
      try {
        const value = check();
        if (value) { clearTimeout(timer); events.off('change', inspect); resolve(value); }
      } catch (error) { clearTimeout(timer); events.off('change', inspect); reject(error); }
    };
    timer = setTimeout(() => { events.off('change', inspect); reject(new Error(`Timed out: ${label}`)); }, timeoutMs);
    events.on('change', inspect);
    inspect();
  });
}

// The real proxy -> ModelRouter -> JevService consumers are exercised. Only
// backend lifecycle replies and the final judge function are synthetic; neither
// inference, authentication, nor an actual backend turn_context is established.
export async function createNativeQueueFixture({ queue = true, perPrompt = false, goals = false, plan = false, decisionTimeoutMs = 5000, select = text => ({ model: MODEL, effort: text === 'B' ? 'high' : 'low' }) } = {}) {
  if (perPrompt && (queue || !process.env.MINDPLAYER_NATIVE_CODEX)) {
    throw new Error('Managed session-toggle tests require queue=false and an explicit frozen MINDPLAYER_NATIVE_CODEX candidate');
  }
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'mindplayer-native-queue-')));
  const home = join(root, 'home'), codexHome = join(root, 'codex'), cwd = join(root, 'work');
  try {
    for (const path of [home, codexHome, cwd, join(root, 'tmp')]) mkdirSync(path, { mode: 0o700 });
    writeFileSync(join(codexHome, 'config.toml'), `[projects.${JSON.stringify(cwd)}]\ntrust_level = "trusted"\n`, { mode: 0o600 });
  } catch (error) { rmSync(root, { recursive: true, force: true }); throw error; }
  // Deliberate allowlist: never spread process.env into the native process.
  const env = { PATH: `${dirname(process.execPath)}:/usr/bin:/bin`, HOME: home, CODEX_HOME: codexHome,
    TMPDIR: `${root}/tmp/`, XDG_CONFIG_HOME: join(home, '.config'), TERM: 'xterm-256color',
    LANG: 'en_US.UTF-8', LC_ALL: 'en_US.UTF-8', RUST_BACKTRACE: '0',
    ...(perPrompt ? { MINDPLAYER_PER_PROMPT_CHOICE: '1' } : {}) };
  const binary = process.env.MINDPLAYER_NATIVE_CODEX ?? '/opt/homebrew/bin/codex';
  let version, binarySha256, contract = null;
  try {
    version = execFileSync(binary, ['--version'], { env, encoding: 'utf8', timeout: 10000 }).trim();
    if (version !== NATIVE_VERSION) throw new Error(`Expected ${NATIVE_VERSION}`);
    if (!queue) {
      const output = execFileSync(binary, ['--mindplayer-ui-contract'], {
        env, encoding: 'utf8', timeout: 5000, maxBuffer: 1024, stdio: ['ignore', 'pipe', 'ignore'],
      });
      if (output !== NATIVE_CONTRACT && output !== `${NATIVE_CONTRACT}\n`) throw new Error(`Expected ${NATIVE_CONTRACT}; received ${JSON.stringify(output.trim())}`);
      contract = NATIVE_CONTRACT;
    }
    const hash = createHash('sha256');
    let bytes = 0;
    for await (const chunk of createReadStream(binary)) {
      if ((bytes += chunk.length) > 512 * 1024 * 1024) throw new Error('Native candidate exceeds the 512MiB binary bound');
      hash.update(chunk);
    }
    binarySha256 = hash.digest('hex');
  } catch (error) { rmSync(root, { recursive: true, force: true }); throw error; }
  const events = new EventEmitter();
  const trace = [], calls = [], attempts = [], decisions = [], messages = [], starts = [], shells = [], approvals = [], unknown = [], bridgeMessages = [];
  const transport = new EventEmitter();
  let sequence = 0, bridge, output = '', stderr = '', closing, nextCommand = 1, evidenceRoot;
  const record = value => { trace.push({ sequence: ++sequence, ...value }); events.emit('change'); };
  const deliver = message => { record({ kind: 'backend', message: structuredClone(message) }); transport.emit('message', message); };
  const reply = (message, result) => deliver({ id: message.id, result });
  const thread = { id: THREAD_ID, sessionId: THREAD_ID, projectId: null, preview: '', ephemeral: false,
    modelProvider: 'openai', model: MODEL, reasoningEffort: 'low', createdAt: 1700000000, updatedAt: 1700000000,
    cwd, cliVersion: '0.162.1', source: 'vscode', originator: 'codex-tui', status: { type: 'idle' }, turns: [] };
  const config = { model: MODEL, model_provider: 'openai', model_reasoning_effort: 'low',
    features: { goals },
    approval_policy: 'on-request', sandbox_mode: 'read-only', analytics: { enabled: false },
    check_for_update_on_startup: false, cli_auth_credentials_store: 'file', projects: { [cwd]: { trust_level: 'trusted' } } };
  const send = message => {
    messages.push(structuredClone(message));
    record({ kind: 'request', message: structuredClone(message) });
    if (!message.method || message.id === undefined) return;
    switch (message.method) {
      case 'initialize': return reply(message, { userAgent: 'native-queue-fixture/0.162.1', codexHome, platformFamily: 'unix', platformOs: process.platform === 'darwin' ? 'macos' : 'linux' });
      case 'config/read': return reply(message, { config, origins: {}, layers: [] });
      case 'config/value/write': case 'config/batchWrite': return reply(message, { status: 'ok', version: 'native-queue-fixture', filePath: join(codexHome, 'config.toml'), overriddenMetadata: null });
      case 'configRequirements/read': return reply(message, { requirements: null });
      case 'account/read': return reply(message, { account: { type: 'chatgpt', email: 'native-queue@example.invalid', planType: 'business' }, requiresOpenaiAuth: false });
      case 'account/rateLimits/read': return reply(message, { rateLimits: {}, ordinaryUsageAllowed: true });
      case 'model/list': return reply(message, { data: catalog, nextCursor: null });
      case 'collaborationMode/list': return reply(message, { data: [
        { name: 'Default', mode: 'default', model: MODEL, reasoning_effort: 'low' },
        // A discovered Plan mask deliberately differs at both configuration
        // levels. Actual native slash dispatch, not this fixture, applies it.
        ...(plan ? [{ name: 'Plan', mode: 'plan', model: OTHER_MODEL, reasoning_effort: 'high' }] : []),
      ] });
      case 'thread/list': case 'thread/loaded/list': return reply(message, { data: [], nextCursor: null });
      case 'app/list': return reply(message, { data: [], nextCursor: null });
      case 'app/installed': return reply(message, { data: [] });
      case 'skills/list': case 'hooks/list': return reply(message, { data: [] });
      case 'mcpServerStatus/list': case 'experimentalFeature/list': case 'permissionProfile/list': return reply(message, { data: [], nextCursor: null });
      case 'plugin/list': return reply(message, { marketplaces: [], marketplaceLoadErrors: [], featuredPluginIds: [] });
      case 'thread/start': return reply(message, { thread, model: MODEL, modelProvider: 'openai', reasoningEffort: 'low',
        approvalPolicy: 'on-request', approvalsReviewer: 'user', cwd, sandbox: { type: 'readOnly' } });
      // v2/thread.rs specifies an empty ACK for the native mode-change RPC.
      // Do not invent a settings/applied receipt or rewrite the next turn.
      case 'thread/settings/update': return reply(message, {});
      case 'turn/start': {
        const index = starts.length;
        starts.push({ request: structuredClone(message), turn: { id: `0199ef00-0000-7000-8000-${String(index + 10).padStart(12, '0')}`, rootTurnId: null,
          status: 'inProgress', items: [], error: null, startedAt: 1700000000, completedAt: null, durationMs: null }, acked: false, started: false, completed: false });
        events.emit('change');
        return; // Test alone releases ACK, started, and completion gates.
      }
      case 'turn/steer': return reply(message, { turnId: starts.at(-1)?.turn.id ?? 'no-turn' });
      case 'turn/interrupt': return reply(message, {}); // No terminal until the test observes the real Escape RPC.
      // 0.162.1 tui/src/app/thread_routing.rs dispatches !shell through
      // thread/shellCommand, NOT command/exec. v2/thread.rs specifies an
      // immediate empty ACK; no shell process or inference is executed here.
      case 'thread/shellCommand': shells.push(structuredClone(message)); return reply(message, {});
      default:
        unknown.push(message.method);
        return deliver({ id: message.id, error: { code: -32601, message: `Fixture does not implement ${message.method}` } });
    }
  };
  let proxy, observedService;
  try { proxy = await createLocalProxy({ catalog, cwd, decisionTimeoutMs, requireFreshTurn: true,
    audit: event => record({ kind: 'audit', event }),
    createTransport: () => ({ events: transport, send, close() {} }),
    serviceFactory: audit => {
      const service = new JevService({ catalog, accountHome: codexHome, requirePromptChoice: perPrompt,
      account: { type: 'chatgpt', email: 'native-queue@example.invalid' }, apiKey: 'synthetic-judge-only',
      externalPromptTransfer: true, policyReader: () => null, judgmentLimits: { cooldownMs: 0 }, audit,
      requirementsForTurn: request => { attempts.push(structuredClone(request)); return { provider: 'codex' }; },
      judgeFactory: ({ configurations }) => async (context, { signal }) => {
        const text = context.currentInput;
        const call = { text, context: structuredClone(context) }; calls.push(call); events.emit('change');
        const chosen = await select(text, { signal, context: structuredClone(context) });
        const index = configurations.findIndex(row => row.model === chosen.model && row.effort === chosen.effort);
        if (index < 0) throw new Error('Requested synthetic judgment configuration is unavailable');
        Object.assign(call, chosen); record({ kind: 'judge', text, ...chosen });
        return { capability: 'bounded_routine', confidence: 1, probabilities: { bounded_routine: 1, multistep_coordination: 0, open_problem: 0, unknown: 0 },
          contextSufficient: 1, highConsequence: 0, usage: { inputTokens: 0, outputTokens: 0 }, gatewayCostUsd: null,
          configuration: { ...chosen, choice: `c${index}`, confidence: 1, probabilities: Object.fromEntries(configurations.map((_, i) => [`c${i}`, Number(i === index)])) } };
      },
      });
      observedService = service;
      const decide = service.decide.bind(service);
      service.decide = async (request, options) => {
        // Passive observation before the actual consumer validates or strips
        // metadata. Never manufacture the native UI's choice, ID or input.
        const row = { request: structuredClone(request) };
        decisions.push(row); record({ kind: 'service-input', ...row });
        try {
          return await decide(request, options);
        } catch (error) { row.error = error.code ?? error.name; throw error; }
      };
      return service;
    },
  }); } catch (error) { rmSync(root, { recursive: true, force: true }); throw error; }
  const args = ['--remote', proxy.url, '--remote-auth-token-env', 'NATIVE_QUEUE_REMOTE_TOKEN', '--no-alt-screen',
    '--cd', cwd, '--model', MODEL, '-c', 'check_for_update_on_startup=false', '-c', 'cli_auth_credentials_store="file"',
    '-c', 'analytics.enabled=false', '-c', 'model_reasoning_effort="low"',
    ...(goals ? ['-c', 'features.goals=true'] : []),
    ...(queue ? QUEUE_CONFIG.flatMap(entry => ['-c', entry]) : [])];
  try {
    bridge = spawn(process.env.MINDPLAYER_NATIVE_PYTHON ?? '/opt/homebrew/bin/python3', ['-u', fileURLToPath(new URL('./native-queue-pty.py', import.meta.url))], { env, stdio: ['pipe', 'pipe', 'pipe'] });
  } catch (error) {
    await proxy.close(); rmSync(root, { recursive: true, force: true }); throw error;
  }
  let bridgeEnded = false;
  const bridgeExit = new Promise(resolve => bridge.once('close', () => { bridgeEnded = true; events.emit('change'); resolve(); }));
  const terminalDecoder = new StringDecoder('utf8');
  const terminal = new TerminalScreenFixture(40, 120);
  bridge.stderr.setEncoding('utf8'); bridge.stderr.on('data', chunk => { stderr += chunk; });
  bridge.on('error', error => { bridgeMessages.push({ event: 'bridge-error', message: error.message }); events.emit('change'); });
  createInterface({ input: bridge.stdout }).on('line', line => {
    let message;
    try { message = JSON.parse(line); }
    catch { message = { event: 'bridge-error', message: 'Invalid JSONL from the owned PTY bridge' }; }
    bridgeMessages.push(message);
    if (message.event === 'output') {
      const text = terminalDecoder.write(Buffer.from(message.data, 'base64'));
      terminal.write(text);
      output = (output + text).slice(-1024 * 1024);
    }
    events.emit('change');
  });
  bridge.stdin.on('error', error => { bridgeMessages.push({ event: 'bridge-error', message: error.code }); events.emit('change'); });
  bridge.stdin.write(`${JSON.stringify({ binary, args, cwd, env: { ...env, NATIVE_QUEUE_REMOTE_TOKEN: proxy.token } })}\n`);
  async function write(text) {
    const id = nextCommand++;
    bridge.stdin.write(`${JSON.stringify({ id, op: 'write', data: Buffer.from(text).toString('base64') })}\n`);
    await until(events, () => bridgeMessages.find(message => message.event === 'ack' && message.id === id), 'PTY write acknowledgement');
  }
  const screen = (offset = 0) => output.slice(offset).replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '').replace(/\x1b\][^\x07]*(?:\x07|\x1b\\)/g, '');
  const wait = (check, label, timeout) => until(events, () => {
    const error = bridgeMessages.find(message => message.event === 'bridge-error' || message.event === 'exit');
    if (error) throw new Error(`Native process stopped: ${JSON.stringify(error)}; terminal=${screen().slice(-4000)}; stderr=${stderr}`);
    return check();
  }, label, timeout).catch(error => { error.message += `; methods=${messages.map(message => message.method).filter(Boolean).join(',')}; terminal=${screen().slice(-4000)}`; throw error; });
  const cleanup = { ownedPids: [], port: Number(new URL(proxy.url).port), alivePids: null, portClosed: false, rootRemoved: false };
  const fixture = { binary, version, contract, binarySha256, root, env, args, events, trace, calls, attempts, decisions, messages, starts, shells, approvals, unknown, bridgeMessages, write, screen, wait, cleanup,
    // Read-only copy of the actual service consumer's ACK/completion-bound prior.
    get previousDecision() { return structuredClone(observedService?.threads.get(THREAD_ID)?.previousDecision ?? null); },
    projectedScreen: () => terminal.text(),
    capture(label) {
      if (!/^[a-z0-9-]+$/.test(label)) throw new Error('Evidence label must be a simple filename');
      evidenceRoot ??= realpathSync(mkdtempSync(join(tmpdir(), 'mindplayer-native-queue-evidence-')));
      const base = join(evidenceRoot, label);
      writeFileSync(`${base}.ansi`, output, { mode: 0o600 });
      writeFileSync(`${base}.txt`, screen(), { mode: 0o600 });
      writeFileSync(`${base}.screen.txt`, terminal.text(), { mode: 0o600 });
      writeFileSync(`${base}.json`, `${JSON.stringify({ label, binary, binarySha256, version, contract,
        args, rows: 40, columns: 120, perPrompt, ownedPids: cleanup.ownedPids,
        scope: 'Actual native PTY bytes; ANSI-stripped text is a cumulative transcript, not a screen reconstruction. Synthetic backend/judge; no auth or inference.',
        projection: 'Existing TerminalScreenFixture text cells: cursor/erase/Hangul widths, no styling; not a general terminal emulator.',
        screen: terminal.text(),
        decisions, calls, trace }, null, 2)}\n`, { mode: 0o600 });
      return base;
    },
    captureStable(label) {
      // Observe a stable 40x120 buffer before saving it; no fabricated screen
      // rows, fixed positive-assertion sleep or native rendering modification.
      return new Promise((resolve, reject) => {
        let quiet, current = terminal.text();
        const finish = error => {
          clearTimeout(quiet); clearTimeout(deadline); events.off('change', inspect);
          if (error) reject(error);
          else {
            try { resolve(fixture.capture(label)); }
            catch (captureError) { reject(captureError); }
          }
        };
        const inspect = () => {
          const next = terminal.text();
          if (next === current && quiet) return;
          current = next; clearTimeout(quiet);
          quiet = setTimeout(() => finish(), 150);
        };
        const deadline = setTimeout(() => finish(new Error('Native text-cell buffer did not stabilize: ' + label)), 3000);
        events.on('change', inspect); inspect();
      });
    },
    checkpoint: () => output.length,
    clientRequests: method => trace.filter(row => row.kind === 'audit' && row.event.event === 'rpc-client' && row.event.method === method),
    waitRequest: (method, index = 0, timeout) => wait(() => messages.filter(message => message.method === method)[index], `native ${method} ${index + 1}`, timeout),
    waitReply: (id, timeout) => wait(() => messages.find(message => !message.method && message.id === id), `native reply ${id}`, timeout),
    async enter(text) {
      // Separate text entry from Enter. An unbracketed A+CR in one PTY write
      // activates the native paste-burst path rather than physical key entry.
      const offset = output.length;
      await write(text);
      // Shell mode renders a separate "! " prompt. Match its command body.
      await wait(() => screen(offset).includes(text.startsWith('!') ? text.slice(1) : text), 'native composer text rendering');
      await write('\r');
    },
    async pasteEnter(...texts) {
      // A complete bracketed paste is one explicit input boundary, even when
      // several entries arrive while the native event loop awaits a start ACK.
      await write(texts.map(text => `\x1b[200~${text}\x1b[201~\r`).join(''));
    },
    async expectNoSubmission(durationMs = 400, offset = trace.length) {
      // Bounded negative observation, not a sleep/retry used to make a positive
      // assertion pass. Observe the client side too: the router can hold a
      // premature turn/start before it reaches the synthetic backend.
      const check = () => trace.slice(offset).find(row => row.kind === 'audit' && row.event.event === 'rpc-client'
        && ['turn/start', 'turn/steer', 'thread/shellCommand'].includes(row.event.method));
      await new Promise((resolve, reject) => {
        const inspect = () => {
          const request = check();
          if (request) { clearTimeout(timer); events.off('change', inspect); reject(new Error(`Unexpected native submission: ${request.event.method}`)); }
        };
        const timer = setTimeout(() => { events.off('change', inspect); resolve(); }, durationMs);
        events.on('change', inspect);
        inspect();
      });
    },
    waitStart: index => wait(() => starts[index], `native turn/start ${index + 1}`),
    reject(index, message = 'native-fixture-start-rejection') {
      const state = starts[index];
      if (!state || state.acked || state.started || state.rejected) throw new Error('Only a pending turn/start can be rejected');
      // app-server/tests/suite/v2/turn_start.rs rejects before ACK/started with
      // JSONRPCError (-32600); event_dispatch.rs handles TypedRequestError::Server.
      // This is a request error, not an invented terminal or a transport failure.
      state.rejected = true;
      deliver({ id: state.request.id, error: { code: -32600, message, data: null } });
    },
    acknowledge(index) { const state = starts[index]; state.acked = true; reply(state.request, { turn: state.turn }); },
    start(index) {
      const state = starts[index]; state.started = true;
      deliver({ method: 'turn/started', params: { threadId: THREAD_ID, turn: state.turn } });
      const clientId = state.request.params.clientUserMessageId ?? null;
      const item = { id: clientId ?? `native-queue-user-${index}`, clientId, type: 'userMessage', content: state.request.params.input };
      state.turn.items.push(item);
      deliver({ method: 'item/started', params: { threadId: THREAD_ID, turnId: state.turn.id, item } });
      deliver({ method: 'item/completed', params: { threadId: THREAD_ID, turnId: state.turn.id, item } });
    },
    complete(index, status = 'completed') {
      if (!['completed', 'interrupted', 'failed'].includes(status)) throw new Error(`Invalid terminal status: ${status}`);
      const state = starts[index]; state.completed = true;
      state.turn = { ...state.turn, status, completedAt: 1700000001, durationMs: 1000,
        error: status === 'failed' ? { message: 'native-fixture-terminal-failure', codexErrorInfo: null, additionalDetails: null } : null };
      deliver({ method: 'turn/completed', params: { threadId: THREAD_ID, turn: state.turn } });
    },
    requestApproval(index) {
      // Exact v2/item.rs commandExecution and approval shapes, including the
      // required startedAtMs; tui/src/chatwidget/protocol_requests.rs handles
      // this server-initiated request. The backend never supplies a decision.
      const state = starts[index], id = `native-queue-approval-${approvals.length}`;
      const item = { id: `native-queue-command-${approvals.length}`, type: 'commandExecution', command: 'printf native-approval-fixture',
        cwd, processId: null, source: 'agent', status: 'inProgress', commandActions: [], aggregatedOutput: null, exitCode: null, durationMs: null };
      const request = { id, method: 'item/commandExecution/requestApproval', params: {
        kind: 'command', threadId: THREAD_ID, turnId: state.turn.id, itemId: item.id, startedAtMs: 1700000000000,
        environmentId: null, command: item.command, cwd, commandActions: [], reason: 'Native queue fixture approval; no command is executed.',
      } };
      approvals.push({ request, item, index });
      deliver({ method: 'item/started', params: { threadId: THREAD_ID, turnId: state.turn.id, item } });
      deliver(request);
      return id;
    },
    resolveApproval(id) {
      const approval = approvals.find(row => row.request.id === id);
      const response = messages.find(message => !message.method && message.id === id);
      if (!response?.result?.decision) throw new Error('Native must answer approval before the fixture resolves it');
      const item = { ...approval.item, status: response.result.decision === 'accept' ? 'completed' : 'declined',
        exitCode: response.result.decision === 'accept' ? 0 : null, aggregatedOutput: '', durationMs: 1 };
      starts[approval.index].turn.items.push(item);
      deliver({ method: 'serverRequest/resolved', params: { threadId: THREAD_ID, requestId: id } });
      deliver({ method: 'item/completed', params: { threadId: THREAD_ID, turnId: starts[approval.index].turn.id, item } });
    },
    deliver,
    close() {
      closing ??= (async () => {
        // Stop the actual bridge consumer, await its finally/waitpid, then
        // verify both owned PIDs and the real listening port before deletion.
        const nativePid = bridgeMessages.find(message => message.event === 'started')?.pid;
        cleanup.ownedPids = [bridge.pid, nativePid].filter(Number.isInteger);
        const alive = pid => {
          try { process.kill(pid, 0); return true; }
          catch (error) { if (error.code === 'ESRCH') return false; throw error; }
        };
        try {
          if (!bridgeEnded) {
            if (bridge.stdin.writable) bridge.stdin.write(`${JSON.stringify({ id: nextCommand++, op: 'stop' })}\n`);
            const finished = new AbortController();
            try { await Promise.race([bridgeExit, delay(4000, undefined, { signal: finished.signal })]); }
            finally { finished.abort(); }
            if (!bridgeEnded) {
              if (nativePid && alive(nativePid)) process.kill(nativePid, 'SIGKILL');
              // EOF enters the Python bridge's finally and reaps the child.
              bridge.stdin.end();
              const grace = new AbortController();
              try { await Promise.race([bridgeExit, delay(2000, undefined, { signal: grace.signal })]); }
              finally { grace.abort(); }
              if (!bridgeEnded) { bridge.kill('SIGKILL'); await bridgeExit; }
            }
          }
        } finally { await proxy.close(); }
        cleanup.alivePids = cleanup.ownedPids.filter(alive);
        cleanup.portClosed = await new Promise((resolve, reject) => {
          const socket = createConnection({ host: '127.0.0.1', port: cleanup.port });
          socket.once('connect', () => { socket.destroy(); resolve(false); });
          socket.once('error', error => { socket.destroy(); if (error.code === 'ECONNREFUSED') resolve(true); else reject(error); });
          socket.setTimeout(1000, () => { socket.destroy(); reject(new Error('Cleanup port probe timed out')); });
        });
        if (cleanup.alivePids.length || !cleanup.portClosed) throw new Error(`Native fixture cleanup incomplete: ${JSON.stringify(cleanup)}`);
        rmSync(root, { recursive: true, force: true });
        cleanup.rootRemoved = !existsSync(root);
        cleanup.evidenceRoot = evidenceRoot ?? null;
        if (evidenceRoot) writeFileSync(join(evidenceRoot, 'cleanup.json'), `${JSON.stringify(cleanup, null, 2)}\n`, { mode: 0o600 });
        return cleanup;
      })();
      return closing;
    },
  };
  try {
    await wait(() => bridgeMessages.find(message => message.event === 'started'), 'actual native PTY child identity');
    cleanup.ownedPids = [bridge.pid, bridgeMessages.find(message => message.event === 'started').pid];
  } catch (error) { await fixture.close(); throw error; }
  return fixture;
}
