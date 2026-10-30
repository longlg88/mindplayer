import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { accessSync, constants, createReadStream, chmodSync, copyFileSync, existsSync, mkdirSync,
  mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, relative, sep } from 'node:path';
import { createInterface } from 'node:readline';
import { StringDecoder } from 'node:string_decoder';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { stripVTControlCharacters } from 'node:util';
import { TerminalScreenFixture } from './terminal-screen-fixture.mjs';

// MANUAL OPT-IN: one small authenticated Codex turn with session Jev OFF;
// no Jev credential, judgment or retry. The historical filename
// and opt-in name are retained, NOT the obsolete startup/run-wide choice.
// MINDPLAYER_TEST_STARTUP_NATIVE_LIVE=1 \
// MINDPLAYER_TEST_BINARY=/absolute/path/to/mindplayer \
// MINDPLAYER_TEST_CODEX_HOME=/absolute/existing/account/home \
// MINDPLAYER_TEST_CODEX_BINARY=/opt/homebrew/bin/codex \
//   node --test tools/local-model-router/startup-native-live.test.mjs
// Optional MINDPLAYER_NATIVE_PYTHON names an absolute parent-side Python 3.
// The installed v4 private bundle is copied from HOME/.mindplayer/native-codex;
// MINDPLAYER_TEST_NATIVE_ROOT may explicitly select another absolute bundle.
// Compare: fake PTY cannot prove inference/settings; direct CLI skips the app;
// chosen: actual installed MindPlayer + v4 F6 composer + owned durable rollout.
// Only auth.json is copied, never config/hooks/policy/skills or API-key env.

const BASELINE = { model: 'gpt-6.1-sol', effort: 'medium' };
const CONTRACT = 'mindplayer-native-routing-v4';
const SOURCE_REVISION = '092d3acd6bec3e3a14bdc7e7a2810ab628ab759d';
const F6 = '\x1b[17~';
const CHOICE = { jev: '[F6] Jev ON · 외부 전송 동의', native: '[F6] Jev OFF · 기본 모델 유지' };
// resume.rs deliberately supplies these CLI flags on every original app
// launch. They override the isolated config's read-only setting in Native.
const APP_PERMISSIONS = { approval: 'never', sandbox: 'danger-full-access' };
const PROMPT = 'Reply only OK. Do not use tools.';
const THREAD_NAME = 'mindplayer-startup-native-live';
const MAX_BYTES = 16 * 1024 * 1024;
const words = value => stripVTControlCharacters(value).replace(/\s/g, '');

function nativeReadiness(nativeOutput, outerOutput) {
  const text = words(nativeOutput);
  return {
    header: text.includes(words('OpenAI Codex')),
    composer: text.includes(words('Ask Codex to do anything')),
    jevOff: text.includes(words(CHOICE.native)),
    // MindPlayer consumes inner PTY mode escapes in vt100. Its own outer
    // paste enablement occurs at startup, before the native-phase offset.
    pasteEnabled: outerOutput.lastIndexOf('\x1b[?2004h') > outerOutput.lastIndexOf('\x1b[?2004l'),
  };
}

class LiveFailure extends Error {}
function requireLive(condition, message) {
  if (!condition) throw new LiveFailure(message);
}

function completeJsonl(body) {
  return body.slice(0, body.lastIndexOf('\n') + 1).split('\n').filter(Boolean).map(line => {
    try { return JSON.parse(line); }
    catch { throw new LiveFailure('Invalid complete JSONL record; payload suppressed'); }
  });
}

function rows(path) {
  try {
    requireLive(statSync(path).size <= MAX_BYTES, 'Owned evidence size bound exceeded');
    return completeJsonl(readFileSync(path, 'utf8'));
  } catch (error) { if (error.code === 'ENOENT') return []; throw error; }
}

function files(directory) {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    // Native Codex creates its own arg0 helper symlinks. Ignore them entirely;
    // do not follow them outside the owned tree while collecting evidence.
    return entry.isDirectory() ? files(path) : entry.isFile() ? [path] : [];
  });
}

function nativeEnvironment(root, codexBinary, nodeBinary = process.execPath) {
  const home = join(root, 'home');
  return {
    // Keep the supplied codex launcher directory: realpath may name an npm JS
    // entrypoint, whose directory neither contains codex nor its node runtime.
    PATH: `${dirname(nodeBinary)}:${dirname(codexBinary)}:/usr/bin:/bin`,
    HOME: home, CODEX_HOME: join(home, '.codex'), TMPDIR: join(root, 'tmp') + sep,
    XDG_CONFIG_HOME: join(home, '.config'), XDG_CACHE_HOME: join(home, '.cache'),
    XDG_DATA_HOME: join(home, '.local', 'share'),
    MINDPLAYER_CODEX_DIR: join(home, '.codex', 'sessions'),
    MINDPLAYER_CLAUDE_DIR: join(root, 'claude'), MINDPLAYER_KIRO_DIR: join(root, 'kiro'),
    MINDPLAYER_CURSOR_DIR: join(root, 'cursor'),
    TERM: 'xterm-256color', LANG: 'en_US.UTF-8', LC_ALL: 'en_US.UTF-8', RUST_BACKTRACE: '0',
  };
}

