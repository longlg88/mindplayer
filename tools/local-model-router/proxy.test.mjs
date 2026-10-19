import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { once } from 'node:events';
import { createLocalProxy } from './proxy.mjs';
import { chooseModels } from './router.mjs';
import { connectTestClient } from './test-client.mjs';

const catalog = ['model-a', 'model-b'].map((model, index) => ({ model, isDefault: !index, hidden: false, inputModalities: ['text'], defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'medium' }] }));

async function fixture(options = {}) {
  const sent = []; let starts = 0, stops = 0;
  const events = new EventEmitter();
  const proxy = await createLocalProxy({ catalog, pair: chooseModels(catalog), createTransport() {
    starts++;
    return { events, send(message) {
      sent.push(message);
      if (message.method === 'initialize') queueMicrotask(() => events.emit('message', { id: message.id, result: { userAgent: 'fixture' } }));
      if (message.method === 'turn/start') queueMicrotask(() => events.emit('message', { id: message.id, result: { turn: { id: 'real-turn', status: 'inProgress' } } }));
    }, close() { stops++; } };
  }, ...options });
  return { proxy, sent, events, starts: () => starts, stops: () => stops };
}

test('localhost health works, browser-origin health denied, no Codex child created', async () => {
  const f = await fixture();
  try {
    const url = f.proxy.url.replace('ws:', 'http:') + '/healthz';
    assert.equal((await fetch(url)).status, 200);
    assert.equal((await fetch(url, { headers: { Origin: 'https://untrusted.example' } })).status, 403);
    assert.equal(f.starts(), 0);
  } finally { await f.proxy.close(); }
});

test('missing/wrong token, browser origin, hostile Host are rejected before spawning Codex', async () => {
  const f = await fixture();
  try {
    await assert.rejects(connectTestClient(f.proxy.url, ''));
    await assert.rejects(connectTestClient(f.proxy.url, 'wrong'));
    await assert.rejects(connectTestClient(f.proxy.url, f.proxy.token, { Origin: 'https://example.org' }));
    await assert.rejects(connectTestClient(f.proxy.url, f.proxy.token, { Host: 'attacker.example' }));
    await assert.rejects(connectTestClient(f.proxy.url, f.proxy.token, { 'Sec-WebSocket-Version': '12' }));
    assert.equal(f.starts(), 0);
  } finally { await f.proxy.close(); }
});

test('authenticated socket routes a real RPC exchange and preserves approvals/stream', async () => {
  const f = await fixture();
  const client = await connectTestClient(f.proxy.url, f.proxy.token);
  try {
    assert.equal((await client.request('initialize', { clientInfo: { name: 'test', version: '0.1.0' } })).userAgent, 'fixture');
    const input = [{ type: 'text', text: '한글\n원인 분석 🐈' }];
    assert.equal((await client.request('turn/start', { threadId: 'thread', input, model: 'model-a' })).turn.id, 'real-turn');
    assert.equal(f.sent.at(-1).params.model, 'model-b'); assert.deepEqual(f.sent.at(-1).params.input, input);
    const approval = { id: 2, method: 'item/commandExecution/requestApproval', params: { itemId: 'item', threadId: 'thread', turnId: 'real-turn' } };
    let incoming = once(client.events, 'message'); f.events.emit('message', approval);
    assert.deepEqual((await incoming)[0], approval);
    client.send({ id: 2, result: { decision: 'decline' } });
    await new Promise(resolve => setTimeout(resolve, 20));
    assert.deepEqual(f.sent.at(-1), { id: 2, result: { decision: 'decline' } });
    const delta = { method: 'item/agentMessage/delta', params: { delta: '응답 🐈' } };
    incoming = once(client.events, 'message'); f.events.emit('message', delta);
    assert.deepEqual((await incoming)[0], delta);
    await assert.rejects(connectTestClient(f.proxy.url, f.proxy.token));
  } finally { client.close(); await f.proxy.close(); }
  assert.ok(f.stops() >= 1);
});

test('malformed RPC closes one connection without executing or bringing down the listener', async () => {
  const f = await fixture(); const client = await connectTestClient(f.proxy.url, f.proxy.token);
  try {
    const closed = once(client.events, 'closed'); client.send([]); await closed;
    assert.equal(f.sent.length, 0);
    assert.equal((await fetch(f.proxy.url.replace('ws:', 'http:') + '/healthz')).status, 200);
  } finally { client.close(); await f.proxy.close(); }
});

test('native startup metadata larger than 8 MiB is forwarded without disconnect', async () => {
  const f = await fixture(); const client = await connectTestClient(f.proxy.url, f.proxy.token);
  try {
    const message = { method: 'fixture/largeMetadata', params: { text: 'x'.repeat(9 * 1024 * 1024) } };
    const incoming = once(client.events, 'message'); f.events.emit('message', message);
    assert.deepEqual((await incoming)[0], message);
    assert.equal((await client.request('initialize', {})).userAgent, 'fixture');
  } finally { client.close(); await f.proxy.close(); }
});

test('audit write failure does not disconnect, resend, or crash error handling', async () => {
  const f = await fixture({ audit() { throw new Error('disk full'); } });
  const client = await connectTestClient(f.proxy.url, f.proxy.token);
  try {
    assert.equal((await client.request('initialize', {})).userAgent, 'fixture');
    await client.request('turn/start', { threadId: 'thread', input: [{ type: 'text', text: 'hi' }] });
    assert.equal(f.sent.filter(m => m.method === 'turn/start').length, 1);
    const closed = once(client.events, 'closed'); client.send([]); await closed;
    assert.equal((await fetch(f.proxy.url.replace('ws:', 'http:') + '/healthz')).status, 200);
  } finally { client.close(); await f.proxy.close(); }
});

test('backend transport close closes the UI without creating a replay', async () => {
  const f = await fixture(); const client = await connectTestClient(f.proxy.url, f.proxy.token);
  try {
    await client.request('turn/start', { threadId: 'thread', input: [{ type: 'text', text: 'hi' }] });
    const closed = once(client.events, 'closed'); f.events.emit('closed', 'fixture-exit'); await closed;
    assert.equal(f.sent.filter(m => m.method === 'turn/start').length, 1);
  } finally { client.close(); await f.proxy.close(); }
});
