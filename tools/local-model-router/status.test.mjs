import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, statSync, chmodSync, symlinkSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createStatusSink } from './status.mjs';

function fixture(run) {
  const dir = mkdtempSync(join(tmpdir(), 'mindplayer-status-test-'));
  const path = join(dir, 'status.json');
  writeFileSync(path, JSON.stringify({ schemaVersion: 2, generation: 'pane-1', revision: 0, stage: 'ready' }), { mode: 0o600 });
  try { run(path, () => JSON.parse(readFileSync(path, 'utf8'))); }
  finally { rmSync(dir, { recursive: true, force: true }); }
}

test('native clear ancestry survives missed polls and every turn status update', () => fixture((path, read) => {
  const sink = createStatusSink(path);
  sink({ event: 'thread-ownership', owned: true, threadId: 'main', clear: false });
  assert.deepEqual(read().clearAncestors, []);
  sink({ event: 'thread-ownership', owned: true, threadId: 'clear-1', clear: true });
  sink({ event: 'thread-ownership', owned: true, threadId: 'clear-2', clear: true });
  const ready = read();
  assert.deepEqual(ready.clearAncestors, ['main', 'clear-1']);
  assert.equal(Object.keys(ready).length, 13);
  for (const event of [
    { event: 'route-deciding', requestId: 1 },
    { event: 'route-requested', requestId: 1, model: 'selected', effort: 'high' },
    { event: 'start-acknowledged', requestId: 1, turnId: 'turn-1' },
    { event: 'turn-started', turnId: 'turn-1' },
    { event: 'model/rerouted', turnId: 'turn-1', toModel: 'reported' },
    { event: 'turn-completed', turnId: 'turn-1', status: 'completed' },
    { event: 'route-deciding', requestId: 2 },
    { event: 'route-failed', requestId: 2, reason: 'selector-error' },
  ]) {
    sink({ ...event, threadId: 'clear-2' });
    assert.deepEqual(read().clearAncestors, ['main', 'clear-1']);
  }
  const current = read();
  sink({ event: 'turn-started', threadId: 'main', turnId: 'late-turn' });
  sink({ event: 'thread-ownership', owned: false, threadId: 'helper', clear: true });
  assert.deepEqual(read(), current);
}));

test('selection, resume, fork and other startups reset clear ancestry, including same-thread resume', () => {
  for (const lifecycle of [
    { threadId: 'resumed', resumed: true, clear: false },
    { threadId: 'forked', forked: true, clear: false },
    { threadId: 'started', startup: true, clear: false },
    { threadId: 'marked', startup: true, mindplayerUserSelection: true },
    { threadId: 'same', resumed: true, clear: false },
    { threadId: 'prompt-lookalike', startup: true, input: '/clear', title: '/clear' },
  ]) fixture((path, read) => {
    const sink = createStatusSink(path);
    sink({ event: 'thread-ownership', owned: true, threadId: 'original' });
    sink({ event: 'thread-ownership', owned: true, threadId: 'same', clear: true });
    assert.deepEqual(read().clearAncestors, ['original']);
    sink({ event: 'thread-ownership', owned: true, ...lifecycle });
    assert.deepEqual(read().clearAncestors, [], JSON.stringify(lifecycle));
    sink({ event: 'thread-ownership', owned: true, threadId: 'next-clear', clear: true });
    assert.deepEqual(read().clearAncestors, [lifecycle.threadId]);
  });
});