function validateManifest(manifest) {
  requireLive(manifest && !Array.isArray(manifest) && Object.keys(manifest).length === 8
    && manifest.schemaVersion === 1 && manifest.sourceRevision === SOURCE_REVISION
    && manifest.contract === CONTRACT && manifest.version === '0.162.1'
    && /^[a-f0-9]{64}$/.test(manifest.binarySha256) && /^[a-f0-9]{64}$/.test(manifest.codeModeHostSha256)
    && manifest.binary === `builds/${manifest.binarySha256}/codex`
    && manifest.codeModeHost === `builds/${manifest.binarySha256}/codex-code-mode-host`,
  'Reviewed v4 private bundle manifest required; no public or legacy fallback');
  return manifest;
}

function processTable() {
  let output;
  try {
    output = execFileSync('/bin/ps', ['-ax', '-o', 'pid=,ppid=,pgid=,comm='], {
      encoding: 'utf8', timeout: 3000, maxBuffer: MAX_BYTES, stdio: ['ignore', 'pipe', 'pipe'],
    });
  } catch { throw new LiveFailure('Cannot inspect owned processes; isolated temporary files retained'); }
  return output.trim().split('\n').filter(Boolean).map(line => {
    const match = line.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(.+)$/);
    requireLive(match, 'Invalid process row; isolated temporary files retained');
    const [pid, parent, group] = match.slice(1, 4).map(Number);
    requireLive([pid, parent, group].every(Number.isSafeInteger), 'Invalid process identifiers');
    return { pid, parent, group, command: match[4] };
  });
}

function groupExists(group) {
  try { process.kill(-group, 0); return true; }
  catch (error) {
    if (error.code === 'ESRCH') return false;
    throw new LiveFailure('Cannot verify owned process-group exit; isolated temporary files retained');
  }
}

async function sha256(path) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}

// Validate durable native evidence, never the assistant's account of its model.
// An incomplete append is polled; a contradictory completed turn fails once.
function completedTurn(records, cwd, threadId) {
  const contexts = records.filter(row => row.type === 'turn_context');
  const events = records.filter(row => row.type === 'event_msg').map(row => row.payload);
  const completed = events.filter(row => row.type === 'task_complete');
  if (!completed.length) return null;
  const meta = records.filter(row => row.type === 'session_meta');
  requireLive(meta.length === 1 && meta[0].payload?.id === threadId, 'One matching owned native thread required');
  requireLive(meta[0].payload.cwd === cwd, 'Native thread must use the owned working directory');
  requireLive(completed.length === 1, 'Exactly one native completed turn required; no replay');
  requireLive(completed[0].error == null && !events.some(row => ['error', 'turn_aborted'].includes(row.type)), 'Native turn must complete without error or interruption');
  const turnId = completed[0].turn_id;
  requireLive(typeof turnId === 'string' && turnId.length > 0, 'Native completion must identify its turn');
  const started = events.filter(row => row.type === 'task_started');
  requireLive(started.length === 1 && started[0].turn_id === turnId, 'Matching native turn start required');
  // Match the canonical persisted input used by discovery.rs, not a legacy
  // UI event whose persistence is optional in different native builds.
  const input = records.filter(row => row.type === 'response_item' && row.payload?.type === 'message'
    && row.payload.role === 'user').map(row => row.payload.content?.filter(item => item.type === 'input_text')
    .map(item => item.text).join('\n'));
  requireLive(input.filter(text => text === PROMPT).length === 1, 'Exactly one canonical fixed user prompt required; no replay');
  requireLive(contexts.length > 0 && contexts.every(row => row.payload?.turn_id === turnId), 'All native contexts must identify the completed turn');
  for (const { payload: context } of contexts) {
    requireLive(context.cwd === cwd, 'Native context must use the owned working directory');
    requireLive(context.model === BASELINE.model && context.effort === BASELINE.effort, 'Native model and effort must preserve the explicit baseline');
    requireLive(context.approval_policy === APP_PERMISSIONS.approval, 'Native approval policy must preserve the original app flags');
    requireLive(context.sandbox_policy?.type === APP_PERMISSIONS.sandbox, 'Native sandbox must preserve the original app flags');
    if (context.collaboration_mode?.settings) {
      requireLive(context.collaboration_mode.settings.model === BASELINE.model
        && context.collaboration_mode.settings.reasoning_effort === BASELINE.effort, 'Native collaboration settings must preserve the explicit baseline');
    }
  }
  const finals = records.filter(row => row.type === 'response_item' && row.payload?.type === 'message'
    && row.payload.role === 'assistant' && row.payload.phase === 'final_answer');
  requireLive(finals.length === 1, 'One durable native assistant final required');
  const answer = finals[0].payload.content?.filter(item => item.type === 'output_text').map(item => item.text).join('\n');
  requireLive(typeof answer === 'string' && /^OK[.!]?$/.test(answer.trim()), 'Bounded native assistant answer required; text suppressed');
  requireLive(completed[0].last_agent_message == null || completed[0].last_agent_message.trim() === answer.trim(), 'Native completion and assistant final must agree');
  requireLive(records.indexOf(finals[0]) > records.findIndex(row => row.type === 'turn_context')
    && records.indexOf(finals[0]) < records.findIndex(row => row.type === 'event_msg' && row.payload === completed[0]), 'Native final must occur inside the completed turn');
  requireLive(!records.some(row => row.type === 'response_item' && /(?:tool|function|computer|shell|search|image).*call/.test(row.payload?.type ?? '')), 'Native tiny turn must not call tools');
  return { model: BASELINE.model, effort: BASELINE.effort, turnId, contexts: contexts.length, turns: 1, assistantFinals: 1 };
}

