import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join } from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { StringDecoder } from 'node:string_decoder';
import { loadJevCredentials } from './service.mjs';
import { TerminalScreenFixture } from './terminal-screen-fixture.mjs';

// Explicit opt-in: three Jev attempts (one cancelled), two tiny native turns,
// including one fixed printf command to verify the private Code Mode host. The existing
// account home is used IN PLACE: no auth copy, login, logout or config writes.
// Only synthetic prompts are submitted in a new, labelled verification session.
// v4 starts OFF: F6 opts this session in, including the identical retry after
// cancellation and later prompts. Enter remains separate. F7 is removed.
// MINDPLAYER_TEST_INSTALLED_RETRY_LIVE=1 MINDPLAYER_TEST_BINARY=/absolute/mindplayer
// MINDPLAYER_TEST_CODEX_HOME=/absolute/existing/home node --test <this file>
const CONTRACT = 'mindplayer-native-routing-v4';
const F6 = '\x1b[17~';
const CHOICE = { native: '[F6] Jev OFF · 기본 모델 유지', jev: '[F6] Jev ON · 외부 전송 동의' };
const readRows = path => {
  try { return readFileSync(path, 'utf8').split('\n').flatMap(line => { try { return [JSON.parse(line)]; } catch { return []; } }); }
  catch (error) { if (error.code === 'ENOENT') return []; throw error; }
};
const findFiles = (root, accept, depth = 0) => {
  if (depth > 4) return [];
  let entries; try { entries = readdirSync(root, { withFileTypes: true }); } catch { return []; }
  return entries.flatMap(entry => entry.isDirectory() ? findFiles(join(root, entry.name), accept, depth + 1)
    : entry.isFile() && accept(entry.name) ? [join(root, entry.name)] : []);
};

