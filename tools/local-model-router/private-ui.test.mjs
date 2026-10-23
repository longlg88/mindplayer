import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, renameSync, rmSync, symlinkSync, truncateSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { privateUiFailureReason, resolveUiBinary } from './private-ui.mjs';
import { safeFailureReason } from './run.mjs';

const CONTRACT = 'mindplayer-native-routing-v4';
const REVISION = '092d3acd6bec3e3a14bdc7e7a2810ab628ab759d';
const SECRET = 'PRIVATE_UI_CREDENTIAL_CANARY';
const resolverUrl = new URL('./private-ui.mjs', import.meta.url).href;
const runUrl = new URL('./run.mjs', import.meta.url).href;
const hostScript = '#!/bin/sh\nexit 0\n';
const uiScript = contract => `#!/bin/sh
if [ "$1" = "--mindplayer-ui-contract" ]; then
  if [ -n "$AI_GATEWAY_API_KEY$TYPESAFE_API_KEY$VERCEL_OIDC_TOKEN$MINDPLAYER_NATIVE_QUEUE_TEST_MARKER$NODE_OPTIONS" ]; then exit 23; fi
  printf '%s\\n' '${contract}'
  exit 0
fi
exec node --input-type=module -e '
  import { appendFileSync, writeFileSync } from "node:fs";
  import { dirname, join } from "node:path";
  import { createInterface } from "node:readline";
  const args = process.argv.slice(2);
  const calls = process.env.HOME + "/calls";
  if (args.includes("app-server")) {
    appendFileSync(calls, "private-app-server\\n");
    createInterface({ input: process.stdin }).on("line", line => {
      const message = JSON.parse(line);
      if (message.id === undefined) return;
      if (message.method === "model/list" && process.env.TAMPER_PRIVATE_UI === "1") writeFileSync(process.argv[1], "tampered");
      if (message.method === "model/list" && process.env.TAMPER_PRIVATE_HOST === "1") writeFileSync(join(dirname(process.argv[1]), "codex-code-mode-host"), "tampered");
      const result = message.method === "model/list" ? { data: [
        { id: "simple", model: "simple", isDefault: true, inputModalities: ["text"] },
        { id: "complex", model: "complex", inputModalities: ["text"] }
      ], nextCursor: null } : message.method === "account/read" ? { account: null } : {};
      process.stdout.write(JSON.stringify({ id: message.id, result }) + "\\n");
    });
  } else {
    appendFileSync(calls, "private-ui\\n");
    appendFileSync(process.env.HOME + "/ui-jev-default", (process.env.MINDPLAYER_JEV_DEFAULT ?? "unset") + "\\n");
    const ws = new WebSocket(args[args.indexOf("--remote") + 1], { headers: { Authorization: "Bearer " + process.env.MINDPLAYER_LOCAL_ROUTER_TOKEN } });
    ws.addEventListener("open", () => ws.send(JSON.stringify({ id: 1, method: "initialize", params: {} })));
    ws.addEventListener("message", () => { ws.close(); });
  }
' -- "$0" "$@"
`;