test('per-prompt Native JSONL parsing ignores a partial append and suppresses malformed payloads', () => {
  assert.deepEqual(completeJsonl('{"type":"ready"}\n{"private":'), [{ type: 'ready' }]);
  assert.throws(() => completeJsonl('private-secret-payload\n'), { message: 'Invalid complete JSONL record; payload suppressed' });
});

test('per-prompt Native environment includes only explicit runtime paths and no inherited keys', () => {
  const env = nativeEnvironment('/owned', '/opt/homebrew/bin/codex', '/explicit/node/bin/node');
  assert.equal(env.PATH, '/explicit/node/bin:/opt/homebrew/bin:/usr/bin:/bin');
  assert.equal(env.HOME, '/owned/home');
  assert.equal(env.CODEX_HOME, '/owned/home/.codex');
  assert.ok(!Object.keys(env).some(key => /KEY|TOKEN|NODE|PROXY|OMX|JEV/.test(key)));
  assert.ok(Object.entries(env).filter(([key]) => /HOME|DIR/.test(key)).every(([, value]) => value.startsWith('/owned/')));
});

test('v4 Native readiness requires header, composer, Jev OFF and paste support independently of model display', () => {
  const header = '\x1b[1mOpenAI Codex\x1b[0m', composer = '› Ask Codex to do anything', paste = '\x1b[?2004h';
  const choice = CHOICE.native;
  const ready = (nativeOutput, outerOutput = nativeOutput) => Object.values(nativeReadiness(nativeOutput, outerOutput)).every(Boolean);
  for (const model of ['GPT-6.1-sol', 'gpt-6.1-sol', '']) {
    assert.equal(ready(`${header}\n${model}\n${composer}\n${choice}${paste}`), true);
  }
  for (const incomplete of [header + composer + choice, header + paste + choice, composer + paste + choice,
    header + composer + paste, BASELINE.model + paste]) {
    assert.equal(ready(incomplete), false);
  }
  const nativePhase = header + composer + choice, outerCapture = paste + 'MindPlayer startup' + nativePhase;
  assert.deepEqual(nativeReadiness(nativePhase, outerCapture), { header: true, composer: true, jevOff: true, pasteEnabled: true });
  assert.equal(ready(nativePhase, outerCapture), true, 'outer paste enablement predates the native-phase offset');
  assert.equal(ready(composer, outerCapture), false, 'a stale outer header cannot satisfy native readiness');
  assert.equal(ready(header, outerCapture), false, 'a stale outer composer cannot satisfy native readiness');
  assert.equal(ready(nativePhase, outerCapture + '\x1b[?2004l'), false, 'outer paste must still be enabled');
  assert.equal(ready(header + composer + CHOICE.jev + paste), false, 'new-session readiness must not accept Jev ON');
});

