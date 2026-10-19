import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync,
  readdirSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, sep } from 'node:path';
import { createInterface } from 'node:readline';
import { PassThrough } from 'node:stream';
import { StringDecoder } from 'node:string_decoder';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { stripVTControlCharacters } from 'node:util';
import { TerminalScreenFixture } from './terminal-screen-fixture.mjs';

// MANUAL OPT-IN: wait for the reviewed private build before running this.
// MINDPLAYER_TEST_NATIVE_UI_LIVE=1 \
// MINDPLAYER_TEST_CODEX_HOME=/absolute/existing/account/home \
// MINDPLAYER_NATIVE_CODEX=/absolute/private/build/codex \
// MINDPLAYER_NATIVE_CODE_MODE_HOST=/absolute/private/build/codex-code-mode-host \
//   node --test tools/local-model-router/native-ui-live.test.mjs
// Add MINDPLAYER_TEST_BINARY=/absolute/installed/mindplayer to exercise the
// installed app, its embedded router bundle, and its ordinary new-session UI.
// Integrated probe, backend, and UI must all use the same manifest-resolved
// private binary. There is no codex PATH shim or global backend prerequisite.
// v4: each session starts OFF; F6 stays ON across prompts and busy queued input.
// Each submission captures its mode. F6 never submits; F7 is removed.
// Two real Jev judgments and two small authenticated native turns are charged.
// Existing credentials are loaded only inside the opted-in callback. Never
// copy source config, hooks, skills, policy, sessions, or inherited API keys.
// Alternatives: simulated backend cannot establish native inference/settings;
// direct app-server cannot establish busy UI input; chosen: source run.mjs,
// real PTY, manifest-resolved private UI, actual Jev and actual app-server.
// Source mode proves only run.mjs. Installed proof requires the optional binary
// and successful real turns. Neither mode attests serving identity, quality,
// cost savings, changed selections, Escape, approvals, or idle !shell.
const REVISION = '092d3acd6bec3e3a14bdc7e7a2810ab628ab759d';
const CONTRACT = 'mindplayer-native-routing-v4';
const VERSION = 'codex-cli 0.162.1';
// Explicit test setup, not a router fallback. Measured from this account's
// current model/list default on 2026-10-16. Upstream's empty-config built-in
// gpt-5.5 is absent from that catalog and must remain ineligible in production.
const TEST_BASELINE = { model: 'gpt-6.1-sol', effort: 'medium' };
const PROMPT_A = '[native-ui-live-A] Execute exactly `sleep 5` now with the available command execution tool: cmd="sleep 5", login=false, yield_time_ms=10000 in the current working directory. If tools are nested in Code Mode, use functions.exec to await tools.exec_command with those arguments. Actually call the tool and wait for exit before replying only OK. Do not read, write, or list files, access the network, or call unrelated tools. If command execution is unavailable, reply only TOOL_UNAVAILABLE instead of claiming execution.';
const PROMPT_B = 'Reply only OK.';
const MAX_BYTES = 64 * 1024 * 1024;
const F6 = '\x1b[17~';
const CHOICE = { native: '[F6] Jev OFF · 기본 모델 유지', jev: '[F6] Jev ON · 외부 전송 동의' };

// Exact audit vocabulary from service.mjs/router.mjs/proxy.mjs, not a regexp
// accepting arbitrary upstream strings. Unknown/missing values stay bounded.
const ROUTING_FAILURE_REASONS = new Map([
  ['route-skipped', ['non-user-thread', 'external-transfer-policy-unset']],
  ['route-failed', ['selector-error', 'selector-timeout', 'invalid-selection', 'submission-uncertain-no-replay',
    'new-turn-active', 'thread-closed', 'stale-decision', 'main-ownership-unverified', 'jev-key-unavailable',
    'jev-unauthorized', 'jev-payment-required', 'jev-rate-limited', 'jev-http-error', 'jev-tls-error',
    'jev-network-error', 'jev-timeout', 'jev-invalid-response', 'jev-request-too-large', 'jev-response-too-large']],
  ['jev-failed', ['jev-key-unavailable', 'unauthorized', 'payment-required', 'rate-limited', 'http-error',
    'tls-error', 'network-error', 'timeout', 'aborted', 'invalid-response', 'request-too-large',
    'response-too-large', 'invalid-config', 'invalid-request', 'jev-unavailable']],
  ['start-rejected', []],
  ['client-protocol-error', ['invalid-json', 'invalid-rpc-or-forwarding']],
  ['server-protocol-error', ['message-size-limit', 'forwarding-failed']],
  ['transport-start-failed', []],
].map(([event, reasons]) => [event, new Set(reasons)]));

function safeRoutingFailure(audit, trace) {
  const failed = audit.find(row => ROUTING_FAILURE_REASONS.has(row.event));
  if (!failed) return null;
  return {
    event: failed.event,
    reason: ROUTING_FAILURE_REASONS.get(failed.event).has(failed.reason) ? failed.reason
      : failed.reason === undefined ? 'not-reported' : 'suppressed',
    errorType: ['PolicyError', 'Error'].includes(failed.errorType) ? failed.errorType
      : failed.errorType === undefined ? 'not-reported' : 'suppressed',
    counts: {
      'jev-started': audit.filter(row => row.event === 'jev-started').length,
      'jev-judged': audit.filter(row => row.event === 'jev-judged').length,
      'native-starts': trace.filter(row => row.event === 'native-turn-start').length,
      'turn-started': audit.filter(row => row.event === 'turn-started').length,
    },
  };
}

function completeJsonl(body) {
  // A writer may have appended only part of its last record. Complete corrupt
  // records still fail; never put their contents into an exception/diagnostic.
  return body.slice(0, body.lastIndexOf('\n') + 1).split('\n').filter(Boolean).map(line => {
    try { return JSON.parse(line); }
    catch { throw new Error('Invalid complete JSONL record; payload suppressed'); }
  });
}

function rows(path) {
  try {
    assert.ok(statSync(path).size <= MAX_BYTES, 'owned evidence file size bound');
    return completeJsonl(readFileSync(path, 'utf8'));
  } catch (error) { if (error.code === 'ENOENT') return []; throw error; }
}

function files(directory) {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? files(path) : entry.isFile() ? [path] : [];
  });
}

const composerText = value => stripVTControlCharacters(value).replace(/\s/g, '');

function isSleepCommand(command) {
  if (typeof command !== 'string') return false;
  // App-server normally exposes the unwrapped command. Permit the equivalent
  // explicit shell presentation, but no chained commands or extra arguments.
  let value = command.trim().replace(/^(?:\/(?:usr\/)?bin\/)?(?:bash|zsh|sh) -l?c /, '');
  if (['\'', '"'].includes(value[0]) && value.at(-1) === value[0]) value = value.slice(1, -1);
  return /^(?:\/(?:usr\/)?bin\/)?sleep 5$/.test(value);
}

