import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const entry = fileURLToPath(new URL('./startup-check.mjs', import.meta.url));
const SECRET = 'STARTUP_CREDENTIAL_CANARY';
const CONTRACT = 'mindplayer-native-routing-v4';
const contractScript = `#!/bin/sh
if [ "$#" != 1 ] || [ "$1" != "--mindplayer-ui-contract" ]; then exit 23; fi
if [ -n "$AI_GATEWAY_API_KEY$TYPESAFE_API_KEY$VERCEL_OIDC_TOKEN$NODE_OPTIONS$CODEX_HOME" ]; then exit 24; fi
printf '%s\\n' '${CONTRACT}'
`;
const hostScript = '#!/bin/sh\nexit 25\n';

function fixture(t, body = contractScript) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'mindplayer-startup-check-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const home = join(root, 'home');
  const data = join(home, '.mindplayer');
  const install = join(data, 'native-codex');
  const hash = createHash('sha256').update(body).digest('hex');
  const build = join(install, 'builds', hash);
  mkdirSync(build, { recursive: true, mode: 0o700 });
  const binary = join(build, 'codex');
  const host = join(build, 'codex-code-mode-host');
  const manifest = join(install, 'active.json');
  writeFileSync(binary, body, { mode: 0o700 });
  writeFileSync(host, hostScript, { mode: 0o700 });
  writeFileSync(manifest, JSON.stringify({
    schemaVersion: 1, sourceRevision: '092d3acd6bec3e3a14bdc7e7a2810ab628ab759d',
    contract: CONTRACT, version: '0.162.1', binarySha256: hash, binary: `builds/${hash}/codex`,
    codeModeHostSha256: createHash('sha256').update(hostScript).digest('hex'),
    codeModeHost: `builds/${hash}/codex-code-mode-host`,
  }), { mode: 0o600 });
  return { root, home, data, install, binary, host, manifest,
    credentials: join(data, 'jev-credentials.json'), policy: join(data, 'jev-policy.json'),
    calls: join(root, 'calls'), unexpected: join(root, 'unexpected'),
    env: { HOME: home, PATH: `${dirname(process.execPath)}:/usr/bin:/bin`,
      CODEX_HOME: '/account-must-not-be-probed', VERCEL_OIDC_TOKEN: SECRET } };
}

function writeCredentials(f, overrides = {}) {
  writeFileSync(f.credentials, JSON.stringify({ schemaVersion: 1, provider: 'typesafe',
    apiKey: SECRET, externalPromptTransfer: true, consentVersion: 'jev-per-prompt-v1', ...overrides }), { mode: 0o600 });
}

function writePolicy(f, overrides = {}) {
  writeFileSync(f.policy, JSON.stringify({ schemaVersion: 1, version: 'fixture', profiles: [], ...overrides }), { mode: 0o600 });
}

// Patch builtins before the module graph loads. Every test fails if readiness
// attempts network/auth/probe/server work, even if the module catches the error.
function offlineGuard(f) {
  return `
    import cp from 'node:child_process';
    import net from 'node:net';
    import tls from 'node:tls';
    import http from 'node:http';
    import https from 'node:https';
    import dns from 'node:dns';
    import dgram from 'node:dgram';
    import { appendFileSync } from 'node:fs';
    import { syncBuiltinESMExports } from 'node:module';
    const unexpected = kind => {
      appendFileSync(${JSON.stringify(f.unexpected)}, kind + '\\n');
      throw new Error(${JSON.stringify(SECRET)});
    };
    globalThis.fetch = () => unexpected('fetch');
    globalThis.WebSocket = class { constructor() { unexpected('websocket'); } };
    for (const name of ['spawn', 'exec', 'execFile', 'execSync', 'execFileSync', 'fork']) cp[name] = () => unexpected(name);
    const spawnSync = cp.spawnSync;
    cp.spawnSync = (binary, args, options) => {
      if (binary !== ${JSON.stringify(f.binary)} || args.length !== 1 || args[0] !== '--mindplayer-ui-contract') unexpected('probe-or-app-server');
      if (Object.keys(options.env).sort().join(',') !== 'HOME,PATH') unexpected('contract-environment');
      appendFileSync(${JSON.stringify(f.calls)}, 'private-ui-contract\\n');
      return spawnSync(binary, args, options);
    };
    net.connect = net.createConnection = net.Socket.prototype.connect = () => unexpected('socket');
    net.Server.prototype.listen = () => unexpected('listen');
    tls.connect = () => unexpected('tls');
    http.request = http.get = https.request = https.get = () => unexpected('http');
    dns.lookup = dns.resolve = dns.promises.lookup = dns.promises.resolve = () => unexpected('dns');
    dgram.createSocket = () => unexpected('dgram');
    syncBuiltinESMExports();
  `;
}