test('per-prompt Native evidence requires one correlated completion and the unchanged permissions', () => {
  const records = [
    { type: 'session_meta', payload: { id: 'owned-thread', cwd: '/owned/work' } },
    { type: 'event_msg', payload: { type: 'task_started', turn_id: 'owned-turn' } },
    { type: 'turn_context', payload: { turn_id: 'owned-turn', cwd: '/owned/work', ...BASELINE,
      approval_policy: 'never', sandbox_policy: { type: 'danger-full-access' } } },
    { type: 'response_item', payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: PROMPT }] } },
    { type: 'response_item', payload: { type: 'message', role: 'assistant', phase: 'final_answer', content: [{ type: 'output_text', text: 'OK' }] } },
    { type: 'event_msg', payload: { type: 'task_complete', turn_id: 'owned-turn', last_agent_message: 'OK' } },
  ];
  assert.equal(completedTurn(records.slice(0, -1), '/owned/work', 'owned-thread'), null);
  assert.deepEqual(completedTurn(records, '/owned/work', 'owned-thread'), { ...BASELINE, turnId: 'owned-turn', contexts: 1, turns: 1, assistantFinals: 1 });
  for (const mutate of [
    copy => { copy[0].payload.id = 'other-thread'; },
    copy => { copy[2].payload.turn_id = 'other-turn'; },
    copy => { copy[2].payload.model = 'private-secret-payload'; },
    copy => { copy[2].payload.effort = 'high'; },
    copy => { copy[2].payload.approval_policy = 'on-request'; },
    copy => { copy[2].payload.sandbox_policy.type = 'read-only'; },
    copy => { copy[4].payload.content[0].text = 'private-secret-payload'; },
    copy => { copy.push(copy[5]); },
    copy => { copy.push(copy[3]); },
    copy => { copy[5].payload.error = { message: 'private-secret-payload' }; },
    copy => { copy.push({ type: 'response_item', payload: { type: 'function_call' } }); },
  ]) {
    const copy = structuredClone(records); mutate(copy);
    assert.throws(() => completedTurn(copy, '/owned/work', 'owned-thread'), error => error instanceof LiveFailure
      && !error.message.includes('private-secret-payload'));
  }
});

test('v4 private Native manifest rejects incompatible contracts, incomplete bundles and escaping paths', () => {
  const manifest = { schemaVersion: 1, sourceRevision: SOURCE_REVISION, contract: CONTRACT, version: '0.162.1',
    binarySha256: 'a'.repeat(64), binary: `builds/${'a'.repeat(64)}/codex`,
    codeModeHostSha256: 'b'.repeat(64), codeModeHost: `builds/${'a'.repeat(64)}/codex-code-mode-host` };
  for (const mutate of [
    copy => { copy.contract = 'mindplayer-native-routing-v5'; },
    copy => { copy.contract = 'mindplayer-native-routing-v3'; },
    copy => { copy.contract = 'mindplayer-native-routing-v2'; },
    copy => { delete copy.codeModeHostSha256; },
    copy => { copy.binary = '/unowned/codex'; },
    copy => { copy.codeModeHost = '../private-secret-payload'; },
    copy => { copy.sourceRevision = 'unreviewed'; },
  ]) {
    const copy = structuredClone(manifest); mutate(copy);
    assert.throws(() => validateManifest(copy), error => error instanceof LiveFailure && !error.message.includes('private-secret-payload'));
  }
});


