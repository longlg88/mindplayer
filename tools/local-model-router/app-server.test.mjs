import test from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { openAppServer, probeModels, APP_SERVER_INPUT_MAX_BYTES } from './app-server.mjs';

const fakeChild = script => ({ binary: process.execPath, args: ['-e', script] });

test('probe separates server requests from responses with the same ID', { timeout: 5000 }, async () => {
  const options = fakeChild(String.raw`
    const readline = require('node:readline');
    const out = value => process.stdout.write(JSON.stringify(value) + '\n');
    readline.createInterface({ input: process.stdin }).on('line', line => {
      const message = JSON.parse(line);
      if (message.method === 'initialize') return out({ id: message.id, method: 'fixture/serverRequest', params: {} });
      if (message.id === 1 && message.error?.code === -32601) return out({ id: 1, result: {} });
      if (message.method === 'model/list') out({ id: message.id, result: { data: [{ model: 'fake-model' }] } });
    });
  `);
  assert.deepEqual(await probeModels(options), [{ model: 'fake-model' }]);
});

test('a real stdin message larger than 16 MiB reaches the child intact', { timeout: 10000 }, async t => {
  const transport = openAppServer(fakeChild(String.raw`
    require('node:readline').createInterface({ input: process.stdin }).on('line', line => {
      const message = JSON.parse(line);
      process.stdout.write(JSON.stringify({ id: message.id, result: { length: message.data.length } }) + '\n');
    });
  `));
  t.after(() => transport.close());
  const response = once(transport.events, 'message');
  const length = 17 * 1024 * 1024;
  assert.equal(APP_SERVER_INPUT_MAX_BYTES, 64 * 1024 * 1024);
  transport.send({ id: 1, data: 'x'.repeat(length) });
  assert.equal((await response)[0].result.length, length);
});

test('input queue hard bound rejects before writing', async t => {
  const transport = openAppServer({ ...fakeChild('process.stdin.resume();'), maxBytes: 1024 });
  t.after(() => transport.close());
  const closed = once(transport.events, 'closed');
  assert.throws(() => transport.send({ data: 'x'.repeat(1024) }), /input queue exceeded/);
  assert.equal((await closed)[0], 'input-size-limit');
});

test('queued stdin bytes are bounded even when individual messages fit', async t => {
  const transport = openAppServer({ ...fakeChild('process.stdin.pause(); setInterval(() => {}, 1000);'), maxBytes: 1024 });
  t.after(() => transport.close());
  transport.send({ data: 'x'.repeat(600) });
  const closed = once(transport.events, 'closed');
  assert.throws(() => transport.send({ data: 'y'.repeat(600) }), /input queue exceeded/);
  assert.equal((await closed)[0], 'input-size-limit');
});

test('large JSONL output is assembled across chunks without rescanning the whole line', { timeout: 10000 }, async t => {
  const transport = openAppServer(fakeChild(String.raw`
    process.stdout.write(JSON.stringify({ id: 1, result: { text: '한'.repeat(6 * 1024 * 1024) } }) + '\n');
    process.stdin.resume();
  `));
  t.after(() => transport.close());
  const [message] = await once(transport.events, 'message');
  assert.equal(message.result.text.length, 6 * 1024 * 1024);
  assert.equal(message.result.text.at(-1), '한');
});

test('JSONL hard bound rejects oversized output without unbounded buffering', async t => {
  const transport = openAppServer({ ...fakeChild(String.raw`process.stdout.write('x'.repeat(2048) + '\n'); process.stdin.resume();`), maxBytes: 1024 });
  t.after(() => transport.close());
  assert.equal((await once(transport.events, 'closed'))[0], 'jsonl-size-limit');
});

test('close terminates an owned POSIX child and grandchild', { skip: process.platform === 'win32', timeout: 5000 }, async t => {
  const transport = openAppServer(fakeChild(String.raw`
    const { spawn } = require('node:child_process');
    const grandchild = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });
    process.stdout.write(JSON.stringify({ method: 'fixture/grandchild', params: { pid: grandchild.pid } }) + '\n');
    process.stdin.resume();
  `));
  t.after(() => transport.close());
  const [message] = await once(transport.events, 'message');
  transport.close();
  await new Promise(resolve => setTimeout(resolve, 1700));
  assert.throws(() => process.kill(message.params.pid, 0), { code: 'ESRCH' });
});

test('aborting a never-replying probe terminates its owned child', { skip: process.platform === 'win32', timeout: 5000 }, async t => {
  const directory = mkdtempSync(join(tmpdir(), 'mindplayer-probe-test-'));
  const pidFile = join(directory, 'pid');
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const controller = new AbortController();
  const probing = probeModels({ ...fakeChild(String.raw`
    require('node:fs').writeFileSync(process.env.PROBE_PID_FILE, String(process.pid));
    process.stdin.resume();
  `), env: { ...process.env, PROBE_PID_FILE: pidFile }, signal: controller.signal });
  for (let attempt = 0; attempt < 50 && !existsSync(pidFile); attempt++) await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(existsSync(pidFile), true);
  const pid = Number(readFileSync(pidFile, 'utf8'));
  controller.abort();
  await assert.rejects(probing, /probe closed\/timed out/);
  await new Promise(resolve => setTimeout(resolve, 1700));
  assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
});
