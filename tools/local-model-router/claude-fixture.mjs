// Synthetic native Claude/profile fixture. No inference, plugin loading, or Jev transport.
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const [mode, executable, ...args] = process.argv.slice(2);
const root = process.env.MINDPLAYER_TEST_CLAUDE_ROOT;
if (!root) throw new Error('Claude fixture requires a task-owned root');
let home = process.env.CLAUDE_CONFIG_DIR ?? `${process.env.HOME}/.claude`;
if (!resolve(home).startsWith(`${resolve(root)}/`)) throw new Error('refuse a non-fixture Claude home');
const environment = {};
for (const key of ['HOME', 'CLAUDE_CONFIG_DIR', 'CLAUDE_SECURESTORAGE_CONFIG_DIR', 'ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN', 'CLAUDE_CODE_OAUTH_REFRESH_TOKEN', 'CLAUDE_CODE_OAUTH_SCOPES', 'ANTHROPIC_CUSTOM_HEADERS', 'ANTHROPIC_BASE_URL', 'CLAUDE_CODE_USE_BEDROCK', 'CLAUDE_CODE_USE_VERTEX', 'CLAUDE_CODE_USE_FOUNDRY']) {
  environment[key] = key in process.env ? (key.includes('TOKEN') || key.includes('KEY') ? 'present' : process.env[key]) : null;
}
function record(event, fields = {}) {
  const visibleArgs = mode === 'curl' ? args.map(arg => /^authorization:/i.test(arg) ? 'Authorization: [redacted]' : arg) : args;
  appendFileSync(`${root}/claude-auth-events.jsonl`, `${JSON.stringify({ event, home, executable, args: visibleArgs, cwd: process.cwd(), environment, pid: process.pid, at: Date.now(), ...fields })}\n`);
}
function setting(name, fallback) {
  try { return readFileSync(`${home}/${name}`, 'utf8').trim(); } catch { return fallback; }
}
function signedOut() {
  return existsSync(`${home}/signed-out`) && !existsSync(`${home}/fixture-signed-in`);
}
function savedStatus() {
  const status = JSON.parse(setting('fixture-auth-status.json', JSON.stringify({ loggedIn: false, authMethod: 'none', apiProvider: 'firstParty', analyticsDisabled: false, projectsDirectory: `${home}/projects`, configDirectory: home })));
  return signedOut() ? { ...status, loggedIn: false, authMethod: 'none', email: null, orgId: null, orgName: null, subscriptionType: null } : status;
}
if (mode === 'curl') {
  // The transport is intentionally offline, even for unexpected URLs. Its
  // positive shape is synthetic, not a verified production API contract.
  if (JSON.stringify(args) !== JSON.stringify(['-q', '--config', '-'])) throw new Error('profile must use secret stdin, not credential argv');
  let config = '';
  for await (const chunk of process.stdin) config += chunk.toString('utf8');
  const url = config.match(/^url = "([^"]+)"$/m)?.[1];
  const bearer = config.match(/^header = "Authorization: Bearer ([^"]+)"$/m)?.[1];
  // Route by this exact synthetic token, never an ambient profile variable.
  // Neither stdin nor the token is written to the event log or stdout.
  const tokenPrefix = 'synthetic-offline-token:';
  home = bearer?.startsWith(tokenPrefix) ? bearer.slice(tokenPrefix.length) : undefined;
  if (!home || !resolve(home).startsWith(`${resolve(root)}/home/`) || !existsSync(home)) throw new Error('profile request did not use a fixture account credential');
  if (JSON.parse(readFileSync(`${home}/.credentials.json`, 'utf8')).claudeAiOauth.accessToken !== bearer) throw new Error('profile credential does not belong to its fixture home');
  if (url !== 'https://api.anthropic.com/api/oauth/profile') {
    record('unexpected-transport', { url });
    process.exit(93);
  }
  record('profile-request');
  while (existsSync(`${home}/hold-profile-response`)) await new Promise(done => setTimeout(done, 20));
  const code = Number(setting('profile-http-status', signedOut() ? '401' : '200'));
  const body = code === 200 ? readFileSync(`${home}/fixture-profile.json`, 'utf8') : JSON.stringify({ type: 'error', error: { type: 'authentication_error', message: 'synthetic rejected credential' } });
  record('profile-response', { code, body: JSON.parse(body) });
  process.stdout.write(`${body}\n${code}`);
  process.exit(0);
}
if (args[0] === 'auth' && args[1] === 'status') {
  const status = savedStatus();
  record(args.includes('--json') ? 'status' : 'whoami', { status });
  process.stdout.write(`${JSON.stringify(status)}\n`);
  process.exit(status.loggedIn ? 0 : 1);
}
if (args[0] === 'auth' && args[1] === 'logout') {
  record('logout');
  process.exit(91);
}
if (args[0] === 'auth' && args[1] === 'login') {
  record('login');
  process.on('SIGINT', () => {
    record('login-exit', { code: 130 });
    process.exit(130);
  });
  mkdirSync(home, { recursive: true });
  while (existsSync(`${home}/hold-login`)) await new Promise(done => setTimeout(done, 20));
  const code = Number(setting('login-exit-code', '0'));
  if (code === 0) {
    writeFileSync(`${home}/fixture-signed-in`, 'synthetic native login completed\n');
    writeFileSync(`${home}/.credentials.json`, JSON.stringify({ claudeAiOauth: { accessToken: `synthetic-offline-token:${home}`, expiresAt: 4102444800000 } }));
    if (!existsSync(`${home}/fixture-auth-status.json`)) {
      writeFileSync(`${home}/fixture-auth-status.json`, JSON.stringify({ ...savedStatus(), loggedIn: true, authMethod: 'claude.ai', email: 'fresh@example.test', orgId: 'fixture-org-fresh', orgName: 'Fixture organization', subscriptionType: null }));
      writeFileSync(`${home}/fixture-profile.json`, JSON.stringify({ account: { uuid: 'fixture-user-fresh', email: 'fresh@example.test' }, organization: { uuid: 'fixture-org-fresh' } }));
    }
  }
  record('login-exit', { code });
  process.exit(code);
}
if (args[0] === '--version') {
  process.stdout.write('2.1.302 (Claude Code)\n');
  process.exit(0);
}
if (!process.env.MINDPLAYER_PANE_ID) {
  record('unexpected-command');
  process.exit(92);
}
record('conversation', { pane: process.env.MINDPLAYER_PANE_ID, parentPid: process.ppid,
  launch: {
    brokerEnvironmentKeys: Object.keys(process.env).filter(key => key.startsWith('MINDPLAYER_CLAUDE_JEV_')).sort(),
    privateEnvironment: Object.fromEntries(['TYPESAFE_API_KEY', 'AI_GATEWAY_API_KEY', 'VERCEL_OIDC_TOKEN',
      'MINDPLAYER_LOCAL_ROUTER_TOKEN', 'MINDPLAYER_ROUTER_AVAILABILITY', 'MINDPLAYER_ROUTER_STATUS',
      'MINDPLAYER_PER_PROMPT_CHOICE'].map(key => [key, key in process.env])),
    pluginArgs: args.filter(arg => arg.startsWith('--plugin-dir')),
  } });
process.stdout.write('PANE-READY\n');
if (process.stdin.isTTY) process.stdin.setRawMode(true);
process.stdin.on('data', chunk => {
  record('input', { text: chunk.toString('utf8') });
  process.stdout.write(chunk);
});
process.stdin.resume();