test('real installed MindPlayer v4 Codex sticky F6 OFF preserves model and effort without Jev judgment', {
  skip: process.env.MINDPLAYER_TEST_STARTUP_NATIVE_LIVE !== '1', timeout: 210_000,
}, async t => {
  let root, bridge, runnerPid, bridgeDidClose = false, closing = false, failure;
  let output = '', outputBytes = 0, nextCommand = 0, lastScan = 0;
  const decoder = new StringDecoder('utf8');
  const screen = new TerminalScreenFixture();
  const groups = new Set(), ownedPids = new Set(), acknowledgements = new Set();
  const discoverGroups = () => {
    const table = processTable(), seeds = new Set(ownedPids);
    // Discover even a child created just before the bridge's started record
    // arrives. The bridge itself shares the harness group and is never used
    // as a group-signal target; only its proven descendants are.
    if (Number.isSafeInteger(bridge?.pid)) seeds.add(bridge.pid);
    if (runnerPid) seeds.add(runnerPid);
    for (let changed = true; changed;) {
      changed = false;
      for (const row of table) if (seeds.has(row.parent) && !seeds.has(row.pid)) { seeds.add(row.pid); changed = true; }
    }
    for (const pid of seeds) if (pid !== bridge?.pid) ownedPids.add(pid);
    for (const row of table) if (seeds.has(row.pid) && row.pid !== bridge?.pid) {
      requireLive(seeds.has(row.group) || groups.has(row.group), 'Only proven owned process groups may be cleaned up');
      groups.add(row.group);
    }
    if (runnerPid) groups.add(runnerPid); // Existing bridge's pty.fork owns this group.
    requireLive(!table.some(row => groups.has(row.group) && !seeds.has(row.pid)), 'Owned cleanup group must not contain unrelated processes');
    return table;
  };
  t.after(async () => {
    closing = true;
    if (!root) return;
    try {
      discoverGroups();
      if (bridge?.exitCode === null && bridge.stdin.writable) bridge.stdin.write(JSON.stringify({ id: ++nextCommand, op: 'stop' }) + '\n');
      const signal = (group, kind) => {
        try { process.kill(-group, kind); }
        catch (error) { if (error.code !== 'ESRCH') throw error; }
      };
      for (const group of groups) if (groupExists(group)) signal(group, 'SIGTERM');
      const deadline = Date.now() + 10_000;
      while (true) {
        const table = discoverGroups(), live = [...groups].filter(groupExists);
        if (!live.length && !table.some(row => ownedPids.has(row.pid)) && (!bridge || bridgeDidClose)) break;
        if (Date.now() > deadline - 7000) {
          for (const group of live) signal(group, 'SIGKILL');
          if (bridge?.exitCode === null && bridge.signalCode === null) bridge.kill('SIGKILL');
        }
        requireLive(Date.now() < deadline, 'Owned children did not exit; isolated temporary files retained');
        await delay(50);
      }
      requireLive([...groups].every(group => !groupExists(group)), 'Owned groups must exit before removing copied auth');
      rmSync(root, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
    } catch (error) {
      throw error instanceof LiveFailure ? error : new LiveFailure('Owned cleanup failed; isolated temporary files retained; payload suppressed');
    }
  });
  try {
    requireLive(process.platform !== 'win32', 'POSIX PTY and owned process groups required');
    const suppliedApp = process.env.MINDPLAYER_TEST_BINARY;
    const codexBinary = process.env.MINDPLAYER_TEST_CODEX_BINARY;
    const sourceHome = process.env.MINDPLAYER_TEST_CODEX_HOME;
    requireLive(suppliedApp && isAbsolute(suppliedApp), 'Explicit absolute MINDPLAYER_TEST_BINARY required');
    requireLive(codexBinary && isAbsolute(codexBinary) && basename(codexBinary) === 'codex', 'Explicit absolute MINDPLAYER_TEST_CODEX_BINARY ending in codex required; no fallback');
    requireLive(sourceHome && isAbsolute(sourceHome), 'Explicit existing absolute MINDPLAYER_TEST_CODEX_HOME required');
    const app = realpathSync(suppliedApp), sourceAuth = join(realpathSync(sourceHome), 'auth.json');
    const suppliedNative = process.env.MINDPLAYER_TEST_NATIVE_ROOT ?? join(realpathSync(process.env.HOME), '.mindplayer', 'native-codex');
    requireLive(isAbsolute(suppliedNative), 'Private bundle source must be an explicit absolute path');
    const sourceNative = realpathSync(suppliedNative);
    requireLive(statSync(join(sourceNative, 'active.json')).size <= 16 * 1024, 'Private manifest size bound required');
    const manifest = validateManifest(JSON.parse(readFileSync(join(sourceNative, 'active.json'), 'utf8')));
    requireLive(statSync(sourceAuth).isFile(), 'Existing account auth.json required');
    accessSync(app, constants.X_OK); accessSync(codexBinary, constants.X_OK);
    const python = process.env.MINDPLAYER_NATIVE_PYTHON ?? '/opt/homebrew/bin/python3';
    requireLive(isAbsolute(python), 'PTY bridge Python must be an absolute executable');
    accessSync(python, constants.X_OK);
    root = realpathSync(mkdtempSync(join(tmpdir(), 'mindplayer-startup-native-live-')));
    chmodSync(root, 0o700);
    const env = nativeEnvironment(root, codexBinary), cwd = join(root, 'work'), state = join(env.HOME, '.mindplayer');
    const nativeRoot = join(state, 'native-codex'), privateBinary = join(nativeRoot, manifest.binary);
    for (const path of [env.HOME, env.CODEX_HOME, cwd, env.TMPDIR, state,
      env.MINDPLAYER_CODEX_DIR, env.MINDPLAYER_CLAUDE_DIR, env.MINDPLAYER_KIRO_DIR, env.MINDPLAYER_CURSOR_DIR]) {
      mkdirSync(path, { recursive: true, mode: 0o700 });
    }
    copyFileSync(sourceAuth, join(env.CODEX_HOME, 'auth.json'));
    chmodSync(join(env.CODEX_HOME, 'auth.json'), 0o600);
    for (const [entry, digest] of [[manifest.binary, manifest.binarySha256], [manifest.codeModeHost, manifest.codeModeHostSha256]]) {
      const source = join(sourceNative, entry), destination = join(nativeRoot, entry);
      requireLive(realpathSync(source) === source && statSync(source).isFile(), 'Private bundle must contain canonical regular executables');
      mkdirSync(dirname(destination), { recursive: true, mode: 0o700 });
      copyFileSync(source, destination); chmodSync(destination, 0o700);
      requireLive(await sha256(destination) === digest, 'Copied private executable must match the reviewed manifest hash');
    }
    writeFileSync(join(nativeRoot, 'active.json'), JSON.stringify(manifest), { flag: 'wx', mode: 0o600 });
    const disabled = ['shell_snapshot', 'multi_agent', 'apps', 'plugins', 'browser_use', 'computer_use',
      'image_generation', 'view_image', 'sleep_tool', 'goals', 'hooks'];
    writeFileSync(join(env.CODEX_HOME, 'config.toml'), [
      `model = ${JSON.stringify(BASELINE.model)}`, `model_reasoning_effort = ${JSON.stringify(BASELINE.effort)}`,
      'cli_auth_credentials_store = "file"', 'check_for_update_on_startup = false', 'web_search = "disabled"',
      'approval_policy = "never"', 'sandbox_mode = "read-only"', 'analytics.enabled = false',
      'tui.animations = false', 'tui.notifications = false', 'shell_environment_policy.inherit = "none"',
      'shell_environment_policy.set = { PATH = "/usr/bin:/bin" }', ...disabled.map(name => `features.${name} = false`),
      `[projects.${JSON.stringify(cwd)}]`, 'trust_level = "trusted"', '',
    ].join('\n'), { flag: 'wx', mode: 0o600 });
    writeFileSync(join(state, 'accounts.json'), JSON.stringify(['codex', 'claude', 'kiro', 'cursor'].map(provider => ({
      provider, name: 'default', slot: { kind: 'inherited' }, role: 'primary', disabled: provider !== 'codex',
    }))), { flag: 'wx', mode: 0o600 });
    const version = (binary, pattern) => {
      let value;
      try { value = execFileSync(binary, ['--version'], { env, cwd, encoding: 'utf8', timeout: 5000,
        maxBuffer: 4096, stdio: ['ignore', 'pipe', 'pipe'] }).trim(); }
      catch { throw new LiveFailure('Could not verify supplied executable version; output suppressed'); }
      requireLive(pattern.test(value), 'Unexpected supplied executable version; output suppressed');
      return value;
    };
    const nativeVersion = version(codexBinary, /^codex-cli \d+\.\d+\.\d+(?:[-.][a-zA-Z0-9.]+)?$/);
    // Warm only the isolated copy, then let the installed consumer enforce its
    // own strict probe. Never replace the supplied installed MindPlayer path.
    let privateContract;
    try { privateContract = execFileSync(privateBinary, ['--mindplayer-ui-contract'], { env, cwd, encoding: 'utf8',
      timeout: 30_000, maxBuffer: 1024, stdio: ['ignore', 'pipe', 'pipe'] }); }
    catch { throw new LiveFailure('Could not verify copied private v4 contract; output suppressed'); }
    requireLive(privateContract === CONTRACT || privateContract === `${CONTRACT}\n`, 'Copied private executable must speak the exact v4 contract');
    const privateVersion = version(privateBinary, /^codex-cli 0\.162\.1$/);
    const mindPlayerVersion = version(app, /^mindplayer \d+\.\d+\.\d+$/);
    const appSha256 = await sha256(app), codexLauncherSha256 = await sha256(realpathSync(codexBinary));
    const audit = () => {
      const paths = files(env.TMPDIR).filter(path => basename(path) === 'routing.jsonl');
      requireLive(paths.length <= 1, 'One owned Codex router audit required');
      return paths.flatMap(rows);
    };
    const assertNoJev = () => {
      // Codex Native uses the managed v4 UI/router for per-message metadata;
      // no-router/no-status is the CLAUDE boundary, not Codex nonuse semantics.
      requireLive(!existsSync(join(state, 'jev-credentials.json')), 'Native-only test must not provision Jev credentials');
      requireLive(!audit().some(row => /^jev-/.test(row.event ?? '')
        || ['model-selected', 'route-requested', 'route-failed', 'start-rejected'].includes(row.event)), 'Native choice must not judge, select, fail over or issue a routed selection');
      requireLive(!files(state).some(path => /claude-(?:run|broker|jev).*\.(?:mjs|json)$/.test(basename(path))), 'Removed Claude Jev assets must not be staged');
    };
    bridge = spawn(python, ['-u', fileURLToPath(new URL('./native-queue-pty.py', import.meta.url))], {
      env, cwd, stdio: ['pipe', 'pipe', 'pipe'],
    });
    bridge.once('close', () => { bridgeDidClose = true; });
    bridge.stderr.resume(); // Never emit raw native/bridge stderr or terminal bytes.
    bridge.once('error', () => { failure ??= new LiveFailure('Real PTY bridge failed to start'); });
    bridge.stdin.on('error', () => { if (!closing) failure ??= new LiveFailure('Real PTY bridge input closed'); });
    createInterface({ input: bridge.stdout }).on('line', line => {
      try {
        requireLive(line.length <= MAX_BYTES, 'PTY bridge record bound exceeded');
        const message = JSON.parse(line);
        if (message.event === 'started') {
          requireLive(Number.isSafeInteger(message.pid) && message.pid > 1, 'Invalid owned PTY process');
          runnerPid = message.pid; groups.add(runnerPid);
        } else if (message.event === 'ack') acknowledgements.add(message.id);
        else if (message.event === 'output') {
          const chunk = Buffer.from(message.data, 'base64'); outputBytes += chunk.length;
          requireLive(outputBytes <= MAX_BYTES, 'Native terminal output bound exceeded');
          const text = decoder.write(chunk); output += text; screen.write(text);
        } else if (!closing && ['exit', 'bridge-error'].includes(message.event)) {
          failure ??= new LiveFailure('Native app or PTY exited unexpectedly; payload suppressed');
        }
      } catch { failure ??= new LiveFailure('Invalid or oversized PTY bridge record; payload suppressed'); }
    });
    bridge.stdin.write(JSON.stringify({ binary: app, args: [cwd], cwd, env }) + '\n');
    const overallDeadline = Date.now() + 120_000;
    const wait = async (check, label, timeout = 15_000) => {
      const deadline = Math.min(overallDeadline, Date.now() + timeout);
      while (true) {
        requireLive(!closing && !t.signal.aborted, 'Live test cancelled; no further native input permitted');
        if (failure) throw failure;
        if (runnerPid && Date.now() - lastScan >= 200) { discoverGroups(); assertNoJev(); lastScan = Date.now(); }
        const value = check(); if (value) return value;
        requireLive(Date.now() < deadline, `Timed out waiting for ${label}; output suppressed; no retry`);
        await delay(25);
      }
    };
    const write = async text => {
      const id = ++nextCommand;
      requireLive(!closing && !t.signal.aborted, 'Live test cancelled; no further native input permitted');
      bridge.stdin.write(JSON.stringify({ id, op: 'write', data: Buffer.from(text).toString('base64') }) + '\n');
      await wait(() => acknowledgements.has(id), 'owned PTY write acknowledgement', 5000);
    };
    let offset = 0;
    const frame = (label, text) => wait(() => words(output.slice(offset)).includes(words(text)), label);
    await frame('MindPlayer scope picker', 'collect sessions');
    offset = output.length; await write('\r');
    await frame('Native main list', 'multi-select');
    requireLive(!words(output.slice(offset)).includes(words('실행 설정표'))
      && !words(output.slice(offset)).includes(words('이번 실행의 판단 방식')), 'Obsolete run-wide startup choice must not intercept the main list');
    assertNoJev();
    offset = output.length; await write('n');
    await frame('new-session account picker', 'New session');
    await frame('default Codex account', 'codex');
    offset = output.length; await write('\r');
    await frame('optional new-session label', 'Label / subject (optional)');
    offset = output.length; await write('\r');
    await wait(() => rows(join(state, 'audit.jsonl')).some(row => row.kind === 'session_open' && row.agent === 'codex'), 'actual Native pane open');
    // Model display labels can differ from the canonical configuration ID.
    // Readiness uses native UI signals; completedTurn verifies model/effort.
    const readiness = () => nativeReadiness(output.slice(offset), output);
    try {
      await wait(() => Object.values(readiness()).every(Boolean), 'v4 Codex header, OFF composer and outer bracketed-paste readiness');
    } catch (error) {
      t.diagnostic(JSON.stringify(readiness())); // Fixed boolean fields only; never raw terminal data.
      throw error;
    }
    const paste = async (text, marker) => {
      offset = output.length;
      await write(`\x1b[200~${text}\x1b[201~`);
      await frame('native composer consumed own input', marker);
    };
    // Naming through the native slash command avoids an extra title-generation
    // inference; the fixed prompt below is the only model submission.
    await paste(`/rename ${THREAD_NAME}`, THREAD_NAME); await write('\r');
    // Native rename is persisted in the local session index, not a rollout
    // event_msg. Use its actual ID to correlate the later durable completion.
    const named = await wait(() => rows(join(env.CODEX_HOME, 'session_index.jsonl'))
      .find(row => row.thread_name === THREAD_NAME && typeof row.id === 'string'), 'native thread rename persisted');
    await paste(PROMPT, PROMPT);
    const currentChoice = label => words(screen.text()).includes(words(label));
    await wait(() => currentChoice(CHOICE.native), 'draft editing preserves default Jev OFF');
    requireLive(!audit().some(row => row.event === 'rpc-client' && row.method === 'mindplayer/prompt/preview'), 'Removed F7 RPC must never be emitted');
    // F6 toggles OFF -> ON -> OFF. Neither press is a submission or an
    // external judgment, even though the ON label discloses transfer.
    for (const choice of [CHOICE.jev, CHOICE.native]) {
      await write(F6); await wait(() => currentChoice(choice), 'session F6 toggle acknowledgement');
      requireLive(!audit().some(row => row.event === 'rpc-client' && row.method === 'turn/start'), 'F6 alone must not submit the native prompt');
      assertNoJev();
    }
    await write('\r'); // Exactly one explicit Native prompt, never replayed.
    const proof = await wait(() => {
      const rollouts = files(env.MINDPLAYER_CODEX_DIR).filter(path => path.endsWith('.jsonl'));
      requireLive(rollouts.length <= 1, 'One owned Native rollout required');
      if (!rollouts.length) return null;
      const within = relative(env.CODEX_HOME, realpathSync(rollouts[0]));
      requireLive(within && within !== '..' && !within.startsWith('..' + sep) && !isAbsolute(within), 'Native rollout must stay inside the isolated account');
      const records = rows(rollouts[0]);
      try { return completedTurn(records, cwd, named.id); }
      catch (error) {
        const events = records.filter(row => row.type === 'event_msg').map(row => row.payload);
        const input = records.filter(row => row.type === 'response_item' && row.payload?.role === 'user');
        const contexts = records.filter(row => row.type === 'turn_context');
        // Fixed counts/booleans only. No text, identifiers, filesystem paths,
        // arbitrary schema keys, or upstream bodies in failure diagnostics.
        t.diagnostic(JSON.stringify({
          legacyInputs: events.filter(row => row.type === 'user_message').length,
          legacyPromptMatches: events.filter(row => row.type === 'user_message' && row.message === PROMPT).length,
          canonicalInputs: input.length,
          canonicalPromptMatches: input.filter(row => row.payload.content?.filter(item => item.type === 'input_text')
            .map(item => item.text).join('\n') === PROMPT).length,
          starts: events.filter(row => row.type === 'task_started').length,
          completions: events.filter(row => row.type === 'task_complete').length,
          contexts: contexts.length,
          modelMatches: contexts.every(row => row.payload.model === BASELINE.model),
          effortMatches: contexts.every(row => row.payload.effort === BASELINE.effort),
          approvalMatches: contexts.every(row => row.payload.approval_policy === APP_PERMISSIONS.approval),
          sandboxMatches: contexts.every(row => row.payload.sandbox_policy?.type === APP_PERMISSIONS.sandbox),
        }));
        throw error;
      }
    }, 'one actual durable Native completion', 90_000);
    await wait(() => audit().some(row => row.event === 'turn-completed' && row.turnId === proof.turnId), 'matching router terminal lifecycle');
    const actualAudit = audit(), starts = actualAudit.filter(row => row.event === 'rpc-client' && row.method === 'turn/start');
    const skipped = actualAudit.filter(row => row.event === 'route-skipped');
    requireLive(starts.length === 1 && skipped.length === 1 && skipped[0].reason === 'prompt-native'
      && skipped[0].requestId === starts[0].requestId && skipped[0].threadId === named.id, 'One explicit per-prompt Native bypass required; no replay');
    const startAcks = actualAudit.filter(row => row.event === 'start-acknowledged');
    requireLive(startAcks.length === 1 && startAcks[0].requestId === starts[0].requestId
      && startAcks[0].threadId === named.id && startAcks[0].turnId === proof.turnId, 'Native bypass ACK must match the durable thread and turn');
    requireLive(actualAudit.filter(row => row.event === 'turn-started').length === 1
      && actualAudit.filter(row => row.event === 'turn-completed').length === 1, 'Exactly one started and terminal lifecycle required');
    requireLive(!actualAudit.some(row => row.event === 'rpc-client'
      && ['turn/steer', 'mindplayer/prompt/preview'].includes(row.method)), 'No steering or removed preview RPC permitted');
    await wait(() => currentChoice(CHOICE.native), 'completed turn keeps this session OFF');
    requireLive(files(join(state, 'runtime')).some(path => basename(path) === 'run.mjs'), 'Supplied installed app must materialize its embedded Codex router');
    const table = discoverGroups();
    requireLive(table.some(row => ownedPids.has(row.pid) && row.command === privateBinary), 'Actual pane must use the isolated manifest-resolved private executable');
    assertNoJev();
    requireLive(rows(join(state, 'audit.jsonl')).some(row => row.kind === 'app_start' && row.run_id === runnerPid), 'Actual supplied MindPlayer process required');
    requireLive((statSync(join(env.CODEX_HOME, 'auth.json')).mode & 0o777) === 0o600, 'Copied auth must remain private');
    t.diagnostic(JSON.stringify({ installedMindPlayer: app, nativeVersion, privateVersion, contract: CONTRACT,
      mindPlayerVersion, appSha256, codexLauncherSha256, binarySha256: manifest.binarySha256,
      codeModeHostSha256: manifest.codeModeHostSha256, ...proof, ...APP_PERMISSIONS,
      perPromptChoice: true, sessionStickyChoice: true, previewRpcCount: 0, actualJevJudgments: 0, routingLogs: 1, privateUi: 1 }));
  } catch (error) {
    throw error instanceof LiveFailure ? error : new LiveFailure('Per-prompt Native live test failed; upstream payload suppressed; no retry');
  }
});
