import { createServer } from 'node:http';
import { randomBytes, timingSafeEqual } from 'node:crypto';
import { acceptWebSocket } from './websocket.mjs';
import { openAppServer, APP_SERVER_INPUT_MAX_BYTES } from './app-server.mjs';
import { ModelRouter } from './router.mjs';

export async function createLocalProxy({ catalog, pair, cwd, env = process.env, binary = 'codex', port = 0, audit, decide, decisionTimeoutMs, backendArgs = [], createTransport = openAppServer, serviceFactory, requireFreshTurn = false }) {
  const token = randomBytes(32).toString('hex');
  let auditFailed = false;
  const record = event => {
    try { audit?.(event); }
    catch {
      if (!auditFailed) console.error('로컬 라우터 메타데이터 기록 실패. 연결은 유지되며 요청을 재전송하지 않습니다.');
      auditFailed = true;
    }
  };
  let live;
  let closing;
  const server = createServer((request, response) => {
    const ok = !request.headers.origin && request.url === '/healthz';
    response.writeHead(ok ? 200 : 403, { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' });
    response.end(ok ? 'local-router-ok\n' : 'Forbidden\n');
  });
  server.on('upgrade', (request, socket, head) => {
    const expected = Buffer.from(`Bearer ${token}`);
    const received = Buffer.from(request.headers.authorization ?? '');
    const local = socket.remoteAddress === '127.0.0.1' || socket.remoteAddress === '::ffff:127.0.0.1';
    const host = request.headers.host === `127.0.0.1:${server.address().port}`;
    const authorized = received.length === expected.length && timingSafeEqual(received, expected);
    if (!local || !host || request.headers.origin || !authorized || request.url !== '/' || live || closing) {
      record({ event: 'upgrade-denied', local, host, origin: !!request.headers.origin, authorized, pathOk: request.url === '/', busy: !!live });
      socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n');
      return;
    }
    record({ event: 'upgrade-accepted' });
    // No requests may arrive until the handshake and router are ready.
    socket.pause();
    let router;
    let transport;
    let ws;
    try { ws = acceptWebSocket(request, socket, Buffer.alloc(0), {
      // The installed CLI emits plugin catalog notifications larger than 8 MiB.
      // Keep a hard bound, but don't mistake normal startup metadata for a prompt.
      maxBytes: APP_SERVER_INPUT_MAX_BYTES,
      onMessage(text) {
        try {
          const message = JSON.parse(text);
          record({ event: 'rpc-client', method: message.method ?? null, requestId: message.id ?? null });
          router.client(message);
        }
        catch (error) {
          record({ event: 'client-protocol-error', reason: error instanceof SyntaxError ? 'invalid-json' : 'invalid-rpc-or-forwarding' });
          ws.close(1008, 'Invalid RPC; reconnect manually');
        }
      },
      onClose() { router?.close(); transport?.close(); if (live?.ws === ws) live = undefined; },
    }); } catch { socket.destroy(); return; }
    if (!ws) return;
    try { transport = createTransport({ binary, cwd, env, args: backendArgs }); }
    catch { record({ event: 'transport-start-failed' }); ws.close(1011, 'Codex could not start'); socket.resume(); return; }
    router = new ModelRouter({ catalog, pair, decide, decisionTimeoutMs, requireFreshTurn, service: serviceFactory?.(record), audit: record, toServer: message => transport.send(message), toClient: message => {
      const text = JSON.stringify(message);
      const bytes = Buffer.byteLength(text);
      if (bytes > 1024 * 1024) record({ event: 'large-server-message', method: message.method ?? null, requestId: message.id ?? null, bytes });
      ws.send(text);
    } });
    transport.events.on('message', message => {
      try {
        if (message.id !== undefined) record({ event: 'rpc-server', method: message.method ?? null, requestId: message.id, errorCode: message.error?.code ?? null });
        router.server(message);
      } catch (error) {
        record({ event: 'server-protocol-error', reason: error.message === 'message exceeds maxBytes' ? 'message-size-limit' : 'forwarding-failed' });
        ws.close(1011, 'Codex transport failed; no replay');
      }
    });
    transport.events.on('closed', reason => {
      record({ event: 'transport-closed', reason });
      ws.close(1011, 'Codex closed; no replay');
    });
    live = { ws, router, transport };
    if (head.length) socket.unshift(head);
    socket.resume();
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', resolve);
  });
  return {
    url: `ws://127.0.0.1:${server.address().port}`,
    token,
    close() {
      closing ??= (async () => {
        live?.router.close(); live?.transport.close(); live?.ws.close();
        server.closeAllConnections();
        await new Promise(resolve => server.close(resolve));
      })();
      return closing;
    },
  };
}