test('clear evidence contains only the latest sixteen distinct validated prior owned identifiers within 4096 bytes', () => fixture((path, read) => {
  writeFileSync(path, JSON.stringify({ schemaVersion: 2, generation: 'g'.repeat(128), revision: 0, stage: 'ready' }));
  const sink = createStatusSink(path);
  const ids = Array.from({ length: 20 }, (_, index) => `thread-${String(index).padStart(3, '0')}-`.padEnd(128, 'x'));
  sink({ event: 'thread-ownership', owned: true, threadId: ids[0], clear: true });
  assert.deepEqual(read().clearAncestors, [], 'a first clear has no previous owned thread');
  for (let index = 1; index < ids.length; index++) {
    sink({ event: 'thread-ownership', owned: true, threadId: ids[index], clear: true });
    assert.deepEqual(read().clearAncestors, ids.slice(Math.max(0, index - 16), index));
  }
  const current = read();
  for (const threadId of ['', 'unsafe id', 'unsafe\n', '한글', 'x'.repeat(129), null, 4]) {
    sink({ event: 'thread-ownership', owned: true, threadId, clear: true });
    assert.deepEqual(read(), current);
  }
  sink({ event: 'route-deciding', threadId: ids.at(-1), requestId: 'r'.repeat(128) });
  sink({ event: 'route-requested', threadId: ids.at(-1), requestId: 'r'.repeat(128), model: 'm'.repeat(128), effort: 'e'.repeat(128), reason: 's'.repeat(128) });
  sink({ event: 'start-acknowledged', threadId: ids.at(-1), requestId: 'r'.repeat(128), turnId: 't'.repeat(128) });
  sink({ event: 'model/rerouted', threadId: ids.at(-1), turnId: 't'.repeat(128), toModel: 'o'.repeat(128) });
  assert.equal(read().clearAncestors.length, 16);
  assert.ok(statSync(path).size <= 4096, 'maximal identifiers and metadata fit the status byte cap');
  sink({ event: 'thread-ownership', owned: true, threadId: ids.at(-2), clear: true });
  assert.deepEqual(read().clearAncestors, [...ids.slice(-17, -2), ids.at(-1)]);
  const revisited = read();
  sink({ event: 'thread-ownership', owned: true, threadId: ids.at(-2), clear: true });
  assert.deepEqual(read(), revisited, 'same-thread ACK cannot append the current ID');
}));

test('thread and connection closure retain historical clear ancestry but sever the chain for fresh ownership', () => {
  for (const event of [{ event: 'thread-closed', threadId: 'cleared' }, { event: 'connection-closed-no-replay' }]) {
    for (const clear of [true, false]) for (const threadId of ['fresh', 'cleared']) fixture((path, read) => {
      const sink = createStatusSink(path);
      sink({ event: 'thread-ownership', owned: true, threadId: 'original' });
      sink({ event: 'thread-ownership', owned: true, threadId: 'cleared', clear: true });
      sink({ event: 'route-deciding', threadId: 'cleared', requestId: 1 });
      sink({ event: 'route-requested', threadId: 'cleared', requestId: 1, model: 'selected' });
      sink({ event: 'start-acknowledged', threadId: 'cleared', requestId: 1, turnId: 'old-turn' });
      sink({ event: 'turn-started', threadId: 'cleared', turnId: 'old-turn' });
      assert.deepEqual(read().clearAncestors, ['original']);
      sink(event);
      assert.equal(read().stage, 'closed');
      assert.equal(read().threadId, 'cleared');
      assert.deepEqual(read().clearAncestors, ['original'], 'verified history survives exit before the next UI poll');
      const closed = read();
      const lateEvents = [
        { event: 'turn-started', requestId: 1, turnId: 'old-turn' },
        { event: 'turn-completed', requestId: 1, turnId: 'old-turn', status: 'completed' },
        { event: 'start-acknowledged', requestId: 1, turnId: 'old-turn' },
        { event: 'model/rerouted', turnId: 'old-turn', toModel: 'late-model' },
      ];
      for (const late of lateEvents) sink({ ...late, threadId: 'cleared' });
      assert.deepEqual(read(), closed, 'old turns cannot rewrite the retained closed snapshot');
      sink({ event: 'thread-ownership', owned: true, threadId, clear });
      assert.equal(read().stage, 'ready');
      assert.equal(read().threadId, threadId);
      assert.deepEqual(read().clearAncestors, []);
      const fresh = read();
      for (const late of lateEvents) sink({ ...late, threadId: 'cleared' });
      assert.deepEqual(read(), fresh, 'old turns cannot claim a fresh binding, even for the same thread ID');
    });
  }
});