// Pinned native exec_command emits UnifiedExecStartup, not Agent. Never
// accept a user !shell command or a later interactive write as inference proof.
const isAgentCommandSource = source => source === 'agent' || source === 'unifiedExecStartup';

async function sha256(path) {
  const digest = createHash('sha256');
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return digest.digest('hex');
}

// Observe only bytes actually accepted by the real native stdin stream. Keep
// the original receiver, overloads, callback, and backpressure return value.
// Never rewrite a request, retain input text, or supply a native response.
function observeNativeInput(stdin, record) {
  const write = stdin.write;
  let pending = Buffer.alloc(0);
  stdin.write = function (chunk, ...args) {
    const result = Reflect.apply(write, this, [chunk, ...args]);
    try {
      pending = Buffer.concat([pending, Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk, typeof args[0] === 'string' ? args[0] : undefined)]);
      if (pending.length > 64 * 1024 * 1024) { record({ event: 'observer-failed' }); pending = Buffer.alloc(0); return result; }
      for (let newline; (newline = pending.indexOf(10)) >= 0;) {
        const line = pending.subarray(0, newline).toString('utf8');
        pending = pending.subarray(newline + 1);
        if (!line.trim()) continue;
        const message = JSON.parse(line);
        if (message.method === 'turn/start') {
          const params = message.params ?? {};
          record({ event: 'native-turn-start', requestId: message.id, threadId: params.threadId,
            mindplayerRequireNewTurn: params.mindplayerRequireNewTurn === true,
            clientUserMessageId: params.clientUserMessageId ?? null,
            routingChoiceForwarded: Object.hasOwn(params, 'mindplayerRoutingChoice'),
            model: params.model, effort: params.effort ?? null,
            collaborationModel: params.collaborationMode?.settings?.model,
            collaborationEffort: params.collaborationMode?.settings?.reasoning_effort ?? null });
        } else if (message.method === 'turn/steer') record({ event: 'native-turn-steer' });
      }
    } catch { record({ event: 'observer-failed' }); }
    return result;
  };
}

// Serialized into a private --import module. This observes actual spawned
// processes, actual HTTP transports, and actual native stdout. It replaces no
// transport, fetch, judge, timer, spawn, response, or backend. Only safe fields
// cross this boundary; neither raw RPC bodies nor terminal output are logged.
async function observe({ tracePath, credentialPath, cwd, provider, prompts }, isSleepCommand, observeNativeInput) {
  // The installed app also starts the ordinary npm Codex wrapper for read-only
  // quota probes. A PATH trampoline must not instrument those unrelated Node
  // processes or mistake them for the embedded integrated pane launcher.
  if (!process.argv.includes('--mindplayer') || !process.argv[1]?.endsWith('/run.mjs')) return;
  const { channel } = await import('node:diagnostics_channel');
  const { appendFileSync, readFileSync } = await import('node:fs');
  const credential = JSON.parse(readFileSync(credentialPath, 'utf8'));
  const secret = credential.apiKey;
  let sequence = 0;
  const record = row => appendFileSync(tracePath, JSON.stringify({ sequence: ++sequence, at: Date.now(), ...row }) + '\n', { mode: 0o600 });
  record({ event: 'launcher', pid: process.pid, parent: process.ppid, script: process.argv[1],
    integrated: process.argv.includes('--mindplayer'), authHome: process.env.CODEX_HOME,
    perPromptChoice: process.env.MINDPLAYER_PER_PROMPT_CHOICE === '1' });
  const inspect = message => {
    if (message.id !== undefined && message.method) record({ event: 'server-request', method: message.method });
    if (message.method === 'thread/name/updated' && message.params?.threadName === 'mindplayer-native-ui-live') {
      record({ event: 'test-thread-named', threadId: message.params.threadId });
    }
    if (!['item/started', 'item/completed'].includes(message.method)) return;
    const { item, threadId, turnId } = message.params;
    const common = { threadId, turnId, itemId: item.id };
    if (item.type === 'commandExecution') {
      record({ event: message.method === 'item/started' ? 'command-started' : 'command-completed', ...common,
        sleepOnly: isSleepCommand(item.command), cwdOwned: item.cwd === cwd,
        source: item.source, status: item.status, exitCode: item.exitCode ?? null });
    } else if (!['userMessage', 'agentMessage', 'reasoning'].includes(item.type)) {
      record({ event: 'unexpected-item', type: item.type, ...common });
    } else if (message.method === 'item/completed' && item.type === 'userMessage') {
      const text = item.content.filter(value => value.type === 'text').map(value => value.text).join('\n');
      record({ event: 'user-input', ...common, input: text === prompts[0] ? 'A' : text === prompts[1] ? 'B' : 'unexpected' });
    } else if (message.method === 'item/completed' && item.type === 'agentMessage' && item.phase !== 'commentary') {
      record({ event: 'answer', ...common, ok: /^OK[.!]?$/.test(item.text.trim()), toolUnavailable: item.text.trim() === 'TOOL_UNAVAILABLE' });
    }
  };
  channel('child_process').subscribe(({ process: child }) => {
    // Node publishes this channel before spawnargs/stdout exist. Inspect only
    // after its real spawn event, which precedes stdout data delivery.
    child.once('spawn', () => {
      const argv = child.spawnargs ?? [];
      if (argv.some(value => value.includes(secret))) record({ event: 'secret-in-argv' });
      const backend = argv.includes('app-server');
      const ui = argv.includes('--remote');
      const curl = child.spawnfile === '/usr/bin/curl';
      if (!backend && !ui && !curl) { record({ event: 'unexpected-child' }); return; }
      record({ event: 'spawn', pid: child.pid, role: backend ? 'app-server' : ui ? 'ui' : 'jev-curl', binary: child.spawnfile });
      if (curl) record({ event: 'jev-transport-started', transport: 'system-curl', expected: provider === 'gateway' && process.platform === 'darwin' });
      if (!backend) return;
      observeNativeInput(child.stdin, record);
      let pending = Buffer.alloc(0);
      child.stdout.on('data', chunk => {
        pending = Buffer.concat([pending, chunk]);
        if (pending.length > 64 * 1024 * 1024) { record({ event: 'observer-failed' }); pending = Buffer.alloc(0); return; }
        for (let newline; (newline = pending.indexOf(10)) >= 0;) {
          const line = pending.subarray(0, newline).toString('utf8');
          pending = pending.subarray(newline + 1);
          if (!line.trim()) continue;
          try { inspect(JSON.parse(line)); } catch { record({ event: 'observer-failed' }); }
        }
      });
    });
  });
  channel('undici:request:create').subscribe(({ request }) => {
    const endpoint = String(request.origin) + request.path;
    if (['https://api.typesafe.ai/v1/systemone', 'https://ai-gateway.vercel.sh/typesafe/v1/systemone'].includes(endpoint)) {
      record({ event: 'jev-transport-started', transport: 'undici', expected: provider === 'typesafe' || process.platform !== 'darwin' });
    }
  });
}