function runCheck(f, reason = 'ready', extraPreload = '') {
  const expected = { ok: reason === 'ready', reason };
  const result = spawnSync(process.execPath, [
    '--import', `data:text/javascript,${encodeURIComponent(offlineGuard(f) + extraPreload)}`, entry,
  ], { env: f.env, encoding: 'utf8', timeout: 10_000, maxBuffer: 1024 });
  assert.equal(result.error, undefined, 'local readiness must finish within its bound');
  assert.equal(result.status, expected.ok ? 0 : 1);
  assert.equal(result.stdout, `${JSON.stringify(expected)}\n`);
  assert.equal(result.stderr, '');
  assert.ok(Buffer.byteLength(result.stdout) <= 256);
  assert.ok(!`${result.stdout}${result.stderr}`.includes(SECRET), 'no credential or private error body in output');
  assert.ok(!`${result.stdout}${result.stderr}`.includes(f.root), 'no private paths in output');
  assert.equal(existsSync(f.unexpected), false, 'no network, auth, model probe, app-server or listener');
  assert.equal(existsSync(join(f.data, 'routing-status')), false, 'readiness does not allocate a pane channel');
  return existsSync(f.calls) ? readFileSync(f.calls, 'utf8') : '';
}

test('missing dedicated key fails before any private UI execution', t => {
  assert.equal(runCheck(fixture(t), 'jev-key-unavailable'), '');
});

for (const name of ['AI_GATEWAY_API_KEY', 'TYPESAFE_API_KEY']) {
  test(`${name} uses the runtime loader without authentication or any external call`, t => {
    const f = fixture(t);
    f.env[name] = SECRET;
    assert.equal(runCheck(f), 'private-ui-contract\n');
  });
}

for (const provider of ['gateway', 'typesafe']) {
  test(`private stored ${provider} credentials satisfy readiness using the runtime consent format`, t => {
    const f = fixture(t);
    writeCredentials(f, { provider });
    assert.equal(runCheck(f), 'private-ui-contract\n');
  });
}

test('environment credential precedence matches the runtime even with an invalid stored file', t => {
  const f = fixture(t);
  f.env.AI_GATEWAY_API_KEY = SECRET;
  f.env.TYPESAFE_API_KEY = `${SECRET}_SECOND`;
  writeFileSync(f.credentials, `{ ${SECRET}`, { mode: 0o600 });
  assert.equal(runCheck(f), 'private-ui-contract\n');
});

for (const key of ['bad\nkey', '', 'x'.repeat(8193)]) {
  test(`invalid environment key (${key.length} bytes) cannot authorize readiness`, t => {
    const f = fixture(t);
    f.env.AI_GATEWAY_API_KEY = key;
    assert.equal(runCheck(f, 'jev-key-unavailable'), '');
  });
}

for (const overrides of [
  { externalPromptTransfer: false }, { consentVersion: 'unknown' },
  { provider: 'arbitrary-host' }, { apiKey: 'invalid\nkey' },
]) {
  test(`stored credential contract rejects ${Object.keys(overrides)[0]}`, t => {
    const f = fixture(t);
    writeCredentials(f, overrides);
    assert.equal(runCheck(f, 'jev-key-unavailable'), '');
  });
}

test('public stored credentials are unavailable until a valid environment key is present', t => {
  const f = fixture(t);
  writeCredentials(f);
  chmodSync(f.credentials, 0o644);
  assert.equal(runCheck(f, 'jev-key-unavailable'), '');
  f.env.TYPESAFE_API_KEY = SECRET;
  assert.equal(runCheck(f), 'private-ui-contract\n');
});

