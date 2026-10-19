import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter, getEventListeners } from 'node:events';
import { PassThrough, Writable } from 'node:stream';
import { setTimeout as delay } from 'node:timers/promises';
import { inspect } from 'node:util';
import { createSystemFetch } from './system-fetch.mjs';
import { createJevClient } from './jev.mjs';

const URL = 'https://ai-gateway.vercel.sh/typesafe/v1/systemone';
const KEY = 'test-key-SYSTEM_FETCH_CANARY';
const PRIVATE = 'private-prompt-SYSTEM_FETCH_CANARY';
const HEADERS = 'HTTP/2 200\r\nContent-Type: application/json\r\n\r\n';

function transport({ killCloses = true, platform = 'darwin', spawnError } = {}) {
  const calls = [], kills = [];
  const child = new EventEmitter();
  let config = '';
  let closed = false;
  child.stdin = new Writable({ write(chunk, encoding, callback) { config += chunk.toString(); callback(); } });
  child.stdout = new PassThrough({ highWaterMark: 64 * 1024 });
  const close = (code, signal = null) => {
    if (!closed) { closed = true; child.emit('close', code, signal); }
  };
  child.kill = signal => {
    kills.push(signal);
    if (killCloses || signal === 'SIGKILL') queueMicrotask(() => close(null, signal));
    return true;
  };
  const fetch = createSystemFetch({ platform, spawnImpl(...args) {
    calls.push(args);
    if (spawnError) throw new Error(KEY + PRIVATE);
    return child;
  } });
  return { fetch, child, calls, kills, config: () => config,
    write: value => child.stdout.write(value),
    finish(code = 0) { child.stdout.end(); queueMicrotask(() => close(code)); },
  };
}

function safe(code) {
  return error => {
    assert.equal(error.code, code);
    assert.equal(error.cause, undefined);
    for (const value of [String(error), error.stack, JSON.stringify(error), inspect(error, { showHidden: true })]) {
      assert.ok(!value.includes(KEY));
      assert.ok(!value.includes(PRIVATE));
    }
    return true;
  };
}

test('fixed argv, isolated environment, and stdin-only credentials and payload', async () => {
  const f = transport();
  const previous = process.env.JEV_API_KEY;
  process.env.JEV_API_KEY = KEY;
  let pending;
  try {
    pending = f.fetch(URL, { method: 'POST', headers: { Authorization: `Bearer ${KEY}`, 'Content-Type': 'application/json' }, body: JSON.stringify({ state: PRIVATE }) });
  } finally {
    if (previous === undefined) delete process.env.JEV_API_KEY;
    else process.env.JEV_API_KEY = previous;
  }
  const [executable, args, options] = f.calls[0];
  assert.equal(executable, '/usr/bin/curl');
  assert.equal(args[0], '--disable');
  for (const flag of ['--silent', '--include', '--suppress-connect-headers', '--globoff', '--no-location']) assert.ok(args.includes(flag));
  for (const [flag, value] of [['--proto', '=https'], ['--proto-redir', '=https'], ['--retry', '0'], ['--max-redirs', '0'], ['--config', '-']]) {
    assert.equal(args[args.indexOf(flag) + 1], value);
  }
  assert.ok(!args.includes('--insecure'));
  assert.ok(!args.includes('--verbose'));
  assert.ok(!JSON.stringify(args).includes(KEY));
  assert.ok(!JSON.stringify(args).includes(PRIVATE));
  assert.ok(!JSON.stringify(args).includes(URL));
  assert.deepEqual(options, { shell: false, env: {}, stdio: ['pipe', 'pipe', 'ignore'] });
  assert.ok(f.config().includes(`authorization: Bearer ${KEY}`));
  assert.ok(f.config().includes(PRIVATE));
  f.write(HEADERS + '{}'); f.finish();
  const response = await pending;
  assert.ok(response instanceof Response);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get('content-type'), 'application/json');
  assert.equal(await response.text(), '{}');
  assert.equal(f.calls.length, 1);
});

test('curl config escapes quote, backslash, tab, newline, carriage return, and vertical tab', async () => {
  const f = transport();
  const pending = f.fetch(URL, { method: 'POST', headers: { 'X-Test': 'quote"slash\\' }, body: '"\\\t\n\r\v\nurl = "https://evil.invalid"' });
  const lines = f.config().trimEnd().split('\n');
  assert.equal(lines.length, 4);
  assert.equal(lines[2], 'header = "x-test: quote\\"slash\\\\"');
  assert.equal(lines[3], 'data-binary = "\\"\\\\\\t\\n\\r\\v\\nurl = \\"https://evil.invalid\\""');
  f.write(HEADERS); f.finish();
  await (await pending).text();
});

test('an initial @ stays literal and cannot read a local file', async () => {
  const f = transport();
  const pending = f.fetch(URL, { method: 'POST', body: '@/private/credential-canary' });
  assert.ok(f.config().includes('data-raw = "@/private/credential-canary"'));
  f.write(HEADERS); f.finish(); await (await pending).text();
});