function fixture(t, body = uiScript(CONTRACT)) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'mindplayer-private-ui-test-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const home = join(root, 'home'), bin = join(root, 'bin'), temp = join(root, 'tmp');
  for (const path of [home, bin, temp]) mkdirSync(path, { mode: 0o700 });
  const install = join(home, '.mindplayer', 'native-codex');
  const digest = createHash('sha256').update(body).digest('hex');
  const build = join(install, 'builds', digest);
  mkdirSync(build, { recursive: true, mode: 0o700 });
  const binary = join(build, 'codex'), codeModeHost = join(build, 'codex-code-mode-host'), manifestPath = join(install, 'active.json');
  writeFileSync(binary, body, { mode: 0o755 });
  writeFileSync(codeModeHost, hostScript, { mode: 0o700 });
  const manifest = { schemaVersion: 1, sourceRevision: REVISION, contract: CONTRACT, version: '0.162.1',
    binarySha256: digest, binary: `builds/${digest}/codex`,
    codeModeHostSha256: createHash('sha256').update(hostScript).digest('hex'), codeModeHost: `builds/${digest}/codex-code-mode-host` };
  const save = () => writeFileSync(manifestPath, JSON.stringify(manifest), { mode: 0o600 });
  save();
  const calls = join(home, 'calls');
  writeFileSync(join(bin, 'codex'), `#!/usr/bin/env node
const { appendFileSync } = require('node:fs');
const args = process.argv.slice(2);
const calls = process.env.HOME + '/calls';
if (args.includes('app-server')) {
  appendFileSync(calls, 'global-app-server\\n');
  require('node:readline').createInterface({ input: process.stdin }).on('line', line => {
    const message = JSON.parse(line);
    if (message.id === undefined) return;
    const result = message.method === 'model/list' ? { data: [
      { id: 'simple', model: 'simple', isDefault: true, inputModalities: ['text'] },
      { id: 'complex', model: 'complex', inputModalities: ['text'] }
    ], nextCursor: null } : message.method === 'account/read' ? { account: null } : {};
    process.stdout.write(JSON.stringify({ id: message.id, result }) + '\\n');
  });
} else if (args.includes('--remote')) {
  appendFileSync(calls, 'global-ui\\n');
  const ws = new WebSocket(args[args.indexOf('--remote') + 1], { headers: { Authorization: 'Bearer ' + process.env.MINDPLAYER_LOCAL_ROUTER_TOKEN } });
  ws.addEventListener('open', () => ws.send(JSON.stringify({ id: 1, method: 'initialize', params: {} })));
  ws.addEventListener('message', () => { ws.close(); });
}
else { appendFileSync(calls, 'global-unexpected\\n'); process.exitCode = 24; }
`, { mode: 0o755 });
  const env = { HOME: home, PATH: `${bin}:${dirname(process.execPath)}:/usr/bin:/bin`, TMPDIR: temp,
    AI_GATEWAY_API_KEY: SECRET, TYPESAFE_API_KEY: SECRET, VERCEL_OIDC_TOKEN: SECRET,
    MINDPLAYER_NATIVE_QUEUE_TEST_MARKER: SECRET, NODE_OPTIONS: '--no-warnings' };
  return { root, home, install, build, binary, codeModeHost, manifestPath, manifest, save, env, calls };
}

function resolve(f, integrated = true) {
  const result = spawnSync(process.execPath, ['--input-type=module', '--eval', `
    import { resolveUiBinary, privateUiFailureReason } from ${JSON.stringify(resolverUrl)};
    try { console.log(JSON.stringify({ binary: resolveUiBinary({ integrated: ${integrated} }) })); }
    catch (error) { console.log(JSON.stringify({ code: error.code, reason: privateUiFailureReason(error), stack: error.stack })); }
  `], { env: f.env, encoding: 'utf8', timeout: 10_000 });
  assert.equal(result.status, 0);
  assert.equal(result.stderr, '');
  assert.ok(!result.stdout.includes(SECRET), 'resolver diagnostics must not retain credential payloads');
  return JSON.parse(result.stdout);
}

function launch(f, integrated, { tty = false, extraEnv = {}, preload } = {}) {
  const args = [...(integrated ? ['--mindplayer'] : []), '--cwd', f.home];
  const script = tty ? `
    Object.defineProperty(process.stdin, 'isTTY', { value: true });
    Object.defineProperty(process.stdout, 'isTTY', { value: true });
    process.argv = ['node', 'fixture', ...${JSON.stringify(args)}];
    const { main, safeFailureReason } = await import(${JSON.stringify(runUrl)});
    try { await main(); }
    catch (error) { console.error(safeFailureReason(error)); process.exitCode = 1; }
  ` : null;
  const nodeArgs = script ? ['--input-type=module', '--eval', script] : [fileURLToPath(runUrl), ...args, '--list-models'];
  if (preload) nodeArgs.unshift('--import', `data:text/javascript,${encodeURIComponent(preload)}`);
  const result = spawnSync(process.execPath, nodeArgs, {
    env: { ...f.env, ...extraEnv }, encoding: 'utf8', timeout: 10_000,
  });
  assert.equal(result.error, undefined, 'launcher must finish within its bound');
  assert.ok(!`${result.stdout}${result.stderr}`.includes(SECRET), 'launcher diagnostics must not print credentials');
  return result;
}

