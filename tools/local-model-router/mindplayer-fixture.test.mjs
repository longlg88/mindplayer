import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { createPromptComposer } from './mindplayer-fixture.mjs';

// Pure tests for the synthetic smoke driver, not the private native composer.
// Importing the driver must not create files, spawn processes, or open sockets.
function harness() {
  const sent = [];
  const composer = createPromptComposer({
    send: request => sent.push(structuredClone(request)), render: () => {}, idPrefix: 'test',
    params: () => ({ threadId: 'same-thread', model: 'gpt-6-luna', effort: 'low', cwd: '/fixture' }),
  });
  return { composer, sent };
}

// Exercise the actual UI message consumer without processes, sockets or disk
// writes. Lifecycle replies no longer belong to the removed preview receiver.
function remoteHarness() {
  const source = readFileSync(new URL('./mindplayer-fixture.mjs', import.meta.url), 'utf8');
  const start = source.indexOf('function remoteUi() {');
  const end = source.indexOf('\nif (import.meta.url ===');
  assert.ok(start >= 0 && end > start, 'actual remote UI consumer required');
  const handlers = new Map(), sent = [];
  let input, state;
  runInNewContext(`${source.slice(start, end)}\nremoteUi();`, {
    args: ['--remote', 'ws://fixture', '--remote-auth-token-env', 'TOKEN', '--cd', '/fixture'],
    markerDir: '/fixture', createPromptComposer,
    baseline: () => ({ model: 'gpt-6-luna', effort: 'low' }),
    WebSocket: class {
      addEventListener(event, handler) { handlers.set(event, handler); }
      send(body) { sent.push(JSON.parse(body)); }
    },
    writeFileSync(path, body) {
      if (path === '/fixture/composer-state.json') state = JSON.parse(body);
    },
    appendFileSync() {},
    process: {
      pid: 1, env: { MINDPLAYER_PER_PROMPT_CHOICE: '1', TOKEN: 'synthetic' },
      stdout: { write() {} }, stdin: { setEncoding() {}, on(event, handler) {
        assert.equal(event, 'data'); input = handler;
      } },
      exit() { assert.fail('lifecycle messages must not close the UI'); },
    },
  });
  const receive = message => handlers.get('message')({ data: JSON.stringify(message) });
  handlers.get('open')();
  receive({ id: 1, result: {} });
  receive({ id: 2, result: { thread: { id: 'same-thread' } } });
  assert.equal(state.mode, 'native');
  sent.length = 0;
  return { composer: { input: chunk => input(chunk), state: () => state }, sent, receive };
}

test('new sessions start OFF and Enter submits native metadata without F6', () => {
  const { composer, sent } = harness();
  assert.deepEqual(composer.state(), { draft: '', mode: 'native' });
  composer.input('analyze B\r');
  assert.equal(sent.length, 1);
  assert.equal(sent[0].params.input[0].text, 'analyze B');
  assert.deepEqual(sent[0].params.mindplayerRoutingChoice, { mode: 'native', includeRecent: false });
  assert.deepEqual(composer.state(), { draft: '', mode: 'native' });
});

test('F6 toggles session ON and OFF without submitting or editing the draft', () => {
  const { composer, sent } = harness();
  composer.input('A\x1b[17~');
  assert.equal(composer.state().mode, 'jev');
  assert.equal(composer.state().draft, 'A');
  composer.input('\x1b[17~');
  assert.deepEqual(composer.state(), { draft: 'A', mode: 'native' });
  assert.equal(sent.length, 0);
});

test('actual UI ACK and lifecycle notifications preserve sticky ON and follow-up submission', () => {
  const { composer, sent, receive } = remoteHarness();
  composer.input('A\x1b[17~\r');
  receive({ id: sent[0].id, result: { turn: { id: 'fixture-turn-A' } } });
  receive({ method: 'turn/started', params: { threadId: 'same-thread', turn: { id: 'fixture-turn-A' } } });
  receive({ method: 'turn/completed', params: { threadId: 'same-thread', turn: { id: 'fixture-turn-A', status: 'completed' } } });
  assert.equal(composer.state().mode, 'jev');
  composer.input('C\r');
  assert.equal(sent.length, 2);
  assert.equal(sent[1].params.input[0].text, 'C');
  assert.deepEqual(sent[1].params.mindplayerRoutingChoice, { mode: 'jev', includeRecent: false });
});