test('rejects unapproved URLs, redirect following, injection, and oversized requests without spawning', async () => {
  const f = transport();
  for (const [url, options] of [
    ['http://ai-gateway.vercel.sh/typesafe/v1/systemone'], ['file:///private/credential-canary'],
    ['https://untrusted.invalid'], [URL + '?secret=' + KEY], [URL + '#fragment'],
    [URL.replace('https://', 'https://user:password@')], [URL, { redirect: 'follow' }],
    [URL, { method: 'POST\nurl = "https://evil.invalid"' }], [URL, { method: 'CONNECT' }],
    [URL, { method: 'POST', body: PRIVATE + '\0' }], [URL, { method: 'GET', body: PRIVATE }],
    [URL, { method: 'POST', body: new Uint8Array([1]) }],
    [URL, { method: 'POST', body: 'x'.repeat(1024 * 1024 + 1) }],
    [URL, { headers: { Authorization: KEY + '\r\nX-Injected: ' + PRIVATE } }],
    [URL, { headers: { 'X-Large': 'x'.repeat(16 * 1024) } }], [URL, { signal: {} }],
  ]) await assert.rejects(f.fetch(url, options), safe('ERR_SYSTEM_FETCH_REQUEST'));
  assert.equal(f.calls.length, 0);
});

test('only macOS can spawn system curl', async () => {
  const f = transport({ platform: 'linux' });
  await assert.rejects(f.fetch(URL), safe('ERR_SYSTEM_FETCH_UNAVAILABLE'));
  assert.equal(f.calls.length, 0);
});

test('fragmented interim headers and body are parsed without exposing interim fields', async () => {
  const f = transport();
  const pending = f.fetch(URL);
  const wire = 'HTTP/1.1 100 Continue\r\nX-Interim: private\r\n\r\nHTTP/2 103\r\nLink: </style>\r\n\r\n' + HEADERS + 'hello 🐈';
  const bytes = Buffer.from(wire);
  for (const byte of bytes) f.write(Buffer.from([byte]));
  f.finish();
  const response = await pending;
  assert.equal(response.headers.get('x-interim'), null);
  assert.equal(response.headers.get('link'), null);
  assert.equal(await response.text(), 'hello 🐈');
});

test('response body streams before curl exits and cancellation kills the child', async () => {
  const f = transport();
  const controller = new AbortController();
  const pending = f.fetch(URL, { signal: controller.signal });
  f.write(HEADERS + 'first');
  const response = await pending;
  const reader = response.body.getReader();
  assert.equal(new TextDecoder().decode((await reader.read()).value), 'first');
  await reader.cancel(new Error(KEY + PRIVATE));
  assert.deepEqual(f.kills, ['SIGTERM']);
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
});

test('slow readers apply backpressure instead of buffering the complete body', async () => {
  const f = transport();
  const pending = f.fetch(URL);
  f.write(HEADERS);
  const response = await pending;
  f.write(Buffer.alloc(64 * 1024));
  assert.equal(f.child.stdout.isPaused(), true);
  await response.body.cancel();
  assert.deepEqual(f.kills, ['SIGTERM']);
});

test('pre-abort prevents spawning and never exposes its reason', async () => {
  const f = transport(), controller = new AbortController();
  controller.abort(new Error(KEY + PRIVATE));
  await assert.rejects(f.fetch(URL, { signal: controller.signal }), safe('ERR_SYSTEM_FETCH_ABORTED'));
  assert.equal(f.calls.length, 0);
});

for (const phase of ['headers', 'body']) {
  test(`abort during ${phase} kills curl and removes the listener`, async () => {
    const f = transport(), controller = new AbortController();
    const pending = f.fetch(URL, { signal: controller.signal });
    let reader;
    if (phase === 'body') { f.write(HEADERS); reader = (await pending).body.getReader(); }
    const failed = assert.rejects(reader ? reader.read() : pending, safe('ERR_SYSTEM_FETCH_ABORTED'));
    controller.abort(new Error(KEY + PRIVATE));
    await failed;
    assert.deepEqual(f.kills, ['SIGTERM']);
    assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  });
}

test('SIGKILL reaps a child that ignores cancellation SIGTERM', { timeout: 1500 }, async () => {
  const f = transport({ killCloses: false });
  const pending = f.fetch(URL);
  f.write(HEADERS);
  await (await pending).body.cancel();
  await delay(300);
  assert.deepEqual(f.kills, ['SIGTERM', 'SIGKILL']);
});

for (const code of [60, 35, 28, 6, 18, 23, null]) {
  test(`curl exit ${code} is sanitized before and after headers without retries`, async () => {
    for (const withHeaders of [false, true]) {
      const f = transport();
      const pending = f.fetch(URL, { method: 'POST', body: PRIVATE, headers: { Authorization: KEY } });
      let response;
      if (withHeaders) { f.write(HEADERS + PRIVATE); response = await pending; }
      const failed = assert.rejects(response ? response.text() : pending,
        safe([60, 35].includes(code) ? 'ERR_TLS_SYSTEM_FETCH' : code === 28 ? 'ERR_SYSTEM_FETCH_TIMEOUT' : 'ERR_SYSTEM_FETCH_NETWORK'));
      f.finish(code);
      await failed;
      assert.equal(f.calls.length, 1);
    }
  });
}