function assertFailure(f, code) {
  const result = resolve(f);
  assert.equal(result.code, code);
  assert.equal(result.reason, privateUiFailureReason({ code }));
  assert.ok(!result.stack.includes(f.root), 'failure must not include private paths');
  assert.equal(existsSync(f.calls), false, 'a resolver failure must never spawn a Codex probe, backend or UI');
}

test('approved eight-field manifest resolves the hashed private UI and sibling host with a clean contract environment', t => {
  const f = fixture(t);
  assert.deepEqual(resolve(f), { binary: f.binary });
  assert.equal(existsSync(f.calls), false);
});

for (const legacy of ['mindplayer-native-queue-v1', 'mindplayer-native-routing-v1', 'mindplayer-native-routing-v2', 'mindplayer-native-routing-v3']) {
  test(`rejects legacy ${legacy} manifests before executing any CLI`, t => {
    const f = fixture(t, uiScript(legacy));
    f.manifest.contract = legacy; f.save();
    assertFailure(f, 'ERR_PRIVATE_UI_MANIFEST');
  });

  test(`rejects a legacy ${legacy} CLI even beneath a v4 manifest`, t => {
    assertFailure(fixture(t, uiScript(legacy)), 'ERR_PRIVATE_UI_CONTRACT');
  });
}

for (const item of ['manifest', 'code-mode host']) {
  test(`missing private ${item} fails before credentials or any Codex probe are loaded`, t => {
    const f = fixture(t);
    rmSync(item === 'manifest' ? f.manifestPath : f.codeModeHost);
    const credentials = join(f.home, '.mindplayer', 'jev-credentials.json');
    writeFileSync(credentials, SECRET, { mode: 0o600 });
    assertFailure(f, 'ERR_PRIVATE_UI_MISSING');
    const result = launch(f, true, {
      extraEnv: { AI_GATEWAY_API_KEY: '', TYPESAFE_API_KEY: '', VERCEL_OIDC_TOKEN: '' },
      preload: `
        import fs from 'node:fs';
        import { syncBuiltinESMExports } from 'node:module';
        const open = fs.openSync;
        fs.openSync = (path, ...args) => {
          if (path === ${JSON.stringify(credentials)}) fs.writeFileSync(${JSON.stringify(f.calls)}, 'credential-load');
          return open(path, ...args);
        };
        syncBuiltinESMExports();
      `,
    });
    assert.equal(result.status, 1);
    assert.equal(result.stdout, '');
    assert.match(result.stderr, /^로컬 라우터 전용 Codex UI 확인 실패: MindPlayer 전용 Codex 설치 파일이 없습니다\./);
    assert.ok(result.stderr.length < 250);
    assert.equal(existsSync(f.calls), false);
  });
}

test('tampered binary fails its hash check before executing its contract flag', t => {
  const f = fixture(t);
  writeFileSync(f.binary, `#!/bin/sh\nprintf '%s\\n' '${SECRET}' >&2\nexit 1\n`);
  assertFailure(f, 'ERR_PRIVATE_UI_HASH');
  const result = launch(f, true);
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, /무결성 확인에 실패했습니다/);
});

test('tampered code-mode host fails its hash check before executing the CLI contract flag', t => {
  const f = fixture(t, `#!/bin/sh\nprintf '%s\\n' '${SECRET}' >&2\nexit 1\n`);
  writeFileSync(f.codeModeHost, `#!/bin/sh\nprintf '%s\\n' '${SECRET}' >&2\nexit 1\n`);
  assertFailure(f, 'ERR_PRIVATE_UI_HASH');
  const result = launch(f, true);
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, /무결성 확인에 실패했습니다/);
});

for (const contract of ['unpatched-global-codex', `${CONTRACT} extra`, ` ${CONTRACT}`, `${CONTRACT}\n${SECRET}`]) {
  test(`rejects a binary with an inexact contract (${JSON.stringify(contract.replace(SECRET, 'canary'))})`, t => {
    assertFailure(fixture(t, uiScript(contract)), 'ERR_PRIVATE_UI_CONTRACT');
  });
}