test('actual UI ignores retired preview replies and stale completion without changing the draft or OFF mode', () => {
  const { composer, sent, receive } = remoteHarness();
  composer.input('C\x1b[18~');
  const before = structuredClone(composer.state());
  receive({ id: 999, result: { snapshot: { currentInput: 'C' }, previewId: 'retired-id' } });
  receive({ method: 'turn/completed', params: { threadId: 'same-thread', turn: { id: 'previous-turn', status: 'completed' } } });
  assert.deepEqual(composer.state(), before);
  assert.equal(sent.length, 0);
  composer.input('\r');
  assert.equal(sent.length, 1);
  assert.equal(sent[0].params.input[0].text, 'C');
  assert.deepEqual(sent[0].params.mindplayerRoutingChoice, { mode: 'native', includeRecent: false });
});

test('typed submissions keep ON or OFF until F6 toggles the session', () => {
  const { composer, sent } = harness();
  for (const [text, keys, mode] of [['analyze B', 1, 'jev'], ['A', 0, 'jev'], ['C', 1, 'native'], ['D', 0, 'native']]) {
    composer.input(text);
    for (let count = 0; count < keys; count++) composer.input('\x1b[17~');
    composer.input('\r');
    assert.equal(composer.state().mode, mode, 'submission must preserve the session mode');
    assert.equal(composer.state().draft, '');
  }
  assert.deepEqual(sent.map(request => request.params.mindplayerRoutingChoice), [
    { mode: 'jev', includeRecent: false }, { mode: 'jev', includeRecent: false },
    { mode: 'native', includeRecent: false }, { mode: 'native', includeRecent: false },
  ]);
  assert.deepEqual(sent.map(request => request.params.input[0].text), ['analyze B', 'A', 'C', 'D']);
  assert.equal(new Set(sent.map(request => request.params.clientUserMessageId)).size, 4);
});

test('identical queued text keeps distinct submission identities and choices', () => {
  const { composer, sent } = harness();
  composer.input('same\x1b[17~\r');
  composer.input('same\x1b[17~\r');
  assert.notEqual(sent[0].params.clientUserMessageId, sent[1].params.clientUserMessageId);
  assert.deepEqual(sent.map(request => request.params.mindplayerRoutingChoice.mode), ['jev', 'native']);
});

test('removed F7 changes neither draft nor session mode and emits no RPC', () => {
  const { composer, sent } = harness();
  composer.input('A');
  for (const mode of ['native', 'jev']) {
    const before = structuredClone(composer.state());
    assert.equal(before.mode, mode);
    composer.input('\x1b[18~');
    assert.deepEqual(composer.state(), before);
    assert.equal(sent.length, 0);
    composer.input('\x1b[17~');
  }
});

test('submitted metadata has only mode and includeRecent, with no preview state or receiver', () => {
  const { composer, sent } = harness();
  composer.input('A\x1b[18~');
  composer.input('\x1b[17~\r');
  assert.equal(sent.length, 1);
  assert.equal(sent[0].method, 'turn/start');
  assert.deepEqual(sent[0].params.mindplayerRoutingChoice, { mode: 'jev', includeRecent: false });
  assert.deepEqual(Object.keys(composer.state()).sort(), ['draft', 'mode']);
  assert.deepEqual(Object.keys(composer).sort(), ['input', 'state']);
  assert.equal(Object.hasOwn(sent[0].params.mindplayerRoutingChoice, 'previewId'), false);
});

test('typing, deletion, paste and clearing preserve ON without reusing submission identity', () => {
  const { composer, sent } = harness();
  composer.input('A\x1b[17~ changed!\x7f\x1b[200~ paste\x1b[201~');
  assert.deepEqual(composer.state(), { draft: 'A changed paste', mode: 'jev' });
  assert.equal(sent.length, 0);
  composer.input('\rdiscard\x15C\r');
  assert.equal(sent[0].params.input[0].text, 'A changed paste');
  assert.equal(sent[1].params.input[0].text, 'C');
  assert.notEqual(sent[0].params.clientUserMessageId, sent[1].params.clientUserMessageId);
  assert.ok(sent.every(request => request.params.mindplayerRoutingChoice.mode === 'jev'));
});

test('a new composer starts OFF without inheriting another session ON mode', () => {
  const { composer, sent } = harness();
  composer.input('A\x1b[17~\r');
  const other = harness();
  other.composer.input('C\r');
  assert.equal(sent[0].params.mindplayerRoutingChoice.mode, 'jev');
  assert.equal(other.sent[0].params.mindplayerRoutingChoice.mode, 'native');
  assert.equal(composer.state().mode, 'jev');
});

test('bracketed paste preserves multiline text until Enter without changing OFF', () => {
  const { composer, sent } = harness();
  composer.input('\x1b[200~first\nsecond\x1b[201~');
  assert.equal(sent.length, 0);
  assert.equal(composer.state().mode, 'native');
  composer.input('\r');
  assert.equal(sent[0].params.input[0].text, 'first\nsecond');
  assert.deepEqual(sent[0].params.mindplayerRoutingChoice, { mode: 'native', includeRecent: false });
});