const observerSource = options => `await (${observe.toString()})(${JSON.stringify(options)}, ${isSleepCommand.toString()}, ${observeNativeInput.toString()});\n`;
const shellWord = value => `'${value.replaceAll("'", "'\\''")}'`;

function processTable() {
  let output;
  try { output = execFileSync('/bin/ps', ['-ax', '-o', 'pid=,ppid=,pgid=,comm='], { encoding: 'utf8', timeout: 3000, stdio: ['ignore', 'pipe', 'pipe'] }); }
  catch { throw new Error('Cannot inspect owned process groups; isolated temporary files retained'); }
  return output.trim().split('\n').filter(Boolean).map(line => {
    const match = line.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(.+)$/);
    assert.ok(match, 'valid bounded process row required');
    const [pid, parent, group] = match.slice(1, 4).map(Number);
    assert.ok([pid, parent, group].every(Number.isSafeInteger), 'valid process identifiers required');
    return { pid, parent, group, command: match[4] };
  });
}

function groupExists(pid) {
  try { process.kill(-pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw new Error('Cannot verify owned process-group exit; isolated temporary files retained'); }
}

test('ignores an unfinished JSONL record while the native writer is appending', () => {
  assert.deepEqual(completeJsonl('{"event":"ready"}\n{"event":'), [{ event: 'ready' }]);
});

test('rejects corrupt complete JSONL records without echoing their contents', () => {
  assert.throws(() => completeJsonl('private-upstream-payload\n'), { message: 'Invalid complete JSONL record; payload suppressed' });
});

test('rejects extra shell operations in the bounded busy-window command', () => {
  for (const command of ['sleep 5; pwd', 'sleep 5 && env', 'sleep 50', 'echo sleep 5', "sh -c 'sleep 5; ls'", '\'sleep 5"']) assert.equal(isSleepCommand(command), false);
});

test('recognizes equivalent native presentations of the one bounded sleep command', () => {
  for (const command of ['sleep 5', '/bin/sleep 5', "/bin/zsh -lc 'sleep 5'", 'sh -c "sleep 5"']) assert.equal(isSleepCommand(command), true);
});

test('accepts native agent execution origins but not user shell or interactive writes', () => {
  for (const source of ['agent', 'unifiedExecStartup']) assert.ok(isAgentCommandSource(source));
  for (const source of ['userShell', 'unifiedExecInteraction', undefined, null, 'other']) assert.equal(isAgentCommandSource(source), false);
});

test('recognizes its own composer input across terminal escapes and line wrapping', () => {
  assert.ok(composerText('\x1b[32mReply\x1b[0m\r\n only\x1b[5;1H OK.').includes(composerText(PROMPT_B)));
});

test('routing failure diagnostics report only fixed failure codes, permitted error types, and actual stage counts', () => {
  for (const errorType of ['PolicyError', 'Error']) {
    assert.deepEqual(safeRoutingFailure([
      { event: 'jev-started' }, { event: 'jev-started' }, { event: 'jev-judged' }, { event: 'turn-started' },
      { event: 'route-failed', reason: 'selector-error', errorType },
    ], [{ event: 'native-turn-start' }]), {
      event: 'route-failed', reason: 'selector-error', errorType,
      counts: { 'jev-started': 2, 'jev-judged': 1, 'native-starts': 1, 'turn-started': 1 },
    });
  }
});

test('routing failure diagnostics suppress unknown values and every raw payload field', () => {
  const privateValue = '/private/prompt-key-secret-sentinel';
  const diagnostic = safeRoutingFailure([
    { event: privateValue, reason: privateValue },
    { event: 'route-failed', reason: privateValue, errorType: privateValue,
      message: privateValue, prompt: privateValue, path: privateValue, apiKey: privateValue },
  ], []);
  assert.deepEqual(diagnostic, {
    event: 'route-failed', reason: 'suppressed', errorType: 'suppressed',
    counts: { 'jev-started': 0, 'jev-judged': 0, 'native-starts': 0, 'turn-started': 0 },
  });
  assert.ok(!JSON.stringify(diagnostic).includes(privateValue));
});

test('routing diagnostics keep helper skips and reasonless native rejection fatal without relaxing gates', () => {
  assert.equal(safeRoutingFailure([{ event: 'route-skipped', reason: 'non-user-thread' }], []).reason, 'non-user-thread');
  assert.equal(safeRoutingFailure([{ event: 'start-rejected' }], []).reason, 'not-reported');
  assert.equal(safeRoutingFailure([{ event: 'turn-started' }], []), null);
});

test('native input observation preserves fragmented bytes, write backpressure, and callback delivery', async () => {
  const stdin = new PassThrough({ highWaterMark: 1 });
  const observed = [], actual = [];
  observeNativeInput(stdin, row => observed.push(row));
  const line = JSON.stringify({ id: 7, method: 'turn/start', params: { threadId: 'owned', clientUserMessageId: 'native-message-7',
    mindplayerRequireNewTurn: true, input: 'private-prompt-sentinel' } }) + '\n';
  let callback;
  const delivered = new Promise(resolve => { callback = resolve; });
  assert.equal(stdin.write(line.slice(0, 12), 'utf8'), false);
  assert.deepEqual(observed, []);
  assert.equal(stdin.write(Buffer.from(line.slice(12)), callback), false);
  stdin.on('data', chunk => actual.push(chunk));
  await delivered;
  assert.equal(Buffer.concat(actual).toString('utf8'), line);
  assert.equal(observed.length, 1);
  assert.equal(observed[0].mindplayerRequireNewTurn, true);
  assert.equal(observed[0].clientUserMessageId, 'native-message-7');
  assert.equal(observed[0].routingChoiceForwarded, false);
  assert.ok(!JSON.stringify(observed).includes('private-prompt-sentinel'));
  stdin.end();
});

test('native new-turn observation accepts only a literal true atomic guard', () => {
  const stdin = new PassThrough();
  stdin.resume();
  const observed = [];
  observeNativeInput(stdin, row => observed.push(row));
  for (const guard of [undefined, false, 'true', 1, true]) stdin.write(JSON.stringify({ id: 1, method: 'turn/start', params: { mindplayerRequireNewTurn: guard } }) + '\n');
  assert.deepEqual(observed.map(row => row.mindplayerRequireNewTurn), [false, false, false, false, true]);
  stdin.end();
});

test('the passive observation module parses without loading credentials or launching native processes', () => {
  execFileSync(process.execPath, ['--check', '--input-type=module'], { input: observerSource({ tracePath: '/unused', credentialPath: '/unused', cwd: '/unused', provider: 'typesafe' }),
    stdio: ['pipe', 'pipe', 'pipe'], timeout: 5000 });
});

test('observation preload leaves unrelated Node quota probes untouched without reading credentials', () => {
  const source = observerSource({ credentialPath: '/must-not-be-read/native-live-secret',
    tracePath: '/must-not-be-written/native-live-observation', cwd: '/unused', provider: 'typesafe', prompts: [] });
  assert.equal(execFileSync(process.execPath, ['--import', `data:text/javascript,${encodeURIComponent(source)}`,
    '-e', 'process.stdout.write("unobserved")'], {
    env: { PATH: process.env.PATH }, encoding: 'utf8', timeout: 5000, stdio: ['ignore', 'pipe', 'pipe'],
  }), 'unobserved');
});

test('v4 busy Codex UI input captures sticky F6 ON for a separately Jev-judged native turn with matching rollout settings', {
  skip: process.env.MINDPLAYER_TEST_NATIVE_UI_LIVE !== '1', timeout: 210_000,
}, async t => {
  assert.notEqual(process.platform, 'win32', 'requires a POSIX PTY and owned process groups');
  const sourceHome = process.env.MINDPLAYER_TEST_CODEX_HOME;
  const suppliedBinary = process.env.MINDPLAYER_NATIVE_CODEX;
  const suppliedHost = process.env.MINDPLAYER_NATIVE_CODE_MODE_HOST;
  const suppliedMindPlayer = process.env.MINDPLAYER_TEST_BINARY;
  assert.ok(sourceHome && isAbsolute(sourceHome), 'explicit existing absolute MINDPLAYER_TEST_CODEX_HOME required');
  assert.ok(suppliedBinary && isAbsolute(suppliedBinary), 'explicit built private MINDPLAYER_NATIVE_CODEX required; no public fallback');
  assert.ok(suppliedHost && isAbsolute(suppliedHost), 'explicit same-build MINDPLAYER_NATIVE_CODE_MODE_HOST required; no public fallback');
  if (suppliedMindPlayer !== undefined) assert.ok(isAbsolute(suppliedMindPlayer), 'MINDPLAYER_TEST_BINARY must explicitly name an absolute installed executable');
  const mindPlayer = suppliedMindPlayer === undefined ? null : realpathSync(suppliedMindPlayer);
  const sourceAuth = join(realpathSync(sourceHome), 'auth.json');
  assert.ok(statSync(sourceAuth).isFile(), 'existing account auth.json required');
  const binary = realpathSync(suppliedBinary);
  const host = realpathSync(suppliedHost);
  const { loadJevCredentials } = await import('./service.mjs');
  const credential = loadJevCredentials(); // Parent only; never print or interpolate the result.
  assert.ok(credential?.apiKey && credential.externalPromptTransfer === true, 'existing Jev credential and prompt-transfer consent required');
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'mindplayer-native-ui-live-')));
  const home = join(root, 'home'), codexHome = join(home, '.codex'), cwd = join(root, 'work'), temporary = join(root, 'tmp');
  const state = join(home, '.mindplayer'), nativeRoot = join(state, 'native-codex');
  const tracePath = join(root, 'native-observation.jsonl');
  let bridge, bridgeClosed, bridgeDidClose = false, runnerPid, output = '', failure, closing = false, nextCommand = 0;
  const screen = new TerminalScreenFixture(), decoder = new StringDecoder('utf8');
  let lastGroupScan = 0;
  const acknowledgements = new Set(), groups = new Set();
  const ownedPids = new Set();
  const trace = () => rows(tracePath);
  const discoverGroups = () => {
    const seeds = new Set([...ownedPids, runnerPid, ...trace().filter(row => ['spawn', 'launcher'].includes(row.event)).map(row => row.pid)].filter(Number.isSafeInteger));
    const table = processTable();
    for (let changed = true; changed;) {
      changed = false;
      for (const row of table) if (seeds.has(row.parent) && !seeds.has(row.pid)) { seeds.add(row.pid); changed = true; }
    }
    for (const pid of seeds) ownedPids.add(pid);
    for (const row of table) if (seeds.has(row.pid)) {
      // Never signal a group shared with the test harness or an unrelated job.
      assert.ok(seeds.has(row.group) || groups.has(row.group), 'only proven owned groups may be cleaned up');
      groups.add(row.group);
    }
    if (runnerPid) groups.add(runnerPid); // pty.fork creates this session/group.
    for (const row of trace().filter(row => row.event === 'spawn' && row.role === 'app-server')) groups.add(row.pid);
  };
  t.after(async () => {
    closing = true;
    discoverGroups();
    if (bridge?.exitCode === null && bridge.stdin.writable) bridge.stdin.write(JSON.stringify({ id: ++nextCommand, op: 'stop' }) + '\n');
    for (const group of groups) if (groupExists(group)) {
      try { process.kill(-group, 'SIGTERM'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
    }
    // run.mjs/openAppServer have their own delayed group-kill timers. Reap the
    // launcher and bridge before validating that every observed group is gone.
    const deadline = Date.now() + 10_000;
    let forceAt = Date.now() + 3000;
    while (true) {
      discoverGroups();
      const live = [...groups].filter(groupExists);
      if (!live.length && (!bridge || bridgeDidClose)) break;
      if (Date.now() >= forceAt) {
        for (const group of live) { try { process.kill(-group, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; } }
        if (bridge?.exitCode === null && bridge.signalCode === null) bridge.kill('SIGKILL');
        forceAt = Infinity;
      }
      assert.ok(Date.now() < deadline, 'owned processes did not exit; isolated temporary files retained');
      await delay(50);
    }
    if (bridgeClosed) await bridgeClosed;
    assert.ok([...groups].every(group => !groupExists(group)), 'owned groups must exit before removing copied auth');
    rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  });
  for (const path of [home, codexHome, cwd, temporary, join(root, 'bin'), state, nativeRoot]) mkdirSync(path, { recursive: true, mode: 0o700 });
  copyFileSync(sourceAuth, join(codexHome, 'auth.json'));
  chmodSync(join(codexHome, 'auth.json'), 0o600);
  const credentialPath = join(state, 'jev-credentials.json');
  writeFileSync(credentialPath, JSON.stringify({ schemaVersion: 1, consentVersion: 'jev-per-prompt-v1',
    apiKey: credential.apiKey, provider: credential.provider, externalPromptTransfer: true }), { flag: 'wx', mode: 0o600 });
  const binarySha256 = await sha256(binary);
  const codeModeHostSha256 = await sha256(host);
  const relativeBinary = `builds/${binarySha256}/codex`;
  const relativeHost = `builds/${binarySha256}/codex-code-mode-host`;
  const privateBinary = join(nativeRoot, relativeBinary);
  mkdirSync(dirname(privateBinary), { recursive: true, mode: 0o700 });
  copyFileSync(binary, privateBinary);
  chmodSync(privateBinary, 0o700);
  copyFileSync(host, join(nativeRoot, relativeHost));
  chmodSync(join(nativeRoot, relativeHost), 0o700);
  assert.equal(await sha256(join(nativeRoot, relativeHost)), codeModeHostSha256, 'isolated Code Mode host must match supplied build');
  assert.equal(await sha256(privateBinary), binarySha256, 'isolated private executable must match supplied build');
  writeFileSync(join(nativeRoot, 'active.json'), JSON.stringify({ schemaVersion: 1, sourceRevision: REVISION,
    contract: CONTRACT, version: '0.162.1', binarySha256, binary: relativeBinary,
    codeModeHostSha256, codeModeHost: relativeHost }), { flag: 'wx', mode: 0o600 });
  // Do not supply a codex PATH shim: that would hide a launcher falling back
  // to an unguarded global app-server. Assert actual absolute spawns below.
  const env = { PATH: `${join(root, 'bin')}:${dirname(process.execPath)}:/opt/homebrew/bin:/usr/bin:/bin`,
    HOME: home, CODEX_HOME: codexHome, TMPDIR: temporary + sep,
    XDG_CONFIG_HOME: join(home, '.config'), XDG_CACHE_HOME: join(home, '.cache'), XDG_DATA_HOME: join(home, '.local', 'share'),
    TERM: 'xterm-256color', LANG: 'en_US.UTF-8', LC_ALL: 'en_US.UTF-8', RUST_BACKTRACE: '0' };
  // Source --mindplayer and installed panes must exercise the same managed
  // composer. The installed app must still set this on its own Node launcher.
  if (!mindPlayer) env.MINDPLAYER_PER_PROMPT_CHOICE = '1';
  if (mindPlayer) {
    env.MINDPLAYER_CODEX_DIR = join(codexHome, 'sessions');
    for (const provider of ['CLAUDE', 'KIRO', 'CURSOR']) {
      env[`MINDPLAYER_${provider}_DIR`] = join(root, provider.toLowerCase());
      mkdirSync(env[`MINDPLAYER_${provider}_DIR`], { mode: 0o700 });
    }
    mkdirSync(env.MINDPLAYER_CODEX_DIR, { mode: 0o700 });
    writeFileSync(join(state, 'accounts.json'), JSON.stringify(['codex', 'claude', 'kiro', 'cursor'].map(provider => ({
      provider, name: 'default', slot: { kind: 'inherited' }, role: 'primary', disabled: provider !== 'codex',
    }))), { flag: 'wx', mode: 0o600 });
  }
  assert.ok(!Object.keys(env).some(key => /KEY|TOKEN|NODE_OPTIONS|PROXY|OMX/.test(key)), 'no inherited credentials, preload, proxy, or runtime configuration');
  assert.ok(!Object.values(env).some(value => value.includes(credential.apiKey)), 'Jev key must not enter launcher/native environment');
  const invoke = args => {
    try { return execFileSync(privateBinary, args, { env, cwd, encoding: 'utf8', timeout: 10_000, stdio: ['ignore', 'pipe', 'pipe'] }).trim(); }
    catch { throw new Error('Could not verify private native binary; output suppressed'); }
  };
  assert.equal(invoke(['--version']), VERSION, 'private native version pinned');
  assert.equal(invoke(['--mindplayer-ui-contract']), CONTRACT, 'reviewed semantic busy queue contract required');
  let mindPlayerVersion, mindPlayerSha256;
  if (mindPlayer) {
    try { mindPlayerVersion = execFileSync(mindPlayer, ['--version'], { env, cwd, encoding: 'utf8', timeout: 5000, stdio: ['ignore', 'pipe', 'pipe'] }).trim(); }
    catch { throw new Error('Could not verify supplied installed MindPlayer; output suppressed'); }
    assert.match(mindPlayerVersion, /^mindplayer \d+\.\d+\.\d+$/);
    mindPlayerSha256 = await sha256(mindPlayer);
  }
  const disabled = ['shell_snapshot', 'multi_agent', 'apps', 'plugins', 'browser_use', 'computer_use',
    'image_generation', 'view_image', 'sleep_tool', 'goals', 'hooks'];
  writeFileSync(join(codexHome, 'config.toml'), [
    `model = ${JSON.stringify(TEST_BASELINE.model)}`, `model_reasoning_effort = ${JSON.stringify(TEST_BASELINE.effort)}`,
    'cli_auth_credentials_store = "file"', 'check_for_update_on_startup = false', 'web_search = "disabled"',
    'approval_policy = "never"', 'sandbox_mode = "read-only"',
    'analytics.enabled = false', 'tui.animations = false', 'tui.notifications = false',
    'shell_environment_policy.inherit = "none"', 'shell_environment_policy.set = { PATH = "/usr/bin:/bin" }',
    ...disabled.map(name => `features.${name} = false`),
    `[projects.${JSON.stringify(cwd)}]`, 'trust_level = "trusted"', '',
  ].join('\n'), { flag: 'wx', mode: 0o600 });
  const observerPath = join(root, 'observe.mjs');
  writeFileSync(observerPath, observerSource({ tracePath, credentialPath, cwd, provider: credential.provider, prompts: [PROMPT_A, PROMPT_B] }), { flag: 'wx', mode: 0o600 });
  if (mindPlayer) {
    // An observation-only Node trampoline, not a Codex/backend replacement.
    // Native binaries receive no NODE_OPTIONS or Jev key environment variables.
    // The installed executable still selects and materializes its own bundle.
    writeFileSync(join(root, 'bin', 'node'), `#!/bin/sh\nexec ${shellWord(process.execPath)} --import ${shellWord(observerPath)} "$@"\n`, { flag: 'wx', mode: 0o700 });
  }
  const runPath = fileURLToPath(new URL('./run.mjs', import.meta.url));
  const args = ['--import', observerPath, runPath, '--mindplayer', '--cwd', cwd, '--codex-home', codexHome, '--',
    '--no-alt-screen', '--ask-for-approval', 'never', '--sandbox', 'read-only'];
  const python = process.env.MINDPLAYER_NATIVE_PYTHON ?? '/opt/homebrew/bin/python3';
  bridge = spawn(python, ['-u', fileURLToPath(new URL('./native-queue-pty.py', import.meta.url))], { env, cwd, stdio: ['pipe', 'pipe', 'pipe'] });
  bridgeClosed = new Promise(resolve => bridge.once('close', () => { bridgeDidClose = true; resolve(); }));
  bridge.stderr.resume(); // Never include upstream stderr or terminal contents in failures.
  bridge.once('error', () => { failure ??= new Error('Real PTY bridge failed to start'); });
  bridge.stdin.on('error', () => { if (!closing) failure ??= new Error('Real PTY bridge input closed'); });
  createInterface({ input: bridge.stdout }).on('line', line => {
    try {
      const message = JSON.parse(line);
      if (message.event === 'started') { runnerPid = message.pid; groups.add(runnerPid); }
      if (message.event === 'ack') acknowledgements.add(message.id);
      if (message.event === 'output') {
        const chunk = decoder.write(Buffer.from(message.data, 'base64'));
        output += chunk; screen.write(chunk);
        if (Buffer.byteLength(output) > MAX_BYTES) failure ??= new Error('Native terminal output exceeded its bound');
        if (output.includes(credential.apiKey)) failure ??= new Error('Native output exposed Jev key; payload suppressed');
      }
      if (!closing && ['exit', 'bridge-error'].includes(message.event)) failure ??= new Error('Native launcher or PTY exited unexpectedly; payload suppressed');
    } catch { failure ??= new Error('Invalid PTY bridge record; payload suppressed'); }
  });
  bridge.stdin.write(JSON.stringify({ binary: mindPlayer ?? process.execPath, args: mindPlayer ? [cwd] : args, cwd, env }) + '\n');
  let logPath;
  const audit = () => {
    if (!logPath) {
      const paths = files(temporary).filter(path => path.endsWith('/routing.jsonl'));
      assert.ok(paths.length <= 1, 'one owned source launcher audit required');
      logPath = paths[0];
    }
    return logPath ? rows(logPath) : [];
  };
  const wait = async (check, label, timeoutMs = 60_000) => {
    const deadline = Date.now() + timeoutMs;
    while (true) {
      assert.ok(!closing && !t.signal.aborted, 'live test cancelled; no further native input permitted');
      if (failure) throw failure;
      if (mindPlayer && runnerPid && Date.now() - lastGroupScan >= 250) { discoverGroups(); lastGroupScan = Date.now(); }
      const routingFailure = safeRoutingFailure(audit(), trace());
      assert.ok(!routingFailure, `Native routing failed while waiting for ${label}; diagnostic=${JSON.stringify(routingFailure)}; payload suppressed; no retry`);
      assert.ok(!trace().some(row => ['observer-failed', 'unexpected-child', 'unexpected-item', 'secret-in-argv', 'server-request'].includes(row.event)), 'native observation/tool/approval boundary failed; payload suppressed');
      const value = check();
      if (value) return value;
      assert.ok(Date.now() < deadline, `Timed out waiting for ${label}; upstream output suppressed; no retry`);
      await delay(25); // Poll actual append-only evidence, never retry a prompt.
    }
  };
  const write = async text => {
    const id = ++nextCommand;
    bridge.stdin.write(JSON.stringify({ id, op: 'write', data: Buffer.from(text).toString('base64') }) + '\n');
    await wait(() => acknowledgements.has(id), 'actual PTY write acknowledgement', 5000);
  };
  const paste = async (text, marker) => {
    const offset = output.length;
    await write(`\x1b[200~${text}\x1b[201~`);
    await wait(() => composerText(output.slice(offset)).includes(composerText(marker)), 'own input consumed by native composer', 10_000);
  };
  const currentChoice = label => composerText(screen.text()).includes(composerText(label));
  const chooseJev = async () => {
    await wait(() => currentChoice(CHOICE.native), 'new native session defaults to Jev OFF');
    const starts = audit().filter(row => row.event === 'rpc-client' && row.method === 'turn/start').length;
    const judgments = audit().filter(row => row.event === 'jev-started').length;
    await write(F6);
    await wait(() => currentChoice(CHOICE.jev), 'explicit F6 external-transfer opt-in for this session');
    assert.equal(audit().filter(row => row.event === 'rpc-client' && row.method === 'turn/start').length, starts, 'F6 must not submit');
    assert.equal(audit().filter(row => row.event === 'jev-started').length, judgments, 'F6 must not judge');
  };
  if (mindPlayer) {
    // Grounded in smoke.rs start_into_main_list and new-session tests, plus
    // main.rs ScopeSelect/Main dispatch. Only MindPlayer's known modal labels
    // are used here; native Codex readiness below is protocol + own-input based.
    let offset = 0;
    const appFrame = async (text, label) => wait(() => composerText(output.slice(offset)).includes(composerText(text)), label, 30_000);
    await appFrame('collect sessions', 'installed MindPlayer scope picker');
    offset = output.length; await write('\r'); // Default = this working directory; never Global.
    await appFrame('multi-select', 'installed MindPlayer main list without a startup choice');
    assert.ok(!composerText(output.slice(offset)).includes(composerText('실행 설정표')), 'obsolete run-wide selection must not intercept startup');
    offset = output.length; await write('n');
    await appFrame('New session', 'installed MindPlayer new-session account picker');
    await appFrame('codex', 'installed MindPlayer default Codex account');
    offset = output.length; await write('\r');
    await appFrame('Label / subject (optional)', 'installed MindPlayer optional label');
    await write('\r');
    await wait(() => rows(join(state, 'audit.jsonl')).some(row => row.kind === 'session_open' && row.agent === 'codex'), 'installed app actual Codex pane open');
  }
  await wait(() => runnerPid && output.includes('\x1b[?2004h')
    && currentChoice(CHOICE.native)
    && audit().some(row => row.event === 'thread-ownership' && row.owned && row.startup), 'v4 OFF composer, native startup lifecycle and bracketed-paste readiness');
  const launchers = trace().filter(row => row.event === 'launcher');
  assert.equal(launchers.length, 1, 'one actual Node router launcher');
  assert.ok(launchers[0].integrated && launchers[0].authHome === codexHome, 'actual integrated launcher must use only the isolated auth home');
  assert.equal(launchers[0].perPromptChoice, true, 'actual launched consumer must require per-message metadata, not inherit a run-wide mode');
  if (mindPlayer) {
    const script = realpathSync(launchers[0].script);
    const withinBundle = relative(join(state, 'runtime'), script);
    assert.ok(withinBundle && !withinBundle.startsWith('..') && !isAbsolute(withinBundle) && script.endsWith('/run.mjs'), 'installed executable must materialize and execute its own embedded router bundle');
    assert.notEqual(script, runPath, 'installed proof must not substitute the source launcher');
    assert.ok(rows(join(state, 'audit.jsonl')).some(row => row.kind === 'app_start' && row.run_id === runnerPid), 'PTY must run the actual supplied MindPlayer process');
  } else assert.equal(realpathSync(launchers[0].script), runPath);
  const ui = trace().filter(row => row.event === 'spawn' && row.role === 'ui');
  assert.equal(ui.length, 1, 'one actual native UI spawn');
  assert.equal(ui[0].binary, privateBinary, 'launcher must use the absolute manifest-resolved private UI, not PATH fallback');
  const backends = trace().filter(row => row.event === 'spawn' && row.role === 'app-server');
  assert.equal(backends.length, 2, 'one real catalog probe and one real native UI backend');
  assert.ok(backends.every(row => row.binary === privateBinary), 'probe and app-server must use the same verified absolute private binary as the UI');
  // Upstream automatically creates a separate ephemeral title-generation
  // inference on the first user message of an unnamed thread. Name this test
  // thread through the real idle UI before measuring exactly two user turns;
  // do not suppress helper RPCs, relax ownership gates, or count them as Jev.
  await paste('/rename mindplayer-native-ui-live', 'mindplayer-native-ui-live');
  await write('\r');
  await wait(() => trace().some(row => row.event === 'test-thread-named'), 'explicit test thread name persisted');
  await paste(PROMPT_A, '[native-ui-live-A]');
  await chooseJev();
  await write('\r');
  const firstAck = await wait(() => audit().find(row => row.event === 'start-acknowledged'), 'A native start ACK after real Jev');
  await wait(() => audit().some(row => row.event === 'turn-started' && row.turnId === firstAck.turnId), 'A actual turn/started');
  const command = await wait(() => {
    const command = trace().find(row => row.event === 'command-started' && row.turnId === firstAck.turnId);
    if (!command && audit().some(row => row.event === 'turn-completed' && row.turnId === firstAck.turnId)) {
      const answer = trace().find(row => row.event === 'answer' && row.turnId === firstAck.turnId);
      assert.fail(`Native turn finished without the requested busy-window command; toolUnavailable=${answer?.toolUnavailable === true}; no retry`);
    }
    return command;
  }, 'A actual native command execution');
  assert.ok(command.sleepOnly && command.cwdOwned && isAgentCommandSource(command.source),
    `only bounded sleep in the owned cwd may execute; sleepOnly=${command.sleepOnly}; cwdOwned=${command.cwdOwned}; agentSource=${isAgentCommandSource(command.source)}`);
  const stillBusy = () => !trace().some(row => row.event === 'command-completed' && row.itemId === command.itemId)
    && !audit().some(row => row.event === 'turn-completed' && row.turnId === firstAck.turnId);
  assert.ok(stillBusy(), 'A must actually be running before B is typed');
  await paste(PROMPT_B, PROMPT_B);
  assert.ok(stillBusy(), 'native composer must consume B while A is executing its command');
  await wait(() => currentChoice(CHOICE.jev), 'editing busy B preserves the session ON mode');
  assert.ok(stillBusy(), 'B must capture sticky ON while A is still executing');
  const submittedBAt = Date.now();
  await write('\r');
  assert.ok(stillBusy(), 'B Enter must reach the real PTY before the bounded command completes');
  await wait(() => currentChoice(CHOICE.jev), 'queued B leaves the session ON');
  // Change only the next input's mode. B already owns its captured ON choice.
  await write(F6);
  await wait(() => currentChoice(CHOICE.native), 'F6 turns the session OFF without rewriting queued B');
  await wait(() => audit().filter(row => row.event === 'turn-completed').length >= 2, 'two actual native terminal outcomes', 120_000);
  await wait(() => trace().filter(row => row.event === 'answer').length >= 2, 'two actual native final responses');
  const actualAudit = audit(), actualTrace = trace();
  const clientStarts = actualAudit.filter(row => row.event === 'rpc-client' && row.method === 'turn/start');
  assert.equal(clientStarts.length, 2, 'native UI must send exactly two separate turn/start requests');
  assert.equal(actualAudit.filter(row => row.event === 'rpc-client' && row.method === 'turn/steer').length, 0, 'busy input must never steer A');
  const forwardedStarts = actualTrace.filter(row => row.event === 'native-turn-start');
  assert.equal(forwardedStarts.length, 2, 'exactly two real native stdin turn/start requests');
  assert.equal(actualTrace.filter(row => row.event === 'native-turn-steer').length, 0, 'forwarded requests must never steer a native turn');
  assert.ok(forwardedStarts.every(row => row.mindplayerRequireNewTurn), 'every forwarded owned turn must carry literal mindplayerRequireNewTurn: true');
  assert.ok(forwardedStarts.every(row => typeof row.clientUserMessageId === 'string' && row.clientUserMessageId.length > 0), 'actual native submission identity must survive queue drain');
  assert.equal(new Set(forwardedStarts.map(row => row.clientUserMessageId)).size, 2, 'queued B must not reuse A submission identity');
  assert.ok(forwardedStarts.every(row => !row.routingChoiceForwarded), 'consent metadata must remain local, not enter the backend');
  assert.equal(actualAudit.filter(row => row.event === 'rpc-client' && row.method === 'mindplayer/prompt/preview').length, 0, 'removed F7 RPC must never be emitted');
  assert.ok(currentChoice(CHOICE.native), 'completed queued B must not restore its captured ON mode onto the session');
  assert.equal(actualAudit.filter(row => row.event === 'route-queued').length, 0, 'B must wait in the native UI, not in the proxy');
  assert.equal(actualTrace.filter(row => row.event === 'jev-transport-started').length, 2, 'exactly two actual Jev HTTP transport invocations');
  assert.ok(actualTrace.filter(row => row.event === 'jev-transport-started').every(row => row.expected), 'real configured credential-provider transport required');
  const starts = actualAudit.filter(row => row.event === 'turn-started');
  const terminals = actualAudit.filter(row => row.event === 'turn-completed');
  assert.equal(starts.length, 2); assert.equal(terminals.length, 2);
  assert.equal(new Set(starts.map(row => row.turnId)).size, 2, 'two distinct real native turn IDs');
  assert.equal(new Set(starts.map(row => row.threadId)).size, 1, 'same actual native thread');
  const terminalA = terminals.find(row => row.turnId === firstAck.turnId);
  assert.ok(actualAudit.indexOf(clientStarts[1]) > actualAudit.indexOf(terminalA), 'native must release B only after A completes');
  const commands = actualTrace.filter(row => row.event === 'command-started');
  assert.equal(commands.length, 1, 'only A may execute the single requested sleep');
  const commandEnd = actualTrace.filter(row => row.event === 'command-completed' && row.itemId === command.itemId);
  assert.equal(commandEnd.length, 1);
  assert.equal(commandEnd[0].exitCode, 0);
  assert.equal(commandEnd[0].status, 'completed');
  assert.ok(command.at <= submittedBAt && submittedBAt <= commandEnd[0].at, 'B was submitted during actual native command execution');
  assert.deepEqual(actualTrace.filter(row => row.event === 'user-input').map(row => row.input), ['A', 'B'], 'two exact fixed prompts become distinct native inputs');
  const rollout = await wait(() => files(join(codexHome, 'sessions')).find(path => path.endsWith('.jsonl')
    && rows(path).some(row => row.type === 'session_meta' && row.payload.id === firstAck.threadId)), 'actual isolated native rollout');
  const within = relative(codexHome, realpathSync(rollout));
  assert.ok(within && within !== '..' && !within.startsWith('..' + sep) && !isAbsolute(within), 'rollout must remain inside isolated auth home');
  const contexts = await wait(() => {
    const found = rows(rollout).filter(row => row.type === 'turn_context');
    return starts.every(start => found.some(row => row.payload.turn_id === start.turnId)) && found;
  }, 'durable turn_context for both actual native turns');
  const proof = [];
  for (const [index, request] of clientStarts.entries()) {
    const one = event => {
      const found = actualAudit.filter(row => row.event === event && row.requestId === request.requestId);
      assert.equal(found.length, 1, `exactly one ${event} for each actual UI request`);
      return found[0];
    };
    const judged = one('jev-judged'), selected = one('model-selected'), routed = one('route-requested'), ack = one('start-acknowledged');
    const forwarded = forwardedStarts.filter(row => row.requestId === request.requestId && row.threadId === routed.threadId);
    assert.equal(forwarded.length, 1, 'one actual guarded native request for each owned routed turn');
    assert.equal(forwarded[0].mindplayerRequireNewTurn, true);
    assert.equal(forwarded[0].model, routed.model); assert.equal(forwarded[0].effort, routed.effort);
    assert.equal(forwarded[0].collaborationModel, routed.model); assert.equal(forwarded[0].collaborationEffort, routed.effort);
    const judging = one('jev-started');
    assert.equal(judged.provider, credential.provider);
    assert.equal(selected.reason, 'catalog-guided-jev', 'no fallback may masquerade as a real Jev selection');
    assert.equal(routed.reason, 'catalog-guided-jev');
    assert.ok(actualAudit.indexOf(judging) < actualAudit.indexOf(judged) && actualAudit.indexOf(judged) < actualAudit.indexOf(routed));
    if (index) assert.ok(actualAudit.indexOf(judging) > actualAudit.indexOf(terminalA), 'B receives its own fresh judgment after A terminates');
    const started = starts.find(row => row.turnId === ack.turnId), terminal = terminals.find(row => row.turnId === ack.turnId);
    assert.ok(started && terminal && actualAudit.indexOf(started) < actualAudit.indexOf(terminal), 'actual started and terminal lifecycle required, independently of ACK');
    assert.equal(terminal.status, 'completed');
    assert.equal(ack.threadId, routed.threadId); assert.equal(started.threadId, routed.threadId);
    const input = actualTrace.filter(row => row.event === 'user-input' && row.turnId === ack.turnId);
    assert.equal(input.length, 1); assert.equal(input[0].input, ['A', 'B'][index], 'request/turn must identify the submitted prompt');
    const answer = actualTrace.filter(row => row.event === 'answer' && row.turnId === ack.turnId);
    assert.equal(answer.length, 1); assert.ok(answer[0].ok, 'bounded actual assistant response required, never used as model/effort evidence');
    const matching = contexts.filter(row => row.payload.turn_id === ack.turnId);
    assert.ok(matching.length > 0, 'actual native context for the acknowledged turn required');
    for (const { payload: context } of matching) {
      assert.equal(context.model, routed.model); assert.equal(context.effort ?? null, routed.effort);
      assert.equal(context.collaboration_mode?.settings?.model, routed.model);
      assert.equal(context.collaboration_mode?.settings?.reasoning_effort ?? null, routed.effort);
      assert.equal(context.approval_policy, 'never');
      // Installed app preserves its actual, explicitly unattended launch
      // policy. The harmless command/cwd boundary above is asserted in both
      // modes; only the standalone source scenario requests OS read-only mode.
      assert.equal(context.sandbox_policy?.type, mindPlayer ? 'danger-full-access' : 'read-only');
      assert.equal(context.cwd, cwd); assert.ok(context.sandbox_policy?.network_access !== true);
    }
    assert.equal(selected.model, routed.model); assert.equal(selected.effort, routed.effort);
    proof.push({ requestId: request.requestId, clientUserMessageId: forwarded[0].clientUserMessageId,
      threadId: ack.threadId, turnId: ack.turnId, model: routed.model, effort: routed.effort,
      mindplayerRequireNewTurn: forwarded[0].mindplayerRequireNewTurn,
      collaborationModel: matching[0].payload.collaboration_mode.settings.model,
      collaborationEffort: matching[0].payload.collaboration_mode.settings.reasoning_effort ?? null, status: terminal.status });
  }
  for (const event of ['jev-started', 'jev-judged', 'model-selected', 'route-requested', 'start-acknowledged']) {
    assert.equal(actualAudit.filter(row => row.event === event).length, 2, `exactly two total ${event} events`);
  }
  const routingFailure = safeRoutingFailure(actualAudit, actualTrace);
  assert.ok(!routingFailure, `no routing failure, fallback, or replay; diagnostic=${JSON.stringify(routingFailure)}`);
  assert.ok(!readFileSync(logPath, 'utf8').includes(credential.apiKey) && !readFileSync(tracePath, 'utf8').includes(credential.apiKey), 'no Jev key in safe evidence files');
  discoverGroups();
  const hosts = processTable().filter(row => ownedPids.has(row.pid) && row.command === join(nativeRoot, relativeHost));
  assert.equal(hosts.length, 1, 'actual inference must use the verified private sibling Code Mode host');
  assert.ok(hosts.every(row => row.group === row.pid), 'observe the actual independently grouped native host');
  // Establish ordinary UI shutdown before t.after is allowed to signal any
  // process. Its emergency cleanup must not masquerade as native host cleanup.
  closing = true;
  bridge.stdin.write(JSON.stringify({ id: ++nextCommand, op: 'write',
    data: Buffer.from('\x1b[200~/quit\x1b[201~\r').toString('base64') }) + '\n');
  const shutdownDeadline = Date.now() + 15_000;
  while (true) {
    const live = processTable();
    if (!live.some(row => row.pid === ui[0].pid)
      && !groupExists(backends[1].pid) && hosts.every(row => !groupExists(row.group))) break;
    assert.ok(Date.now() < shutdownDeadline, 'ordinary native /quit must close UI, backend and detached Code Mode host before forced test cleanup');
    await delay(50);
  }
  t.diagnostic(JSON.stringify({ launcher: mindPlayer ? 'installed MindPlayer embedded router' : 'source run.mjs --mindplayer',
    installedMindPlayerProven: !!mindPlayer, ...(mindPlayer ? { mindPlayerBinary: mindPlayer, mindPlayerVersion, mindPlayerSha256 } : {}),
    nativeVersion: VERSION, sourceRevision: REVISION, contract: CONTRACT, binarySha256, codeModeHostSha256, provider: credential.provider,
    backendBinary: privateBinary, backendVersion: VERSION, backendSha256: binarySha256,
    actualJevTransports: 2, actualNativeInference: true, actualBusyInput: true, nativeQueue: true, perPromptChoice: true, sessionStickyChoice: true, previewRpcCount: 0,
    privateCodeModeHostObserved: true, ordinaryNativeShutdownBeforeForcedCleanup: true, servingAttestation: false, proof }));
});