test('spawn, process, stdin, and stdout failures never escape raw errors', async () => {
  const throwing = transport({ spawnError: true });
  await assert.rejects(throwing.fetch(URL), safe('ERR_SYSTEM_FETCH_NETWORK'));
  for (const target of ['child', 'stdin', 'stdout']) {
    const f = transport();
    const pending = f.fetch(URL);
    const failed = assert.rejects(pending, safe('ERR_SYSTEM_FETCH_NETWORK'));
    (target === 'child' ? f.child : f.child[target]).emit('error', new Error(KEY + PRIVATE));
    await failed;
    assert.deepEqual(f.kills, ['SIGTERM']);
  }
});

test('invalid, truncated, oversized, and cumulative interim headers are bounded', async () => {
  for (const wire of [
    PRIVATE + '\r\n\r\n', 'HTTP/2 101\r\n\r\n', 'HTTP/2 200\r\n' + PRIVATE + '\r\n\r\n',
    'HTTP/2 200\r\nX-Large: ' + 'x'.repeat(16 * 1024),
    ('HTTP/2 103\r\nX-Interim: ' + 'x'.repeat(8000) + '\r\n\r\n').repeat(3),
    'HTTP/2 200\r\nContent-Type: application/json',
  ]) {
    const f = transport();
    const pending = f.fetch(URL);
    const failed = assert.rejects(pending, safe('ERR_SYSTEM_FETCH_HEADERS'));
    f.write(wire); f.finish();
    await failed;
  }
});

test('manual redirects are returned once; error redirects kill curl before any follow', async () => {
  for (const redirect of ['manual', 'error']) {
    const f = transport();
    const pending = f.fetch(URL, { redirect });
    const failed = redirect === 'error' ? assert.rejects(pending, safe('ERR_SYSTEM_FETCH_REDIRECT')) : null;
    f.write('HTTP/2 302\r\nLocation: http://untrusted.invalid/' + KEY + '\r\n\r\n');
    if (failed) { await failed; assert.deepEqual(f.kills, ['SIGTERM']); }
    else { f.finish(); const response = await pending; assert.equal(response.status, 302); assert.equal(response.redirected, false); await response.body.cancel(); }
    assert.equal(f.calls.length, 1);
  }
});

test('HEAD and null-body statuses wait for exit, retain headers, and return null bodies', async () => {
  for (const [method, status] of [['HEAD', 405], ['GET', 204], ['GET', 205], ['GET', 304]]) {
    const f = transport(), controller = new AbortController();
    let resolved = false;
    const pending = f.fetch(URL, { method, signal: controller.signal }).then(value => { resolved = true; return value; });
    f.write(`HTTP/2 ${status}\r\nContent-Length: 123\r\n\r\n`);
    await Promise.resolve();
    assert.equal(resolved, false);
    if (method === 'HEAD') assert.ok(f.config().includes('\nhead\n'));
    f.finish();
    const response = await pending;
    assert.equal(response.status, status);
    assert.equal(response.body, null);
    assert.equal(response.headers.get('content-length'), '123');
    assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  }
  const f = transport();
  const pending = f.fetch(URL, { method: 'HEAD' });
  const failed = assert.rejects(pending, safe('ERR_TLS_SYSTEM_FETCH'));
  f.write('HTTP/2 405\r\n\r\n'); f.finish(60); await failed;
});

test('Jev deadlines, response size guards, HTTP rejection, and caller cancellation kill curl', { timeout: 2000 }, async () => {
  for (const scenario of ['header-timeout', 'body-timeout', 'declared-size', 'stream-size', 'http', 'abort', 'tls']) {
    const f = transport(), controller = new AbortController();
    const judge = createJevClient({ apiKey: KEY, fetchImpl: f.fetch, timeoutMs: 30, maxResponseBytes: 8 });
    const pending = judge(PRIVATE, { signal: controller.signal });
    const code = { 'header-timeout': 'timeout', 'body-timeout': 'timeout', 'declared-size': 'response-too-large', 'stream-size': 'response-too-large', http: 'unauthorized', abort: 'aborted', tls: 'tls-error' }[scenario];
    const failed = assert.rejects(pending, safe(code));
    if (scenario === 'body-timeout') f.write(HEADERS + '{');
    if (scenario === 'declared-size') f.write('HTTP/2 200\r\nContent-Length: 99\r\n\r\n');
    if (scenario === 'stream-size') f.write(HEADERS + '123456789');
    if (scenario === 'http') f.write('HTTP/2 401\r\n\r\n');
    if (scenario === 'abort') controller.abort(new Error(KEY + PRIVATE));
    if (scenario === 'tls') f.finish(60);
    await failed;
    if (scenario !== 'tls') assert.deepEqual(f.kills, ['SIGTERM']);
    assert.equal(f.calls.length, 1);
  }
});
