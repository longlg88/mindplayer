// Input integration only: real outer MindPlayer and native composer, synthetic
// account/catalog and backend. The private launcher redirects the native UI to
// native-queue-fixture's loopback backend; this is not a routing/inference test.
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { catalog } from './native-queue-fixture.mjs';

const quote = value => `'${value.replaceAll("'", "'\\''")}'`;
const digest = value => createHash('sha256').update(value).digest('hex');
const account = { account: { type: 'chatgpt', email: 'native-queue@example.invalid', planType: 'business' },
  workspaceRouting: { chatgptAccountId: 'offline-input-account' }, requiresOpenaiAuth: false };

export function outerMindplayerLaunch(appBinary) {
  return ({ root, binary, args, cwd, env }) => {
    const app = resolve(appBinary);
    const home = env.HOME, data = join(home, '.mindplayer'), bin = join(root, 'bin');
    const markers = join(root, 'input-processes.jsonl');
    const helper = fileURLToPath(import.meta.url);
    for (const directory of [data, bin, join(home, '.codex'), ...['state', 'audit', 'prompts', 'handoffs',
      'codex-sessions', 'claude', 'kiro', 'cursor', 'omp'].map(name => join(root, name))]) {
      mkdirSync(directory, { recursive: true, mode: 0o700 });
    }
    const backend = `exec ${quote(process.execPath)} ${quote(helper)} --backend ${quote(markers)}\n`;
    const wrapper = '#!/bin/sh\n'
      + `if [ "$1" = '--mindplayer-ui-contract' ]; then exec ${quote(binary)} "$@"; fi\n`
      + `case " $* " in *' app-server '*) ${backend};; esac\n`
      + `printf '{"role":"native","pid":%s}\\n' "$$" >> ${quote(markers)}\n`
      + `printf '{"role":"router","pid":%s}\\n' "$PPID" >> ${quote(markers)}\n`
      + `export NATIVE_QUEUE_REMOTE_TOKEN=${quote(env.NATIVE_QUEUE_REMOTE_TOKEN)}\n`
      + `exec ${[binary, ...args].map(quote).join(' ')}\n`;
    const host = '#!/bin/sh\nexit 97\n'; // Tools are outside this offline input test.
    const sha = digest(wrapper), hostSha = digest(host);
    const bundle = join(data, 'native-codex'), build = join(bundle, 'builds', sha);
    mkdirSync(build, { recursive: true, mode: 0o700 });
    writeFileSync(join(build, 'codex'), wrapper, { mode: 0o700 });
    writeFileSync(join(build, 'codex-code-mode-host'), host, { mode: 0o700 });
    writeFileSync(join(bundle, 'active.json'), JSON.stringify({ schemaVersion: 1,
      sourceRevision: '092d3acd6bec3e3a14bdc7e7a2810ab628ab759d', contract: 'mindplayer-native-routing-v4',
      version: '0.162.1', binarySha256: sha, binary: `builds/${sha}/codex`,
      codeModeHostSha256: hostSha, codeModeHost: `builds/${sha}/codex-code-mode-host` }), { mode: 0o600 });
    writeFileSync(join(bin, 'codex'), '#!/bin/sh\n' + backend, { mode: 0o700 });
    // No executable in this fixture may reach a real provider, keychain or OS
    // credential transport, including background account/usage discovery.
    for (const name of ['claude', 'kiro-cli', 'cursor-agent', 'agent', 'omp', 'pi', 'security', 'curl']) {
      writeFileSync(join(bin, name), '#!/bin/sh\nexit 97\n', { mode: 0o700 });
    }
    const identity = JSON.parse(execFileSync(app, ['--mindplayer-verify-codex-identity'], {
      env, encoding: 'utf8', input: JSON.stringify({ accountRead: { result: account }, quotaRead: null }),
    })).fingerprint;
    writeFileSync(join(data, 'accounts.json'), JSON.stringify([{ provider: 'codex', name: 'default',
      slot: { kind: 'inherited' }, role: 'primary', disabled: false, codex_identity: identity }]), { mode: 0o600 });
    const launchedEnv = { ...env, PATH: `${bin}:${dirname(process.execPath)}:/usr/bin:/bin`,
      XDG_CACHE_HOME: join(home, '.cache'), XDG_DATA_HOME: join(home, '.local/share'),
      CODEX_HOME: join(home, '.codex'), PI_CODING_AGENT_DIR: join(root, 'omp'),
      MINDPLAYER_CODEX_DIR: join(root, 'codex-sessions'), MINDPLAYER_CLAUDE_DIR: join(root, 'claude'),
      MINDPLAYER_KIRO_DIR: join(root, 'kiro'), MINDPLAYER_CURSOR_DIR: join(root, 'cursor'),
      MINDPLAYER_OMP_DIR: join(root, 'omp'), MINDPLAYER_STATE: join(root, 'state'),
      MINDPLAYER_AUDIT: join(root, 'audit'), MINDPLAYER_PROMPTS_DIR: join(root, 'prompts'),
      MINDPLAYER_HANDOFF_DIR: join(root, 'handoffs') };
    return { binary: app, args: [cwd], cwd, env: launchedEnv,
      evidence: { binary: app, binarySha256: digest(readFileSync(app)),
        version: execFileSync(app, ['--version'], { env, encoding: 'utf8' }).trim(),
        scope: 'Actual outer MindPlayer PTY -> actual installed native Codex composer -> synthetic loopback backend. Private launcher redirects the backend, not input bytes. No auth, inference, physical IME or production-router claim.' },
      ownedPids: () => existsSync(markers) ? readFileSync(markers, 'utf8').trim().split('\n').filter(Boolean)
        .map(line => JSON.parse(line).pid) : [],
      async shutdown(write) { await write('\x18'); await write('q'); },
    };
  };
}

export async function openOuterNative(fixture) {
  const f = fixture;
  await f.wait(() => f.screen().includes('collect sessions'), 'outer scope picker');
  await f.write('\r');
  await f.wait(() => f.screen().includes('multi-select'), 'outer main list');
  await f.write('n');
  await f.wait(() => f.screen().includes('New session'), 'outer new-session picker');
  await f.write('\r');
  await f.wait(() => f.screen().includes('Label / subject (optional)'), 'outer optional label');
  await f.write('\r');
  await f.wait(() => f.messages.some(message => message.method === 'thread/start'), 'actual native startup');
  await f.wait(() => f.projectedScreen().includes('Ask Codex to do anything'), 'native composer inside outer pane');
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  if (process.argv[2] !== '--backend') throw new Error('Only the synthetic backend entrypoint is supported');
  const markers = process.argv[3];
  appendFileSync(markers, `${JSON.stringify({ role: 'backend', pid: process.pid })}\n`, { mode: 0o600 });
  const input = createInterface({ input: process.stdin });
  input.on('line', line => {
    const message = JSON.parse(line);
    if (message.id === undefined) return;
    let result;
    if (message.method === 'initialize') result = { userAgent: 'offline-input-fixture' };
    else if (message.method === 'model/list') result = { data: catalog, nextCursor: null };
    else if (message.method === 'account/read') result = account;
    else if (message.method === 'account/rateLimits/read') result = { accountId: 'offline-input-account',
      rateLimits: { primary: { usedPercent: 0, windowDurationMins: 10080 } }, ordinaryUsageAllowed: true };
    else {
      process.stdout.write(`${JSON.stringify({ id: message.id, error: { code: -32601, message: 'Unsupported offline probe' } })}\n`);
      return;
    }
    process.stdout.write(`${JSON.stringify({ id: message.id, result })}\n`);
  });
}
