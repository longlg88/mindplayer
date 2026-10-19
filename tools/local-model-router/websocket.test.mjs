import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { connect } from 'node:net';
import test from 'node:test';
import { acceptWebSocket } from './websocket.mjs';

const maskPayload = (data, mask = Buffer.from([1, 2, 3, 4])) => {
  const encoded = Buffer.alloc(data.length);
  for (let i = 0; i < data.length; i++) encoded[i] = data[i] ^ mask[i & 3];
  return encoded;
};

async function start(t, options = {}) {
  const messages = [];
  let closeCount = 0;
  let received;
  let closed;
  const message = new Promise(resolve => { received = resolve; });
  const closing = new Promise(resolve => { closed = resolve; });
  const server = createServer();
  const sockets = new Set();
  const fixture = { server, messages, message, closing, get closeCount() { return closeCount; } };
  server.on('connection', socket => {
    sockets.add(socket);
    socket.once('close', () => sockets.delete(socket));
  });
  t.after(async () => {
    const stopped = server.listening ? new Promise(resolve => server.close(resolve)) : Promise.resolve();
    const disconnected = [...sockets].map(socket => new Promise(resolve => socket.once('close', resolve)));
    fixture.raw?.destroy();
    for (const socket of sockets) socket.destroy();
    await Promise.all(disconnected);
    await stopped;
    assert.equal(server.listening, false);
    assert.equal(sockets.size, 0);
  });
  server.on('upgrade', (request, socket, head) => {
    acceptWebSocket(request, socket, head, {
      ...options,
      onMessage: (value) => { messages.push(value); received(value); },
      onClose: () => { closeCount++; closed(); },
    });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  fixture.port = server.address().port;
  return fixture;
}

test('performs a real handshake, receives text, sends text, and closes once', { timeout: 2000 }, async t => {
  const fixture = await start(t);
  const client = new WebSocket(`ws://127.0.0.1:${fixture.port}`);
  const opened = once(client, 'open');
  await opened;
  client.send('hello');
  await fixture.message;
  assert.deepEqual(fixture.messages, ['hello']);
  const closing = once(client, 'close');
  client.close(1000, 'done');
  await closing;
  await fixture.closing;
  assert.equal(fixture.closeCount, 1);
});

test('handles fragmented UTF-8 with a code point split across frames', { timeout: 2000 }, async t => {
  const fixture = await start(t);
  const raw = fixture.raw = connect(fixture.port, '127.0.0.1');
  await once(raw, 'connect');
  const key = 'dGhlIHNhbXBsZSBub25jZQ==';
  raw.write(`GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`);
  await once(raw, 'data');
  const mask = Buffer.from([1, 2, 3, 4]);
  raw.write(Buffer.concat([Buffer.from([0x01, 0x82]), mask, maskPayload(Buffer.from([0xf0, 0x9f]), mask)]));
  raw.write(Buffer.concat([Buffer.from([0x80, 0x82]), mask, maskPayload(Buffer.from([0x98, 0x80]), mask)]));
  await fixture.message;
  assert.deepEqual(fixture.messages, ['😀']);
  raw.destroy();
  await fixture.closing;
});

function rawFrame({ first = 0x81, payload = Buffer.alloc(0), masked = true, mask = Buffer.from([1, 2, 3, 4]) } = {}) {
  const data = Buffer.from(payload);
  const header = data.length < 126 ? Buffer.from([first, (masked ? 0x80 : 0) | data.length]) : Buffer.from([first, (masked ? 0x80 : 0) | 126, data.length >> 8, data.length & 255]);
  if (!masked) return Buffer.concat([header, data]);
  const encoded = maskPayload(data, mask);
  return Buffer.concat([header, mask, encoded]);
}

async function rawConnection(t) {
  const fixture = await start(t);
  const raw = fixture.raw = connect(fixture.port, '127.0.0.1');
  await once(raw, 'connect');
  raw.write('GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n');
  await once(raw, 'data');
  return fixture;
}

for (const [name, frame, code] of [
  ['rejects an unmasked frame', rawFrame({ masked: false }), 1002],
  ['rejects invalid control framing', rawFrame({ first: 0x09, payload: Buffer.from('x') }), 1002],
  ['rejects an oversized control frame', rawFrame({ first: 0x89, payload: Buffer.alloc(126) }), 1002],
  ['rejects binary data', rawFrame({ first: 0x82, payload: Buffer.from('x') }), 1002],
  ['rejects invalid UTF-8', rawFrame({ payload: Buffer.from([0xc0, 0xaf]) }), 1007],
  ['rejects a reserved close code', rawFrame({ first: 0x88, payload: Buffer.from([3, 237]) }), 1002],
  ['rejects non-minimal payload lengths', Buffer.from([0x81, 0xfe, 0, 1, 1, 2, 3, 4, 0]), 1002],
]) {
  test(name, { timeout: 2000 }, async t => {
    const fixture = await rawConnection(t);
    const closing = once(fixture.raw, 'data');
    fixture.raw.write(frame);
    const [response] = await closing;
    assert.equal(response[0] & 0x0f, 8);
    assert.equal(response.readUInt16BE(2), code);
    assert.deepEqual(fixture.messages, []);
    fixture.raw.end();
    await fixture.closing;
    assert.equal(fixture.closeCount, 1);
  });
}

test('responds to ping with pong and rejects oversize messages', { timeout: 2000 }, async t => {
  const fixture = await start(t, { maxBytes: 4 });
  const raw = fixture.raw = connect(fixture.port, '127.0.0.1');
  await once(raw, 'connect');
  raw.write('GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n');
  await once(raw, 'data');
  const pong = new Promise((resolve) => raw.once('data', resolve));
  raw.write(rawFrame({ first: 0x89, payload: Buffer.from('p') }));
  assert.equal((await pong)[0] & 0x0f, 0xa);
  const closing = once(raw, 'data');
  raw.write(rawFrame({ payload: Buffer.from('12345') }));
  const [response] = await closing;
  assert.equal(response.readUInt16BE(2), 1009);
  raw.end();
  await fixture.closing;
  assert.equal(fixture.closeCount, 1);
});