test('cancelled judgment is interrupted without a native turn and an identical retry starts fresh', () => fixture((path, read) => {
  const sink = createStatusSink(path);
  sink({ event: 'thread-ownership', owned: true, threadId: 'main' });
  sink({ event: 'route-deciding', threadId: 'main', requestId: 1 });
  sink({ event: 'route-cancelled', threadId: 'main', requestId: 1, reason: 'user-cancelled' });
  assert.equal(read().stage, 'interrupted');
  assert.equal(read().turnId, null);
  assert.equal(read().serverModelReported, false);
  sink({ event: 'route-deciding', threadId: 'main', requestId: 2 });
  const current = read();
  sink({ event: 'route-cancelled', threadId: 'main', requestId: 1, reason: 'user-cancelled' });
  assert.deepEqual(read(), current);
  sink({ event: 'route-requested', threadId: 'main', requestId: 2, model: 'selected', effort: 'high' });
  assert.equal(read().stage, 'requested');
}));

test('status lifecycle separates selection, acknowledgement, server report, completion, and next judgment', () => fixture((path, read) => {
  const sink = createStatusSink(path);
  sink({ event: 'thread-ownership', owned: true, threadId: 'main' });
  sink({ event: 'route-deciding', threadId: 'main', requestId: 1 });
  assert.equal(read().stage, 'judging');
  sink({ event: 'model-selected', threadId: 'main', requestId: 1, model: 'gpt-test', effort: 'low', reason: 'catalog-guided-jev' });
  assert.equal(read().stage, 'requested');
  sink({ event: 'route-requested', threadId: 'main', requestId: 1, model: 'gpt-test', effort: 'low', reason: 'catalog-guided-jev' });
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  assert.equal(read().stage, 'accepted', 'ACK alone is not actual execution');
  sink({ event: 'turn-started', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  assert.equal(read().stage, 'running'); assert.equal(read().serverModelReported, false);
  sink({ event: 'model/verification', threadId: 'main', turnId: 'turn-1', model: 'not-evidence' });
  assert.equal(read().serverModelReported, false);
  sink({ event: 'model/safetyBuffering/updated', threadId: 'main', turnId: 'turn-1', model: 'configured-only' });
  assert.equal(read().serverModelReported, false);
  sink({ event: 'model/rerouted', threadId: 'main', turnId: 'other', toModel: 'wrong' });
  assert.equal(read().observedModel, null);
  sink({ event: 'model/rerouted', threadId: 'main', turnId: 'turn-1', toModel: 'gpt-real' });
  assert.equal(read().observedModel, 'gpt-real'); assert.equal(read().serverModelReported, true);
  sink({ event: 'turn-completed', threadId: 'main', turnId: 'turn-1', status: 'completed' });
  assert.equal(read().stage, 'completed');
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  assert.equal(read().stage, 'completed', 'late acknowledgement cannot roll back completion');
  const revision = read().revision;
  sink({ event: 'route-deciding', threadId: 'main', requestId: 1 });
  assert.equal(read().stage, 'judging'); assert.equal(read().observedModel, null);
  assert.equal(read().serverModelReported, false); assert.ok(read().revision > revision);
  assert.equal(statSync(path).mode & 0o777, 0o600);
}));

test('helper threads, unowned events, stale IDs, and unsafe metadata cannot overwrite pane state or leak text', () => fixture((path, read) => {
  const sink = createStatusSink(path);
  sink({ event: 'jev-started', threadId: 'helper', requestId: 1 }); assert.equal(read().revision, 0);
  sink({ event: 'thread-ownership', owned: true, threadId: 'main' });
  sink({ event: 'route-deciding', threadId: 'main', requestId: 2 });
  const current = read();
  for (const event of [
    { event: 'thread-ownership', owned: false, threadId: 'helper' },
    { event: 'model-selected', threadId: 'helper', model: 'other' },
    { event: 'model-selected', threadId: 'main', requestId: 1, model: 'stale' },
    { event: 'route-requested', threadId: 'main', requestId: 1, model: 'stale' },
    { event: 'model-selected', threadId: 'main', model: '\u001b[31msecret-prompt' },
    { event: 'start-acknowledged', threadId: 'main', requestId: 1, turnId: 'old' },
    { event: 'route-failed', threadId: 'main', requestId: 1, reason: 'old' },
  ]) sink(event);
  assert.deepEqual(read(), current);
  sink({ event: 'route-failed', threadId: 'main', requestId: 2, reason: 'selector-error', input: 'private-prompt', apiKey: 'key-canary', error: 'upstream-body' });
  assert.equal(read().stage, 'failed');
  for (const value of ['private-prompt', 'key-canary', 'upstream-body']) assert.ok(!readFileSync(path, 'utf8').includes(value));
  sink({ event: 'connection-closed-no-replay' }); assert.equal(read().stage, 'closed');
}));

test('completion before acknowledgement is retained when correlated to the sent request', () => fixture((path, read) => {
  const sink = createStatusSink(path);
  sink({ event: 'thread-ownership', owned: true, threadId: 'main' });
  sink({ event: 'route-deciding', threadId: 'main', requestId: 1 });
  sink({ event: 'route-requested', threadId: 'main', requestId: 1, model: 'gpt-test' });
  sink({ event: 'turn-completed', threadId: 'main', requestId: 1, turnId: 'turn-1', status: 'completed' });
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  assert.equal(read().stage, 'completed'); assert.equal(read().turnId, 'turn-1');
}));

test('normal turns start and finish without any server model-change telemetry', () => fixture((path, read) => {
  const sink = createStatusSink(path);
  sink({ event: 'thread-ownership', owned: true, threadId: 'main' });
  sink({ event: 'route-deciding', threadId: 'main', requestId: 1 });
  sink({ event: 'route-requested', threadId: 'main', requestId: 1, model: 'selected', effort: 'high' });
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  assert.equal(read().stage, 'accepted');
  sink({ event: 'turn-started', threadId: 'main', turnId: 'turn-1' });
  assert.equal(read().stage, 'running');
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  assert.equal(read().stage, 'running', 'late ACK must not regress actual execution');
  sink({ event: 'turn-completed', threadId: 'main', turnId: 'turn-1', status: 'completed' });
  assert.equal(read().stage, 'completed');
  assert.equal(read().model, 'selected');
  assert.equal(read().observedModel, null);
  assert.equal(read().serverModelReported, false);
  assert.equal(Object.hasOwn(read(), 'executionVerified'), false, 'there is no serving-receipt claim');
}));

test('start before ACK is correlated, late ACK cannot regress it, and followups reject old events', () => fixture((path, read) => {
  const sink = createStatusSink(path);
  sink({ event: 'thread-ownership', owned: true, threadId: 'main' });
  sink({ event: 'route-deciding', threadId: 'main', requestId: 1 });
  sink({ event: 'route-requested', threadId: 'main', requestId: 1, model: 'first' });
  const requested = read();
  sink({ event: 'turn-started', threadId: 'main', turnId: 'unrelated' });
  assert.deepEqual(read(), requested, 'no turn binding before ACK or correlated start');
  sink({ event: 'turn-started', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  assert.equal(read().stage, 'running');
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  assert.equal(read().stage, 'running');
  sink({ event: 'turn-completed', threadId: 'main', turnId: 'turn-1', status: 'completed' });
  sink({ event: 'route-deciding', threadId: 'main', requestId: 2 });
  const judging = read();
  for (const event of [
    { event: 'turn-started', threadId: 'main', turnId: 'turn-1' },
    { event: 'turn-completed', threadId: 'main', requestId: 1, turnId: 'turn-1', status: 'completed' },
    { event: 'model/rerouted', threadId: 'main', turnId: 'turn-1', toModel: 'old' },
  ]) sink(event);
  assert.deepEqual(read(), judging);
  sink({ event: 'route-requested', threadId: 'main', requestId: 2, model: 'second' });
  const next = read();
  sink({ event: 'turn-started', threadId: 'main', requestId: 2, turnId: 'turn-1' });
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 2, turnId: 'turn-1' });
  assert.deepEqual(read(), next, 'completed turn IDs cannot be rebound to new requests');
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 2, turnId: 'turn-2' });
  sink({ event: 'turn-started', threadId: 'main', turnId: 'turn-2' });
  assert.equal(read().stage, 'running'); assert.equal(read().model, 'second');
}));

