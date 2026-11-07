import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { createNativeQueueFixture } from './native-queue-fixture.mjs';
import { openOuterNative, outerMindplayerLaunch } from './mindplayer-input-fixture.mjs';

// Explicit frozen binaries only. All writable state is under each fixture's
// mkdtemp root; no auth files, real sessions or provider calls are involved.
// MINDPLAYER_TEST_INPUT=1 MINDPLAYER_TEST_BINARY=/absolute/path/to/mindplayer \
// MINDPLAYER_NATIVE_CODEX=/absolute/path/to/installed/codex \
//   node --test tools/local-model-router/mindplayer-input.test.mjs
// Injected UTF-8 tests byte preservation, not physical keyboard/IME composition.
const enabled = process.env.MINDPLAYER_TEST_INPUT === '1';
const inputText = start => start.request.params.input.filter(item => item.type === 'text').map(item => item.text).join('\n');

async function withFixture(t, run, outer = true) {
  if (outer) assert.ok(process.env.MINDPLAYER_TEST_BINARY, 'explicit outer binary required');
  const f = await createNativeQueueFixture({ queue: false, perPrompt: true,
    ...(outer ? { launch: outerMindplayerLaunch(process.env.MINDPLAYER_TEST_BINARY) } : {}) });
  try {
    if (outer) await openOuterNative(f);
    else await f.wait(() => f.projectedScreen().includes('Ask Codex to do anything'), 'native-only composer');
    const boundaries = await run(f);
    assert.deepEqual(f.unknown, [], 'all actual native RPCs were understood');
    assert.equal(f.calls.length, 0, 'Jev OFF does not invoke even the synthetic judge');
    t.diagnostic(JSON.stringify({ scope: outer ? f.launched.evidence.scope : 'native-only PTY -> synthetic loopback backend',
      ...(outer ? { outer: f.launched.evidence } : {}), nativeVersion: f.version, nativeSha256: f.binarySha256,
      boundaries, actualStarts: f.starts.map(inputText), clientStarts: f.clientRequests('turn/start').length }));
    t.diagnostic(`input evidence: ${f.capture('input-success')}`);
  } catch (error) {
    t.diagnostic(`input evidence: ${f.capture('input-failure')}`);
    throw error;
  } finally {
    const cleanup = await f.close();
    assert.deepEqual(cleanup.alivePids, []);
    assert.equal(cleanup.portClosed, true);
    assert.equal(cleanup.rootRemoved, true);
    t.diagnostic('owned processes, loopback port and temporary root cleaned');
  }
}

const withOuter = (t, run) => withFixture(t, run);

for (const prompt of ['ordinary input', '한글 입력']) test(`outer separated ${prompt} submits once on the first Enter`, { skip: !enabled }, async t => {
  await withOuter(t, async f => {
    let draft = '';
    for (const character of prompt) {
      await f.write(character);
      draft += character;
      await f.wait(() => f.projectedScreen().includes(`› ${draft}`), 'native typed character rendering');
    }
    await f.write('\r');
    assert.equal(inputText(await f.waitStart(0)), prompt);
    await f.expectNoSubmission(300);
    assert.equal(f.clientRequests('turn/start').length, 1);
    return { firstEnter: f.clientRequests('turn/start').length };
  });
});

test('outer rapid separate keystrokes submit once on the first Enter', { skip: !enabled }, async t => {
  await withOuter(t, async f => {
    for (const character of 'rapid typed input') { await f.write(character); await delay(20); }
    await f.write('\r');
    assert.equal(inputText(await f.waitStart(0)), 'rapid typed input');
    await f.expectNoSubmission(300);
    assert.equal(f.clientRequests('turn/start').length, 1);
    return { separateWriteIntervalMs: 20, firstEnter: f.clientRequests('turn/start').length };
  });
});

for (const prompt of ['rapid prompt hello', '한글 빠른 입력']) test(`outer raw burst ${prompt} preserves unbracketed paste protection`, { skip: !enabled }, async t => {
  await withOuter(t, async f => {
    await f.write(`${prompt}\r`);
    await f.expectNoSubmission(400, 0);
    const firstEnter = f.clientRequests('turn/start').length;
    assert.equal(firstEnter, 0, 'ambiguous raw CR is not forced into submission');
    // A second user action is explicit in this safety test, never automated by
    // the host. The previous failing force-submit expectation is not a fix.
    await f.write('\r');
    assert.equal(inputText(await f.waitStart(0)), prompt);
    await f.expectNoSubmission(300);
    assert.equal(f.clientRequests('turn/start').length, 1);
    return { rapidUnbracketedFirstEnter: firstEnter, afterSeparateUserEnter: f.clientRequests('turn/start').length };
  });
});

for (const outer of [false, true]) test(`${outer ? 'outer' : 'native-only reference'} explicit Ctrl+Enter keeps the native modifier instead of submitting`, { skip: !enabled }, async t => {
  await withFixture(t, async f => {
    await f.write('\x1b[200~first\x1b[201~\x1b[13;5u');
    await f.expectNoSubmission(400, 0);
    const enhancedEnter = f.clientRequests('turn/start').length;
    await f.write('\x1b[200~second\x1b[201~\r');
    assert.equal(inputText(await f.waitStart(0)), 'firstsecond', 'native Ctrl+Enter remains non-submitting');
    await f.expectNoSubmission(300);
    assert.equal(f.clientRequests('turn/start').length, 1, 'one later plain Enter is the only submission');
    return { explicitCtrlEnter: enhancedEnter, afterPlainEnter: f.clientRequests('turn/start').length };
  }, outer);
});

test('outer single-line bracketed paste submits once on the first explicit Enter', { skip: !enabled }, async t => {
  await withOuter(t, async f => {
    await f.write('\x1b[200~pasted input\x1b[201~');
    await f.expectNoSubmission(400, 0);
    const pasteOnly = f.clientRequests('turn/start').length;
    await f.write('\r');
    assert.equal(inputText(await f.waitStart(0)), 'pasted input');
    await f.expectNoSubmission(300);
    assert.equal(f.clientRequests('turn/start').length, 1);
    return { pasteOnly, firstEnter: f.clientRequests('turn/start').length };
  });
});

test('outer bracketed multiline paste and LF stay drafts until explicit Enter', { skip: !enabled }, async t => {
  await withOuter(t, async f => {
    await f.write('\x1b[200~first line\n둘째 줄\x1b[201~');
    await f.expectNoSubmission(400, 0);
    const multilinePaste = f.clientRequests('turn/start').length;
    await f.write('\n');
    await f.expectNoSubmission(400, 0);
    const intentionalLf = f.clientRequests('turn/start').length;
    await f.write('\x1b[200~last line\x1b[201~');
    await f.expectNoSubmission(400, 0);
    await f.write('\r');
    assert.equal(inputText(await f.waitStart(0)), 'first line\n둘째 줄\nlast line');
    await f.expectNoSubmission(300);
    assert.equal(f.clientRequests('turn/start').length, 1);
    return { multilinePaste, intentionalLf, firstEnter: f.clientRequests('turn/start').length };
  });
});