for (const [field, value] of [
  ['schemaVersion', 2], ['sourceRevision', '0'.repeat(40)], ['contract', 'other'], ['version', '0.162.2'],
  ['binarySha256', '../outside'], ['binary', '../outside/codex'], ['binary', '/usr/bin/codex'],
  ['binary', 'builds/other/codex'], ['patchSha', 'receipt-only'],
  ['codeModeHostSha256', '../outside'], ['codeModeHostSha256', 'A'.repeat(64)],
  ['codeModeHostSha256', undefined], ['codeModeHost', undefined],
  ['codeModeHost', '../outside/codex-code-mode-host'], ['codeModeHost', '/usr/bin/codex-code-mode-host'],
  ['codeModeHost', `builds/${'0'.repeat(64)}/codex-code-mode-host`],
]) {
  test(`rejects invalid runtime manifest field ${field}=${value}`, t => {
    const f = fixture(t);
    f.manifest[field] = value; f.save();
    assertFailure(f, 'ERR_PRIVATE_UI_MANIFEST');
  });
}

test('legacy six-field manifests cannot activate a CLI without its required host', t => {
  const f = fixture(t);
  delete f.manifest.codeModeHostSha256;
  delete f.manifest.codeModeHost;
  f.save();
  assertFailure(f, 'ERR_PRIVATE_UI_MANIFEST');
});

test('invalid JSON cannot expose its body in failure diagnostics', t => {
  const f = fixture(t);
  writeFileSync(f.manifestPath, `{ ${SECRET}`);
  assertFailure(f, 'ERR_PRIVATE_UI_MANIFEST');
});

for (const item of ['manifest', 'binary', 'codeModeHost', 'build', 'builds', 'install', 'data']) {
  test(`rejects a symbolic link at the private ${item} path`, t => {
    const f = fixture(t);
    const path = { manifest: f.manifestPath, binary: f.binary, codeModeHost: f.codeModeHost, build: f.build,
      builds: join(f.install, 'builds'), install: f.install, data: join(f.home, '.mindplayer') }[item];
    const moved = join(f.root, 'moved');
    renameSync(path, moved);
    symlinkSync(moved, path);
    assertFailure(f, 'ERR_PRIVATE_UI_PATH');
  });
}

for (const [item, mode] of [
  ['manifest', 0o644], ['binary', 0o644], ['binary', 0o777], ['binary', 0o4755], ['build', 0o777],
  ['codeModeHost', 0o644], ['codeModeHost', 0o777], ['codeModeHost', 0o4755], ['codeModeHost', 0o2755],
]) {
  test(`rejects unsafe ${item} permissions ${mode.toString(8)}`, t => {
    const f = fixture(t);
    chmodSync({ manifest: f.manifestPath, binary: f.binary, codeModeHost: f.codeModeHost, build: f.build }[item], mode);
    assertFailure(f, 'ERR_PRIVATE_UI_PATH');
  });
}

test('oversized manifest and binary are rejected before parsing, hashing or execution', t => {
  const f = fixture(t);
  truncateSync(f.manifestPath, 16 * 1024 + 1);
  assertFailure(f, 'ERR_PRIVATE_UI_PATH');
  f.save();
  truncateSync(f.binary, 512 * 1024 * 1024 + 1);
  assertFailure(f, 'ERR_PRIVATE_UI_PATH');
});

test('a directory cannot stand in for the required regular code-mode host executable', t => {
  const f = fixture(t);
  rmSync(f.codeModeHost);
  mkdirSync(f.codeModeHost, { mode: 0o700 });
  assertFailure(f, 'ERR_PRIVATE_UI_PATH');
});

for (const size of [0, 512 * 1024 * 1024 + 1]) {
  test(`rejects a code-mode host with an unsafe byte size of ${size}`, t => {
    const f = fixture(t);
    truncateSync(f.codeModeHost, size);
    assertFailure(f, 'ERR_PRIVATE_UI_PATH');
  });
}

