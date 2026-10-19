import { spawn } from 'node:child_process';

const ENDPOINTS = new Set([
  'https://ai-gateway.vercel.sh/typesafe/v1/systemone',
  'https://api.typesafe.ai/v1/systemone',
]);
const HEADER_LIMIT = 16 * 1024;
const CONFIG_LIMIT = 1024 * 1024;
const TIMEOUT_MS = 60_000;
// --disable must be first: user curlrc files must never affect this transport.
const ARGS = Object.freeze([
  '--disable', '--silent', '--no-buffer', '--include', '--suppress-connect-headers',
  '--globoff', '--proto', '=https', '--proto-redir', '=https',
  '--no-location', '--max-redirs', '0', '--retry', '0',
  '--connect-timeout', '10', '--max-time', '60', '--config', '-',
]);
const MESSAGES = Object.freeze({
  ERR_SYSTEM_FETCH_REQUEST: 'Invalid system fetch request',
  ERR_SYSTEM_FETCH_UNAVAILABLE: 'System fetch is unavailable',
  ERR_SYSTEM_FETCH_HEADERS: 'Invalid or oversized system fetch headers',
  ERR_SYSTEM_FETCH_REDIRECT: 'System fetch redirect refused',
  ERR_SYSTEM_FETCH_NETWORK: 'System fetch transport failed',
  ERR_SYSTEM_FETCH_TIMEOUT: 'System fetch timed out',
  ERR_SYSTEM_FETCH_ABORTED: 'System fetch cancelled',
  ERR_TLS_SYSTEM_FETCH: 'System fetch TLS verification failed',
});

function safeError(code) {
  const error = new Error(MESSAGES[code]);
  error.name = code === 'ERR_SYSTEM_FETCH_ABORTED' ? 'AbortError' : 'SystemFetchError';
  Object.defineProperty(error, 'code', { value: code, enumerable: true });
  return error;
}

// curl config quoting is not JSON quoting. These are its six supported escapes:
// https://curl.se/docs/manpage.html#--config
function quote(value) {
  const escapes = { '\\': '\\\\', '"': '\\"', '\t': '\\t', '\n': '\\n', '\r': '\\r', '\v': '\\v' };
  return `"${value.replace(/[\\"\t\n\r\v]/g, character => escapes[character])}"`;
}