test('malformed completion and unsubmitted model selection cannot claim execution', () => fixture((path, read) => {
  const sink = createStatusSink(path);
  sink({ event: 'thread-ownership', owned: true, threadId: 'main' });
  sink({ event: 'route-deciding', threadId: 'main', requestId: 1 });
  sink({ event: 'model-selected', threadId: 'main', requestId: 1, model: 'selected' });
  const selected = read();
  sink({ event: 'turn-started', threadId: 'main', requestId: 1, turnId: 'unsubmitted' });
  assert.deepEqual(read(), selected);
  sink({ event: 'route-requested', threadId: 'main', requestId: 1, model: 'selected' });
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  const accepted = read();
  sink({ event: 'turn-completed', threadId: 'main', turnId: 'turn-1', status: 'not-a-status' });
  assert.deepEqual(read(), accepted);
}));

test('closing the owned thread ends running status and ignores late lifecycle events', () => fixture((path, read) => {
  const sink = createStatusSink(path);
  sink({ event: 'thread-ownership', owned: true, threadId: 'main' });
  sink({ event: 'route-deciding', threadId: 'main', requestId: 1 });
  sink({ event: 'route-requested', threadId: 'main', requestId: 1, model: 'selected' });
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  sink({ event: 'turn-started', threadId: 'main', turnId: 'turn-1' });
  const running = read();
  sink({ event: 'thread-closed', threadId: 'helper' }); assert.deepEqual(read(), running);
  sink({ event: 'thread-closed', threadId: 'main' });
  assert.equal(read().stage, 'closed'); assert.equal(read().reason, 'thread-closed');
  const closed = read();
  sink({ event: 'turn-started', threadId: 'main', turnId: 'turn-1' });
  sink({ event: 'start-acknowledged', threadId: 'main', requestId: 1, turnId: 'turn-1' });
  assert.deepEqual(read(), closed);
}));

test('status bootstrap rejects symlinks, public files, excessive bytes, invalid generations and stale revisions', () => fixture(path => {
  const link = `${path}.link`; symlinkSync(path, link);
  assert.throws(() => createStatusSink(link), /Invalid pane status channel/);
  chmodSync(path, 0o644); assert.throws(() => createStatusSink(path)); chmodSync(path, 0o600);
  for (const value of ['x'.repeat(4097), JSON.stringify({ schemaVersion: 2, generation: '\n', revision: 0, stage: 'ready' }),
    JSON.stringify({ schemaVersion: 2, generation: 'pane-1', revision: 1, stage: 'ready' }),
    JSON.stringify({ schemaVersion: 1, generation: 'pane-1', revision: 0, stage: 'ready' })]) {
    writeFileSync(path, value); assert.throws(() => createStatusSink(path));
  }
}));

test('missing channel is a no-op for standalone harnesses', () => {
  assert.doesNotThrow(() => createStatusSink()({ event: 'anything' }));
});