test('contract output, exit status and execution time are bounded', { timeout: 15_000 }, t => {
  for (const body of [
    `#!/bin/sh\nprintf '%s\\n' '${CONTRACT}'; exit 1\n`,
    `#!/bin/sh\nprintf '%s\\n' '${CONTRACT}'; printf '%s' '${SECRET}' >&2\n`,
    `#!/bin/sh\nexec node -e 'process.stdout.write("x".repeat(2048))'\n`,
    `#!/bin/sh\nexec node -e 'setInterval(() => {}, 1000)'\n`,
  ]) assertFailure(fixture(t, body), 'ERR_PRIVATE_UI_CONTRACT');
});

test('integrated launch uses the verified private binary for probe, UI and app-server', t => {
  const f = fixture(t);
  const result = launch(f, true, { tty: true });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(readFileSync(f.calls, 'utf8').trim().split('\n'), ['private-app-server', 'private-ui', 'private-app-server']);
});

test('managed launch advertises the session F6 toggle without F7 or per-input reset instructions', t => {
  const f = fixture(t);
  const result = launch(f, true, { tty: true, extraEnv: { MINDPLAYER_PER_PROMPT_CHOICE: '1' } });
  assert.equal(result.status, 0);
  assert.ok(result.stdout.split('\n').includes('세션별 Jev · 새 세션 ON · F6 ON/OFF · Enter 전송'), result.stdout);
  assert.equal(readFileSync(join(f.home, 'ui-jev-default'), 'utf8'), 'jev\n', 'a loaded key starts the session ON');
  assert.doesNotMatch(result.stdout, /F7|매 프롬프트|다시 선택/);
  assert.deepEqual(readFileSync(f.calls, 'utf8').trim().split('\n'), ['private-app-server', 'private-ui', 'private-app-server']);
});

for (const [item, flag] of [['binary', 'TAMPER_PRIVATE_UI'], ['code-mode host', 'TAMPER_PRIVATE_HOST']]) {
  test(`integrated launch rechecks a ${item} tampered during the private model probe without spawning UI`, t => {
    const f = fixture(t);
    const result = launch(f, true, { tty: true, extraEnv: { [flag]: '1' } });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /무결성 확인에 실패했습니다/);
    assert.equal(readFileSync(f.calls, 'utf8'), 'private-app-server\n');
  });
}

test('standalone UI, model probes and app-server use global Codex without any private install', t => {
  const f = fixture(t);
  rmSync(f.install, { recursive: true });
  assert.deepEqual(resolve(f, false), { binary: 'codex' });
  assert.equal(resolveUiBinary(), 'codex');
  const result = launch(f, false, { tty: true });
  assert.equal(result.status, 0);
  assert.deepEqual(readFileSync(f.calls, 'utf8').trim().split('\n'), ['global-app-server', 'global-ui', 'global-app-server']);
});

test('failure-reason mapping accepts only bounded codes and never arbitrary exception messages', () => {
  assert.equal(privateUiFailureReason({ code: SECRET, message: SECRET }), undefined);
  assert.equal(safeFailureReason({ code: 'ERR_PRIVATE_UI_HASH', message: SECRET }), '전용 Codex 파일 무결성 확인에 실패했습니다');
});

test('without a Jev key the managed session starts OFF and an inherited default never reaches the UI', t => {
  const f = fixture(t);
  for (const key of ['AI_GATEWAY_API_KEY', 'TYPESAFE_API_KEY', 'VERCEL_OIDC_TOKEN']) delete f.env[key];
  const result = launch(f, true, { tty: true, extraEnv: { MINDPLAYER_PER_PROMPT_CHOICE: '1', MINDPLAYER_JEV_DEFAULT: 'jev' } });
  assert.equal(result.status, 0, result.stderr);
  assert.ok(result.stdout.split('\n').includes('세션별 Jev · 키 없음 · 새 세션 OFF · F6 ON/OFF · Enter 전송'), result.stdout);
  assert.equal(readFileSync(join(f.home, 'ui-jev-default'), 'utf8'), 'unset\n');
});