function requestConfig(url, { method = 'GET', headers, body, redirect = 'error', signal } = {}) {
  try {
    const href = typeof url === 'string' ? url : url instanceof URL ? url.href : '';
    if (!ENDPOINTS.has(href) || !['error', 'manual'].includes(redirect)
      || (signal !== undefined && !(signal instanceof AbortSignal))
      || typeof method !== 'string' || !/^[!#$%&'*+.^_`|~0-9A-Za-z-]{1,32}$/.test(method)) throw new Error();
    method = method.toUpperCase();
    if (['CONNECT', 'TRACE', 'TRACK'].includes(method)
      || (body != null && (typeof body !== 'string' || body.includes('\0')
        || Buffer.byteLength(body) > CONFIG_LIMIT || ['GET', 'HEAD'].includes(method)))) throw new Error();
    const lines = [`url = ${quote(href)}`, `request = ${quote(method)}`];
    if (method === 'HEAD') lines.push('head');
    let headerBytes = 0;
    for (const [name, value] of new Headers(headers)) {
      headerBytes += Buffer.byteLength(name) + Buffer.byteLength(value) + 4;
      if (headerBytes > HEADER_LIMIT || /[\x00-\x1f\x7f]/.test(value)) throw new Error();
      lines.push(`header = ${quote(`${name}: ${value}`)}`);
    }
    // data-raw prevents an initial @ from becoming a local-file read.
    if (body != null) lines.push(`${body.startsWith('@') ? 'data-raw' : 'data-binary'} = ${quote(body)}`);
    const config = lines.join('\n') + '\n';
    if (Buffer.byteLength(config) > CONFIG_LIMIT) throw new Error();
    return { config, method, redirect, signal };
  } catch {
    throw safeError('ERR_SYSTEM_FETCH_REQUEST');
  }
}

function parseHeaders(buffer) {
  const [statusLine, ...lines] = buffer.toString('latin1').split('\r\n');
  const match = /^HTTP\/(?:1\.[01]|2|3) ([1-5][0-9]{2})(?: [\t\x20-\xff]*)?$/.exec(statusLine);
  if (!match) throw new Error();
  const status = Number(match[1]);
  if (status === 101) throw new Error();
  const headers = new Headers();
  for (const line of lines) {
    const colon = line.indexOf(':');
    if (colon < 1 || /^[ \t]/.test(line)) throw new Error();
    headers.append(line.slice(0, colon), line.slice(colon + 1).trim());
  }
  return { status, headers };
}

function exitError(code) {
  if ([35, 51, 58, 59, 60, 64, 66, 77, 80, 82, 83, 90, 91, 98].includes(code)) return 'ERR_TLS_SYSTEM_FETCH';
  return code === 28 ? 'ERR_SYSTEM_FETCH_TIMEOUT' : 'ERR_SYSTEM_FETCH_NETWORK';
}

// The injection seam keeps tests offline without replacing global fetch/spawn.
export function createSystemFetch({ spawnImpl = spawn, platform = process.platform } = {}) {
  return async function systemFetch(url, options) {
    const request = requestConfig(url, options);
    if (request.signal?.aborted) throw safeError('ERR_SYSTEM_FETCH_ABORTED');
    if (platform !== 'darwin') throw safeError('ERR_SYSTEM_FETCH_UNAVAILABLE');

    return new Promise((resolve, reject) => {
      let child, deadline, killTimer, controller, response;
      let stopped = false, settled = false, closed = false, ended = false;
      let headerBytes = 0;
      let headerBuffer = Buffer.alloc(0);
      const onAbort = () => fail('ERR_SYSTEM_FETCH_ABORTED');
      const cleanup = () => {
        clearTimeout(deadline);
        request.signal?.removeEventListener('abort', onAbort);
      };
      const terminate = () => {
        child?.stdin.destroy();
        child?.stdout.destroy();
        if (!child || closed || killTimer) return;
        try { child.kill('SIGTERM'); } catch { /* Never forward raw OS errors. */ }
        killTimer = setTimeout(() => {
          if (!closed) { try { child.kill('SIGKILL'); } catch { /* Already reaped. */ } }
        }, 250);
        killTimer.unref();
      };
      function fail(code) {
        if (stopped) return;
        stopped = true;
        cleanup();
        terminate();
        if (!settled) { settled = true; reject(safeError(code)); }
        else controller?.error(safeError(code));
      }
      const finish = () => {
        if (stopped || !ended || !closed) return;
        if (!response) { fail('ERR_SYSTEM_FETCH_HEADERS'); return; }
        stopped = true;
        cleanup();
        if (!settled) { settled = true; resolve(response); }
        else controller.close();
      };
      const enqueue = chunk => {
        if (!chunk.length) return;
        if (!controller) { fail('ERR_SYSTEM_FETCH_HEADERS'); return; }
        controller.enqueue(chunk);
        if (controller.desiredSize <= 0) child.stdout.pause();
      };
      const onData = chunk => {
        if (stopped) return;
        if (response) { enqueue(chunk); return; }
        while (chunk.length && !stopped) {
          // Never retain more than 16 KiB, including all interim header blocks.
          const capacity = HEADER_LIMIT - headerBytes - headerBuffer.length;
          const prefix = chunk.subarray(0, capacity);
          const buffer = Buffer.concat([headerBuffer, prefix]);
          const boundary = buffer.indexOf('\r\n\r\n');
          if (boundary === -1) {
            if (prefix.length === capacity) fail('ERR_SYSTEM_FETCH_HEADERS');
            else headerBuffer = buffer;
            return;
          }
          chunk = chunk.subarray(boundary + 4 - headerBuffer.length);
          headerBytes += boundary + 4;
          headerBuffer = Buffer.alloc(0);
          let parsed;
          try { parsed = parseHeaders(buffer.subarray(0, boundary)); }
          catch { fail('ERR_SYSTEM_FETCH_HEADERS'); return; }
          if (parsed.status < 200) continue;
          if (parsed.status >= 300 && parsed.status < 400 && parsed.status !== 304 && request.redirect === 'error') {
            fail('ERR_SYSTEM_FETCH_REDIRECT'); return;
          }
          const noBody = request.method === 'HEAD' || [204, 205, 304].includes(parsed.status);
          const stream = noBody ? null : new ReadableStream({
            start(value) { controller = value; },
            pull() { if (!stopped) child.stdout.resume(); },
            cancel() {
              if (stopped) return;
              stopped = true;
              cleanup();
              terminate();
            },
          }, { highWaterMark: 64 * 1024, size: value => value.byteLength });
          try { response = new Response(stream, parsed); }
          catch { fail('ERR_SYSTEM_FETCH_HEADERS'); return; }
          // Bodyless responses wait for curl's exit so transport failures surface.
          if (!noBody) { settled = true; resolve(response); }
          enqueue(chunk);
          return;
        }
      };

      request.signal?.addEventListener('abort', onAbort, { once: true });
      if (request.signal?.aborted) { onAbort(); return; }
      try {
        child = spawnImpl('/usr/bin/curl', [...ARGS], {
          shell: false, env: {}, stdio: ['pipe', 'pipe', 'ignore'],
        });
      } catch { fail('ERR_SYSTEM_FETCH_NETWORK'); return; }
      child.on('error', () => fail('ERR_SYSTEM_FETCH_NETWORK'));
      child.stdin.on('error', () => fail('ERR_SYSTEM_FETCH_NETWORK'));
      child.stdout.on('error', () => fail('ERR_SYSTEM_FETCH_NETWORK'));
      child.stdout.on('data', onData);
      child.stdout.once('end', () => { ended = true; finish(); });
      child.once('close', code => {
        closed = true;
        clearTimeout(killTimer);
        if (stopped) return;
        if (code !== 0) fail(exitError(code));
        else finish();
      });
      if (stopped) { terminate(); return; }
      deadline = setTimeout(() => fail('ERR_SYSTEM_FETCH_TIMEOUT'), TIMEOUT_MS);
      deadline.unref();
      try { child.stdin.end(request.config); }
      catch { fail('ERR_SYSTEM_FETCH_NETWORK'); }
    });
  };
}

export const systemFetch = createSystemFetch();