for (const keyPresent of [true, false]) {
  test(`explicit nested policy veto wins before key/private UI checks (key=${keyPresent})`, t => {
    const f = fixture(t);
    if (keyPresent) f.env.AI_GATEWAY_API_KEY = SECRET;
    writePolicy(f, { consent: { externalPromptTransfer: false } });
    rmSync(f.binary);
    assert.equal(runCheck(f, 'external-transfer-policy-unset'), '');
  });
}

test('a top-level transfer field is not the runtime policy veto', t => {
  const f = fixture(t);
  writeCredentials(f);
  writePolicy(f, { externalPromptTransfer: false });
  assert.equal(runCheck(f), 'private-ui-contract\n');
});

for (const consent of [undefined, { externalPromptTransfer: true }, { externalPromptTransfer: true, version: 'fixture' }]) {
  test(`valid policy with ${consent ? 'explicit' : 'credential'} consent follows runtime transfer permission`, t => {
    const f = fixture(t);
    writeCredentials(f);
    writePolicy(f, { consent });
    assert.equal(runCheck(f), 'private-ui-contract\n');
  });
}

for (const [label, body] of [
  ['malformed JSON', `{ ${SECRET}`], ['null', 'null'], ['array', '[]'],
  ['wrong version', JSON.stringify({ schemaVersion: 2, version: 'fixture', profiles: [] })],
  ['missing profiles', JSON.stringify({ schemaVersion: 1, version: 'fixture' })],
  ['oversized file', 'x'.repeat(129 * 1024)],
]) {
  test(`${label} policy is rejected without printing its body or starting a probe`, t => {
    const f = fixture(t);
    f.env.AI_GATEWAY_API_KEY = SECRET;
    writeFileSync(f.policy, body, { mode: 0o600 });
    assert.equal(runCheck(f, 'policy-invalid'), '');
  });
}

test('unsafe policy permissions and symlinks use the same runtime rejection', t => {
  const f = fixture(t);
  writeCredentials(f);
  writePolicy(f);
  chmodSync(f.policy, 0o644);
  assert.equal(runCheck(f, 'policy-invalid'), '');
  rmSync(f.policy);
  symlinkSync(f.credentials, f.policy);
  assert.equal(runCheck(f, 'policy-invalid'), '');
});

for (const item of ['manifest', 'binary', 'host']) {
  test(`missing private ${item} fails without falling back to PATH or starting app-server`, t => {
    const f = fixture(t);
    writeCredentials(f);
    rmSync(f[item]);
    assert.equal(runCheck(f, 'private-ui-missing'), '');
  });
}

for (const item of ['manifest', 'binary', 'host']) {
  test(`invalid private ${item} fails without exposing its content`, t => {
    const f = fixture(t);
    writeCredentials(f);
    writeFileSync(f[item], `{ ${SECRET}`);
    assert.equal(runCheck(f, 'private-ui-invalid'), '');
  });
}

test('legacy v3 manifests fail before executing even a v4 contract fixture', t => {
  const f = fixture(t);
  writeCredentials(f);
  const manifest = JSON.parse(readFileSync(f.manifest, 'utf8'));
  manifest.contract = 'mindplayer-native-routing-v3';
  writeFileSync(f.manifest, JSON.stringify(manifest));
  assert.equal(runCheck(f, 'private-ui-invalid'), '');
});

test('a legacy v3 executable fails the v4 probe without fallback', t => {
  const f = fixture(t, contractScript.replace(CONTRACT, 'mindplayer-native-routing-v3'));
  writeCredentials(f);
  assert.equal(runCheck(f, 'private-ui-invalid'), 'private-ui-contract\n');
});

test('a contract error containing a credential canary yields only a fixed reason', t => {
  const f = fixture(t, `#!/bin/sh\nprintf '%s\\n' '${SECRET}' >&2\nexit 1\n`);
  writeCredentials(f);
  assert.equal(runCheck(f, 'private-ui-invalid'), 'private-ui-contract\n');
});

test('unrecognized local exceptions are reduced to a fixed generic reason', t => {
  const f = fixture(t);
  writeCredentials(f);
  const preload = `
    import fs from 'node:fs';
    const close = fs.closeSync;
    fs.closeSync = fd => { close(fd); throw new Error(${JSON.stringify(SECRET)}); };
    syncBuiltinESMExports();
  `;
  assert.equal(runCheck(f, 'startup-check-failed', preload), '');
});