test('installed v4 Codex keeps F6 ON after cancellation and applies real Jev/native settings', {
  skip: process.env.MINDPLAYER_TEST_INSTALLED_RETRY_LIVE !== '1', timeout: 210_000,
}, async t => {
  const app = process.env.MINDPLAYER_TEST_BINARY;
  const suppliedHome = process.env.MINDPLAYER_TEST_CODEX_HOME;
  assert.ok(app && isAbsolute(app) && suppliedHome && isAbsolute(suppliedHome), 'explicit installed binary and existing account home required');
  const accountHome = realpathSync(suppliedHome);
  assert.ok(statSync(join(accountHome, 'auth.json')).isFile(), 'existing account auth required; never copied');
  const configHash = () => {
    try { return createHash('sha256').update(readFileSync(join(accountHome, 'config.toml'))).digest('hex'); }
    catch (error) { if (error.code === 'ENOENT') return null; throw error; }
  };
  const beforeConfig = configHash();
  const credential = loadJevCredentials();
  assert.ok(credential?.apiKey && credential.externalPromptTransfer, 'existing consented Jev credential required');
  const sourceNative = join(realpathSync(process.env.HOME), '.mindplayer/native-codex');
  const manifest = JSON.parse(readFileSync(join(sourceNative, 'active.json'), 'utf8'));
  assert.equal(manifest.contract, CONTRACT, 'installed private bundle must be v4; no legacy native-contract fallback');
  assert.match(manifest.binarySha256, /^[a-f0-9]{64}$/);
  assert.equal(manifest.binary, `builds/${manifest.binarySha256}/codex`);
  assert.equal(manifest.codeModeHost, `builds/${manifest.binarySha256}/codex-code-mode-host`);
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'mindplayer-installed-retry-')));
  const home = join(root, 'home'), cwd = join(root, 'work'), temporary = join(root, 'tmp');
  const state = join(home, '.mindplayer'), native = join(state, 'native-codex');
  for (const path of [home, cwd, temporary, state, native]) mkdirSync(path, { recursive: true, mode: 0o700 });
  for (const entry of [manifest.binary, manifest.codeModeHost]) {
    mkdirSync(dirname(join(native, entry)), { recursive: true, mode: 0o700 });
    copyFileSync(join(sourceNative, entry), join(native, entry));
  }
  writeFileSync(join(native, 'active.json'), JSON.stringify(manifest), { flag: 'wx', mode: 0o600 });
  writeFileSync(join(state, 'accounts.json'), JSON.stringify([...['codex', 'claude', 'kiro', 'cursor'].map(provider => ({
    provider, name: 'verification', role: 'primary', disabled: provider !== 'codex',
    slot: provider === 'codex' ? { kind: 'isolated', path: accountHome } : { kind: 'inherited' },
  })), { provider: 'codex', name: 'unused-default', role: 'fallback', disabled: true,
    slot: { kind: 'inherited' } }]), { flag: 'wx', mode: 0o600 });
  const env = { PATH: `${dirname(process.execPath)}:/opt/homebrew/bin:/usr/bin:/bin`, HOME: home,
    TMPDIR: temporary + '/', TERM: 'xterm-256color', LANG: 'en_US.UTF-8', LC_ALL: 'en_US.UTF-8',
    [credential.provider === 'gateway' ? 'AI_GATEWAY_API_KEY' : 'TYPESAFE_API_KEY']: credential.apiKey };
  for (const provider of ['CODEX', 'CLAUDE', 'KIRO', 'CURSOR']) {
    env[`MINDPLAYER_${provider}_DIR`] = join(root, provider.toLowerCase());
    mkdirSync(env[`MINDPLAYER_${provider}_DIR`], { mode: 0o700 });
  }
  let bridge, runner, output = '', failure, closed = true, stopping = false;
  const acknowledgements = new Set();
  const screen = new TerminalScreenFixture(), decoder = new StringDecoder('utf8');
  const owned = new Set(), groups = new Set();
  const processTable = () => execFileSync('/bin/ps', ['-ax', '-o', 'pid=,ppid=,pgid=,uid='], { encoding: 'utf8', timeout: 3000 })
    .trim().split('\n').map(line => line.trim().split(/\s+/).map(Number));
  const discover = () => {
    if (runner) owned.add(runner);
    const table = processTable();
    for (let changed = true; changed;) {
      changed = false;
      for (const [pid, parent] of table) if (owned.has(parent) && !owned.has(pid)) { owned.add(pid); changed = true; }
    }
    for (const [pid, , group] of table) if (owned.has(pid)) {
      assert.ok(owned.has(group), 'never signal an unowned process group'); groups.add(group);
    }
  };
  const exists = group => {
    const members = processTable().filter(([, , pgid]) => pgid === group);
    assert.ok(members.every(([pid, , , uid]) => owned.has(pid) && uid === process.getuid()),
      'never signal a reused or unowned process group');
    // Darwin may return EPERM for kill(-pgid, 0) after the final member exits.
    // Inspect actual membership, not that ambiguous probe result.
    return members.length > 0;
  };
  const signalGroup = (group, signal) => {
    if (!exists(group)) return;
    try { process.kill(-group, signal); }
    catch (error) { if (error.code !== 'ESRCH') throw error; }
  };
  t.after(async () => {
    stopping = true; discover();
    if (bridge?.stdin.writable) bridge.stdin.write('{"id":9999,"op":"stop"}\n');
    for (const group of groups) signalGroup(group, 'SIGTERM');
    for (let n = 0; n < 100 && ([...groups].some(exists) || !closed); n++) {
      await delay(100); discover();
      if (n === 40) for (const group of groups) signalGroup(group, 'SIGKILL');
    }
    assert.ok(closed && [...groups].every(group => !exists(group)), 'owned processes remain; temporary files retained');
    rmSync(root, { recursive: true, force: true });
  });
  // macOS validates this newly copied ~400 MB executable on first execution.
  // Materialize that fixture before the unchanged five-second runtime probe;
  // the later installed consumer must still pass its own strict probe. Warming
  // this copy is not evidence that the supplied app or native path works.
  const copiedContract = spawnSync(join(native, manifest.binary), ['--mindplayer-ui-contract'], {
    env: { HOME: home, PATH: env.PATH }, cwd, encoding: 'utf8', timeout: 30000,
    maxBuffer: 1024, stdio: ['ignore', 'pipe', 'ignore'],
  });
  assert.ok(copiedContract.status === 0 && copiedContract.stdout.trim() === manifest.contract,
    'fresh native fixture must execute the exact contract before UI validation');
  closed = false;
  bridge = spawn('/opt/homebrew/bin/python3', ['-u', fileURLToPath(new URL('./native-queue-pty.py', import.meta.url))], { env, cwd, stdio: ['pipe', 'pipe', 'pipe'] });
  bridge.stderr.resume();
  bridge.on('error', () => { failure = new Error('PTY bridge failed; no secret/output disclosure'); });
  bridge.on('close', () => { closed = true; });
  bridge.stdin.on('error', () => { if (!stopping) failure = new Error('PTY input closed'); });
  createInterface({ input: bridge.stdout }).on('line', line => {
    const message = JSON.parse(line);
    if (message.event === 'started') runner = message.pid;
    if (message.event === 'ack') acknowledgements.add(message.id);
    if (message.event === 'output') {
      const chunk = decoder.write(Buffer.from(message.data, 'base64'));
      output += chunk; screen.write(chunk);
    }
    if (!stopping && ['exit', 'bridge-error'].includes(message.event)) failure = new Error('Installed process exited unexpectedly; output suppressed');
  });
  bridge.stdin.write(JSON.stringify({ binary: realpathSync(app), args: [cwd], cwd, env }) + '\n');
  let id = 0, lastDiscovery = 0, ownedThread;
  const audit = () => findFiles(temporary, name => name === 'routing.jsonl').flatMap(readRows)
    .filter(row => !ownedThread || !row.threadId || row.threadId === ownedThread);
  const plain = () => screen.text();
  const wait = async (check, label, timeout = 60000) => {
    const deadline = Date.now() + timeout;
    for (;;) {
      assert.ok(!stopping && !t.signal.aborted, 'live test cancelled; no further native input permitted');
      if (failure) throw failure;
      assert.ok(!output.includes(credential.apiKey), 'secret leaked; terminal output suppressed');
      if (Date.now() - lastDiscovery > 500) { discover(); lastDiscovery = Date.now(); }
      const result = check(); if (result) return result;
      if (Date.now() >= deadline) {
        t.diagnostic(JSON.stringify({ timedOutAt: label, sessionToggle: {
          off: plain().includes(CHOICE.native),
          on: plain().includes(CHOICE.jev),
          nativeRecovery: plain().includes('prompt kept in queue'),
          attempts: audit().filter(row => row.event === 'jev-started').length,
          completions: audit().filter(row => row.event === 'turn-completed').length,
        } }));
        assert.fail(`Timed out: ${label}; terminal/upstream payload suppressed`);
      }
      await delay(10);
    }
  };
  const write = async text => {
    const commandId = ++id;
    assert.ok(!stopping && !t.signal.aborted, 'live test cancelled; no further native input permitted');
    bridge.stdin.write(JSON.stringify({ id: commandId, op: 'write', data: Buffer.from(text).toString('base64') }) + '\n');
    await wait(() => acknowledgements.has(commandId), 'actual PTY write acknowledgement', 5000);
  };
  await wait(() => plain().includes('collect sessions'), 'scope picker'); await write('\r');
  await wait(() => plain().includes('multi-select'), 'main list without a startup choice');
  assert.ok(!plain().includes('실행 설정표'), 'obsolete startup configuration sheet must not intercept the list');
  // This is only a local credential/contract diagnostic, not a UI choice or a
  // judgment. It must not be mistaken for consent for any future prompt.
  const readiness = spawnSync(process.execPath, [fileURLToPath(new URL('./startup-check.mjs', import.meta.url))], {
    env, cwd, encoding: 'utf8', timeout: 10000, maxBuffer: 1024, stdio: ['ignore', 'pipe', 'ignore'],
  });
  const readinessReasons = ['ready', 'jev-key-unavailable', 'private-ui-missing', 'private-ui-invalid', 'startup-check-failed'];
  let readinessReason;
  try { readinessReason = JSON.parse(readiness.stdout).reason; } catch { /* sanitized below */ }
  if (readiness.status !== 0) {
    const probe = spawnSync(process.execPath, ['--input-type=module', '--eval',
      `import {resolveUiBinary} from ${JSON.stringify(new URL('./private-ui.mjs', import.meta.url).href)};try{resolveUiBinary({integrated:true});console.log('ready')}catch(e){console.log(e.code)}`],
    { env, cwd, encoding: 'utf8', timeout: 10000, maxBuffer: 1024, stdio: ['ignore', 'pipe', 'ignore'] });
    const code = probe.stdout?.trim();
    t.diagnostic(JSON.stringify({ fixturePrivateUiFailure: code === 'ready' || /^ERR_PRIVATE_UI_[A-Z]+$/.test(code ?? '') ? code : 'unknown',
      modes: [state, native, join(native, 'builds'), dirname(join(native, manifest.binary)),
        join(native, 'active.json'), join(native, manifest.binary), join(native, manifest.codeModeHost)]
        .map(path => ({ mode: (statSync(path).mode & 0o7777).toString(8), bytes: statSync(path).size,
          canonical: realpathSync(path) === path })) }));
  }
  assert.equal(readiness.status, 0, `fixture local readiness: ${readinessReasons.includes(readinessReason) ? readinessReason : 'unknown'}`);
  await write('n');
  await wait(() => plain().includes('New session'), 'new-session picker'); await write('\r');
  await wait(() => plain().includes('Label / subject (optional)'), 'label');
  await write('[MindPlayer routing verification]\r');
  ownedThread = (await wait(() => audit().find(row => row.event === 'thread-ownership' && row.owned && row.startup), 'native main-thread ownership')).threadId;
  await wait(() => output.includes('\x1b[?2004h'), 'bracketed paste support');
  await wait(() => plain().includes(CHOICE.native), 'v4 new session defaults to Jev OFF');
  // Name the new test thread explicitly so automatic ephemeral title inference
  // cannot be confused with a submitted human prompt or command receipt.
  // The enclosing Ratatui pane emits changed cells, not a linear transcript.
  // A real /rename RPC + ACK proves composer readiness more reliably than
  // searching that delta stream for an uninterrupted placeholder sentence.
  await write('/rename mindplayer-installed-retry-verification\r');
  const rename = await wait(() => audit().find(row => row.event === 'rpc-client' && row.method === 'thread/name/set'), 'explicit native test thread name');
  await wait(() => audit().some(row => row.event === 'rpc-server' && row.requestId === rename.requestId && row.errorCode === null), 'native rename acknowledgement');
  const prompt = 'Reply only OK. Do not use tools, commands, files, network, subagents or goals.';
  const submitWithSessionJev = async (text = prompt, enable = false) => {
    await wait(() => plain().includes(enable ? CHOICE.native : CHOICE.jev),
      enable ? 'session starts OFF before explicit opt-in' : 'session stays ON for the next attempt');
    await write('\x1b[200~' + text + '\x1b[201~');
    const starts = audit().filter(row => row.event === 'rpc-client' && row.method === 'turn/start').length;
    const judgments = audit().filter(row => row.event === 'jev-started').length;
    if (enable) await write(F6);
    await wait(() => plain().includes(CHOICE.jev), 'session ON after opt-in and draft editing');
    assert.equal(audit().filter(row => row.event === 'rpc-client' && row.method === 'turn/start').length, starts, 'editing/F6 alone must not submit');
    assert.equal(audit().filter(row => row.event === 'jev-started').length, judgments, 'editing/F6 alone must not judge');
    await write('\r');
  };
  await submitWithSessionJev(prompt, true);
  const cancelled = await wait(() => audit().find(row => row.event === 'jev-started'), 'first real Jev request');
  await write('\x1b');
  await wait(() => audit().some(row => row.requestId === cancelled.requestId
    && row.event === 'route-cancelled' && row.reason === 'user-cancelled'), 'identity-bound pending judgment cancellation');
  assert.equal(audit().filter(row => row.event === 'rpc-client' && row.method === 'mindplayer/turn/cancel').length, 1, 'v4 UI must send the dedicated local cancellation RPC');
  assert.ok(!audit().some(row => row.event === 'route-requested' && row.requestId === cancelled.requestId), 'cancelled attempt must not be submitted');
  await wait(() => plain().includes('prompt kept in queue'), 'native cancelled-start recovery');
  // Escape cancels execution, not a restored composer draft: clear it explicitly.
  await write('\x15'); await submitWithSessionJev();
  await wait(() => audit().filter(row => row.event === 'jev-started').length >= 2, 'fresh judgment for identical text');
  await wait(() => audit().some(row => row.event === 'turn-completed' && row.status === 'completed'), 'first native completion');
  await wait(() => plain().includes('Applied for this turn:'), 'native Core-applied model and effort notice');
  await submitWithSessionJev('Use the command execution tool exactly once with cmd="printf MINDPLAYER_NATIVE_HOST_OK", login=false, yield_time_ms=1000. If tools are nested in Code Mode, use functions.exec to await tools.exec_command. Then reply only OK. No other tools, files, network, subagents or goals. If unavailable reply TOOL_UNAVAILABLE.');
  await wait(() => audit().filter(row => row.event === 'turn-completed' && row.status === 'completed').length === 2, 'second native completion');
  const rows = audit(), judgments = rows.filter(row => row.event === 'jev-started');
  assert.equal(judgments.length, 3); assert.equal(new Set(judgments.map(row => row.requestId)).size, 3);
  assert.equal(rows.filter(row => row.event === 'jev-judged').length, 2, 'both noncancelled attempts need real Jev responses');
  assert.equal(rows.filter(row => row.event === 'rpc-client' && row.method === 'turn/start').length, 3, 'exactly three explicit UI attempts, never an implicit replay');
  assert.equal(rows.filter(row => row.event === 'rpc-client' && row.method === 'mindplayer/prompt/preview').length, 0, 'removed F7 RPC must never be emitted');
  await wait(() => plain().includes(CHOICE.jev), 'completion keeps this session ON');
  assert.ok(!rows.some(row => row.requestId === cancelled.requestId
    && ['route-requested', 'start-acknowledged'].includes(row.event)), 'cancelled attempt must remain unsubmitted after later turns complete');
  const completed = rows.filter(row => row.event === 'turn-completed' && row.status === 'completed');
  const ownThread = rows.find(row => row.event === 'thread-ownership' && row.owned && row.startup).threadId;
  const rollout = await wait(() => findFiles(join(accountHome, 'sessions'), name => name.includes(ownThread) && name.endsWith('.jsonl'))[0], 'native rollout');
  const rolloutRows = readRows(rollout);
  const contexts = rolloutRows.filter(row => row.type === 'turn_context');
  const settings = completed.map(turn => {
    const ack = rows.find(row => row.event === 'start-acknowledged' && row.turnId === turn.turnId);
    const selection = rows.find(row => row.event === 'model-selected' && row.requestId === ack?.requestId);
    const context = contexts.find(row => row.payload.turn_id === turn.turnId)?.payload;
    assert.ok(selection && context, 'correlated selection and durable native turn context required');
    assert.equal(context.model, selection.model); assert.equal(context.effort, selection.effort);
    assert.equal(context.collaboration_mode.settings.model, selection.model);
    assert.equal(context.collaboration_mode.settings.reasoning_effort, selection.effort);
    return { model: context.model, effort: context.effort };
  });
  const commandReceipt = rolloutRows.find(row => row.type === 'event_msg'
    && row.payload?.type === 'item_completed' && row.payload.turn_id === completed[1].turnId
    && row.payload.item?.type === 'CommandExecution' && row.payload.item.exit_code === 0
    && String(row.payload.item.aggregated_output).includes('MINDPLAYER_NATIVE_HOST_OK'));
  assert.ok(commandReceipt, 'actual successful native command receipt required, not an assistant claim');
  assert.equal(configHash(), beforeConfig, 'routing must not persist changes to the existing Codex config');
  t.diagnostic(JSON.stringify({ installedMindPlayer: realpathSync(app), contract: CONTRACT, perPromptChoice: true, sessionStickyChoice: true, previewRpcCount: 0,
    jevAttempts: judgments.length, completedRealJevJudgments: 2, cancelledBeforeSubmission: true,
    nativeCompletions: completed.length, settings, actualNativeCommandExitCode: 0, authCopied: false, configurationChanged: false,
    existingAccountHomeUsedInPlace: true, rollout, servingIdentityAttested: false }));
});
