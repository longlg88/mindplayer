// Local integration-test client, not part of the user-facing runtime.
import { connect } from 'node:net';
import { randomBytes } from 'node:crypto';
import { EventEmitter } from 'node:events';

export function connectTestClient(url, token, extraHeaders = {}) {
  return new Promise((resolve, reject) => {
    const address = new URL(url);
    const socket = connect({ host: address.hostname, port: Number(address.port) });
    const events = new EventEmitter();
    let buffer = Buffer.alloc(0), upgraded = false, id = 0;
    const pending = new Map();
    const timer = setTimeout(() => { socket.destroy(); reject(new Error('Handshake timeout')); }, 3000);
    const send = message => {
      const payload = Buffer.from(JSON.stringify(message));
      const mask = randomBytes(4);
      const header = payload.length < 126 ? Buffer.from([0x81, 0x80 | payload.length]) : Buffer.from([0x81, 0xfe, payload.length >> 8, payload.length & 255]);
      const masked = Buffer.from(payload);
      for (let i = 0; i < masked.length; i++) masked[i] ^= mask[i % 4];
      socket.write(Buffer.concat([header, mask, masked]));
    };
    socket.on('connect', () => {
      const headers = { Host: address.host, Upgrade: 'websocket', Connection: 'Upgrade', 'Sec-WebSocket-Key': randomBytes(16).toString('base64'), 'Sec-WebSocket-Version': '13', Authorization: `Bearer ${token}`, ...extraHeaders };
      socket.write(`GET / HTTP/1.1\r\n${Object.entries(headers).map(([key, value]) => `${key}: ${value}\r\n`).join('')}\r\n`);
    });
    socket.on('error', error => { clearTimeout(timer); reject(error); });
    socket.on('close', () => {
      clearTimeout(timer);
      for (const waiter of pending.values()) { clearTimeout(waiter.timer); waiter.reject(new Error('Connection closed')); }
      pending.clear(); events.emit('closed');
    });
    socket.on('data', chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      if (!upgraded) {
        const end = buffer.indexOf('\r\n\r\n');
        if (end < 0) return;
        clearTimeout(timer);
        if (!buffer.subarray(0, end).toString().startsWith('HTTP/1.1 101 ')) {
          socket.destroy(); reject(new Error('Upgrade rejected')); return;
        }
        upgraded = true; buffer = buffer.subarray(end + 4);
        resolve({
          events, send, close: () => socket.destroy(),
          request(method, params = {}, timeoutMs = 10_000) {
            return new Promise((done, fail) => {
              const requestId = ++id;
              const waitTimer = setTimeout(() => { pending.delete(requestId); fail(new Error(`RPC timeout: ${method}`)); }, timeoutMs);
              pending.set(requestId, { resolve: done, reject: fail, timer: waitTimer });
              send({ id: requestId, method, params });
            });
          },
        });
      }
      while (buffer.length >= 2) {
        let size = buffer[1] & 127, offset = 2;
        if (size === 126) { if (buffer.length < 4) return; size = buffer.readUInt16BE(2); offset = 4; }
        if (size === 127) { if (buffer.length < 10) return; size = Number(buffer.readBigUInt64BE(2)); offset = 10; }
        if (buffer.length < offset + size) return;
        const opcode = buffer[0] & 15, payload = buffer.subarray(offset, offset + size);
        buffer = buffer.subarray(offset + size);
        if (opcode === 8) { socket.destroy(); return; }
        if (opcode !== 1) continue;
        const message = JSON.parse(payload.toString());
        if (!message.method && pending.has(message.id)) {
          const waiter = pending.get(message.id); pending.delete(message.id); clearTimeout(waiter.timer);
          if (message.error) waiter.reject(new Error(`RPC rejected: ${message.error.code}`)); else waiter.resolve(message.result);
        }
        events.emit('message', message);
      }
    });
  });
}
