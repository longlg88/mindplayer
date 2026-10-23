'use strict';
// Offline HTML interaction checks only. Never starts MindPlayer/CLI/providers.
// MINDPLAYER_PLAYWRIGHT_MODULE=/existing/playwright node docs/tests/office-view-gallery.test.cjs
// MINDPLAYER_WRITE_VALIDATION=1 also rewrites docs/plans/2026-10-23-office-view-gallery.ko.validation.json from this run.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');
const crypto = require('node:crypto');
const repo = path.resolve(__dirname, '../..');
const file = path.join(repo, 'docs/plans/2026-10-23-office-view-gallery.ko.html');
const record = path.join(repo, 'docs/plans/2026-10-23-office-view-gallery.ko.validation.json');
const hash = text => crypto.createHash('sha256').update(text).digest('hex');
const PANE_ORDER = ['s12', 's18', 's20', 's24', 's31', 's33', 's42'];
const GLYPHS = { s12: '!', s18: 'z', s20: '●', s24: '●', s31: '✓', s33: '○', s42: '!' };

// Sprites in the page must be the Rust sources' pixels, not a redraw.
function rustSprites() {
  const walker = fs.readFileSync(path.join(repo, 'crates/mindplayer-tui/src/walker.rs'), 'utf8');
  const mascot = fs.readFileSync(path.join(repo, 'crates/mindplayer-tui/src/mascot.rs'), 'utf8');
  const start = walker.indexOf('pub const ALL: &[Character] = &[');
  const body = walker.slice(start, walker.indexOf('\n];', start));
  const rows = text => [...text.matchAll(/"([.A-E]+)"/g)].map(m => m[1]);
  const walkers = body.split('Character {').slice(1).map(chunk => {
    const frames = rows(chunk.slice(chunk.indexOf('frames:'), chunk.indexOf('sit:')));
    return { id: /id: "([^"]+)"/.exec(chunk)[1], frames: [frames.slice(0, 8), frames.slice(8)],
      sit: rows(chunk.slice(chunk.indexOf('sit:'), chunk.indexOf('sleep:'))), sleep: rows(chunk.slice(chunk.indexOf('sleep:'))).slice(0, 8) };
  });
  const spout = [...mascot.slice(mascot.indexOf('const SPOUT'), mascot.indexOf('const VARIANTS')).matchAll(/"([.A-F]+)"/g)].map(m => m[1]);
  const first = mascot.slice(mascot.indexOf('const VARIANTS'));
  const whale = spout.concat([...first.slice(0, first.indexOf('eye:')).matchAll(/"([.A-F]+)"/g)].map(m => m[1]));
  return { walkers, whale };
}

// Text cells (labels, status bubbles, wall signs) must never share a cell, and a desk plate must never cover a character.
function clashes(frame) {
  const boxes = [];
  for (const l of frame.labels) boxes.push({ kind: `label:${l.sid}`, row: l.row, c0: l.col, c1: l.col + l.w });
  for (const b of frame.bubbles) boxes.push({ kind: `bubble:${b.sid}`, row: b.row, c0: b.col, c1: b.col + 3 });
  for (const s of frame.signs || []) boxes.push({ kind: 'sign', row: s.row, c0: s.col, c1: s.col + s.w });
  const hits = [];
  for (let a = 0; a < boxes.length; a++) for (let b = a + 1; b < boxes.length; b++) {
    const A = boxes[a], B = boxes[b];
    if (A.row === B.row && A.c0 < B.c1 && B.c0 < A.c1) hits.push(`${A.kind} x ${B.kind} @${A.row}`);
  }
  for (const A of boxes) if (A.kind.startsWith('label')) for (const s of frame.sprites)
    if (s.y0 < 2 * A.row + 2 && 2 * A.row < s.y1 && s.x0 < A.c1 && A.c0 < s.x1) hits.push(`${A.kind} covers ${s.sid}`);
  return hits;
}

async function main() {
  const source = fs.readFileSync(file, 'utf8');
  const before = hash(source);
  const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-office-qa-'));
  const result = { scope: 'offline standalone HTML synthetic interactions, not native runtime proof',
    verifiedAt: new Date().toISOString(), before, after: null, result: 'failed',
    passed: [], failed: [], screenshots: [], geometry: [], keyboardEvents: 0, proposals: 0, syntheticSessions: 0, spritesMatchRustSource: false,
    externalRequests: 0, pageErrors: 0, consoleErrors: 0, providerCalls: 0, cleanup: { browser: false, server: false } };
  let browser, server;
  try {
    for (const m of source.matchAll(/<script>([\s\S]*?)<\/script>/g)) new vm.Script(m[1]);
    assert.doesNotMatch(source, /\b(?:fetch|XMLHttpRequest|WebSocket|localStorage|sessionStorage)\s*[.(]/);
    assert.doesNotMatch(source, /https?:\/\//);
    result.passed.push('inline syntax, no network/storage APIs, no external URLs');
    const { chromium } = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');
    server = http.createServer((req, res) => {
      if (req.url === '/favicon.ico') { res.writeHead(204).end(); return; }
      if (req.url !== '/') { res.writeHead(404).end(); return; }
      res.setHeader('Content-Type', 'text/html; charset=utf-8'); res.end(source);
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const origin = `http://127.0.0.1:${server.address().port}`;
    browser = await chromium.launch({ channel: 'chrome', headless: true });

    for (const motion of ['no-preference', 'reduce']) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, reducedMotion: motion });
      await context.route('**/*', async route => {
        if (new URL(route.request().url()).origin !== origin) { result.externalRequests++; await route.abort(); }
        else await route.continue();
      });
      const page = await context.newPage();
      page.on('pageerror', () => result.pageErrors++);
      page.on('console', msg => { if (msg.type() === 'error') result.consoleErrors++; });
      await page.goto(origin);
      const office = (expr, arg) => page.evaluate(expr, arg);
      await office(() => window.__office.pause());
      const frame = () => office(() => window.__office.frame);
      const sim = () => office(() => JSON.parse(JSON.stringify(window.__office.sim)));
      const choose = k => office(k => window.__office.selectDraft(k), k);
      const tick = n => office(n => window.__office.tick(n), n);
      const press = async key => { await page.locator('#terminal').focus(); await page.keyboard.press(key); result.keyboardEvents++; };
      const canvasHash = () => page.evaluate(() => document.getElementById('screen').toDataURL());
      assert.equal(await office(() => window.__office.reduce), motion === 'reduce');

      if (motion === 'no-preference') {
        const page_ = rustSprites(), spr = await office(() => window.__office.SPR);
        assert.deepEqual(spr.walkers.map(w => ({ id: w.id, frames: w.frames, sit: w.sit, sleep: w.sleep })), page_.walkers);
        assert.deepEqual(spr.whale.rows, page_.whale);
        const blink = await office(() => [window.__office.WHALE_FRAME(0).rows, window.__office.WHALE_FRAME(2).rows]);
        assert.deepEqual(blink[1], page_.whale, 'open-eye frame equals mascot.rs');
        const changed = []; blink[0].forEach((row, r) => [...row].forEach((ch, c) => { if (ch !== page_.whale[r][c]) changed.push([r, c, ch]); }));
        assert.deepEqual(changed, [[6, 5, 'A'], [6, 6, 'A']], 'blink closes exactly mascot.rs eye cells (6,5) and (6,6)');
        result.spritesMatchRustSource = true; result.passed.push('six walker characters and the first mascot variant equal walker.rs / mascot.rs pixel rows; blink closes only the mascot.rs eye cells');

        result.proposals = await page.locator('.tab').count(); assert.equal(result.proposals, 5);
        result.syntheticSessions = (await sim()).sessions.length; assert.equal(result.syntheticSessions, 7);
        const unique = new Set();
        for (let k = 0; k < 5; k++) {
          await choose(k); unique.add(hash(await canvasHash()));
          await page.locator('[data-view="current"]').click(); const cur = await frame();
          assert.equal(cur.view, 'current'); assert.equal(cur.rooms.length, 0, 'current screen has no office');
          await page.locator('[data-view="proposed"]').click();
          const shot = path.join(artifacts, `${k + 1}-desktop.png`); await page.screenshot({ path: shot, fullPage: true }); result.screenshots.push(shot);
        }
        assert.equal(unique.size, 5); result.passed.push('five distinct proposals, each with a current-screen reconstruction and no office in it');

        for (let k = 0; k < 5; k++) for (const n of [0, 2, 4, 25]) {
          await choose(k); await tick(n); const f = await frame();
          assert.deepEqual(clashes(f), [], `draft ${k + 1} tick ${n}`);
          for (const room of f.rooms) if (room.bounds) {
            const { x0, x1, y0, y1 } = room.bounds, r = room.rect;
            assert.ok(x0 >= r.x && x1 <= r.x + r.w && y0 >= r.y && y1 <= r.y + r.h, `draft ${k + 1} room inside its area ${JSON.stringify({ bounds: room.bounds, r })}`);
          }
          const glyphs = Object.fromEntries(f.bubbles.map(b => [b.sid, b.text.trim().replace('Z', 'z')]));
          assert.deepEqual(glyphs, GLYPHS, `draft ${k + 1} tick ${n} glyph per session`);
        }
        result.passed.push('every draft at 4 ticks: no text cell overlap, plates never cover a character, room stays in its area, each session shows its own status glyph');

        await choose(0); await press('ArrowDown');
        const s1 = await sim(); assert.equal(s1.selected, 1);
        assert.equal((await frame()).labels.find(l => l.sid === 's18').bg, '#303c60');
        await choose(1); await press('j'); assert.equal((await sim()).selected, 1);
        assert.equal((await frame()).labels.find(l => l.sid === 's18').bg, '#303c60');
        assert.equal((await frame()).labels.find(l => l.sid === 's12').bg, '#1d2129');
        result.passed.push('01/02: list selection moves the highlighted desk plate');

        await choose(2); assert.equal((await sim()).office, true);
        await press('ArrowRight'); assert.notEqual((await sim()).selected, 0);
        await press('ArrowDown'); await press('Enter'); assert.match(await page.locator('#live').textContent(), /실제 열기 없음/);
        await press('o'); assert.equal((await sim()).office, false); assert.equal((await frame()).rooms.length, 0);
        await press('o'); assert.equal((await sim()).office, true); await press('Escape'); assert.equal((await sim()).office, false);
        await page.locator('#terminal').evaluate(el => el.dispatchEvent(new KeyboardEvent('keydown', { key: 'ㅐ', code: 'KeyO', bubbles: true, cancelable: true })));
        result.keyboardEvents++; assert.equal((await sim()).office, true, 'Korean layout ㅐ on the O key opens the office');
        result.passed.push('03: o opens and closes the office (also ㅐ on a Korean layout), Esc returns, arrows move desks, Enter is synthetic only');

        await choose(3); const strip = await frame();
        assert.deepEqual([...strip.labels].sort((a, b) => a.col - b.col).map(l => l.sid), PANE_ORDER);
        await press('ArrowRight'); assert.equal((await sim()).focus, 1);
        assert.equal((await frame()).labels.find(l => l.sid === 's18').bg, '#303c60');
        result.passed.push('04: seven desks in pane order, focused pane and desk move together');

        await choose(4); assert.equal((await sim()).saver, true);
        await press('Shift'); assert.equal((await sim()).saver, true, 'modifier-only key passes through');
        await press('j'); const after = await sim();
        assert.equal(after.saver, false); assert.equal(after.selected, 0, 'the waking key is not passed to the list');
        await page.locator('#b-away').click(); assert.equal((await sim()).saver, true);
        result.passed.push('05: a modifier-only key keeps the away screen, the next key leaves it without moving the list, away button returns');

        await choose(0); await page.locator('#terminal').focus(); await page.keyboard.press('Tab');
        assert.notEqual(await page.evaluate(() => document.activeElement.id), 'terminal');
        result.passed.push('Tab exits the terminal surface');
      }

      for (let k = 0; k < 5; k++) {
        await choose(k);
        await page.locator('#b-start').click();
        if (motion === 'no-preference' && k !== 3) { await tick(4); assert.equal((await sim()).sessions.find(s => s.sid === 's33').walking, true, `draft ${k + 1} walks`); }
        await page.locator('#b-end').click(); await tick(220);
        const st = await sim(), s33 = st.sessions.find(s => s.sid === 's33'), s31 = st.sessions.find(s => s.sid === 's31');
        assert.equal(s33.status, 'working'); assert.equal(s33.walking, false); assert.equal(s31.gone, true, `draft ${k + 1} ended session leaves`);
        if (k !== 3) { const L = await office(k => window.__office.LAYOUTS[window.__office.DRAFTS[k].layout], k); const [i, j] = L.desks.s33; assert.deepEqual(s33.pos, { i: i - 1, j: j - 1 }); }
        const f = await frame(); assert.deepEqual(clashes(f), [], `draft ${k + 1} after transitions`);
        assert.equal(f.labels.find(l => l.sid === 's31').fg, '#aeb7c6', 'ended desk plate turns ended grey');
        await page.locator('#b-end').click(); assert.match(await page.locator('#live').textContent(), /이미 ended/);
        if (motion === 'no-preference' && k === 2) { const shot = path.join(artifacts, '3-after-transitions.png'); await page.locator('#terminal').screenshot({ path: shot }); result.screenshots.push(shot); }
      }
      result.passed.push(`${motion}: idle → working walks to the chair, done → ended leaves, a repeated click reports the current state, in all five drafts`);

      if (motion === 'no-preference') for (const width of [1440, 1280, 768, 390, 320]) {
        await page.setViewportSize({ width, height: 900 }); await choose(2);
        const m = await page.evaluate(() => { const el = document.querySelector('.terminal-scroll');
          return { viewport: innerWidth, page: document.documentElement.scrollWidth, terminalClient: el.clientWidth, terminalScroll: el.scrollWidth }; });
        result.geometry.push(m); assert.ok(m.page <= width, JSON.stringify(m)); assert.ok(m.terminalClient > 0);
        if (width <= 1280) assert.ok(m.terminalScroll > m.terminalClient, JSON.stringify(m));
        if (width === 390) { const shot = path.join(artifacts, '3-390px.png'); await page.screenshot({ path: shot, fullPage: true }); result.screenshots.push(shot); }
        result.passed.push(`viewport ${width}: no document overflow, fixed cell terminal scrolls inside its frame`);
      }
      await context.close();
    }
    assert.equal(result.externalRequests, 0); assert.equal(result.pageErrors, 0); assert.equal(result.consoleErrors, 0);
    assert.equal(hash(fs.readFileSync(file)), before); result.after = before; result.result = 'passed';
  } catch (error) {
    result.failed.push({ name: error.name, message: error.message }); throw error;
  } finally {
    if (browser) { await browser.close(); result.cleanup.browser = true; }
    if (server) { await new Promise(resolve => server.close(resolve)); result.cleanup.server = true; }
    fs.writeFileSync(path.join(artifacts, 'validation.json'), JSON.stringify(result, null, 2) + '\n');
    if (process.env.MINDPLAYER_WRITE_VALIDATION === '1' && result.result === 'passed') {
      const summary = { scope: result.scope, verifiedAt: result.verifiedAt, html: path.basename(file), test: '../tests/office-view-gallery.test.cjs',
        result: result.result, passedContracts: result.passed.length, checked: result.passed, keyboardEvents: result.keyboardEvents,
        proposals: result.proposals, syntheticSessions: result.syntheticSessions, motionModes: ['no-preference', 'reduce'],
        spritesMatchRustSource: result.spritesMatchRustSource, viewportWidths: result.geometry.map(g => g.viewport),
        documentOverflow: Math.max(...result.geometry.map(g => g.page - g.viewport)), externalRequests: result.externalRequests,
        pageErrors: result.pageErrors, consoleErrors: result.consoleErrors, providerCalls: result.providerCalls,
        sourceSha256Before: result.before, sourceSha256After: result.after, cleanup: result.cleanup,
        diagnosticReceipt: `<os-tmpdir>/${path.basename(artifacts)}/validation.json`,
        notVerified: ['Real ratatui rendering and redraw cost', 'Small real terminals and fonts with other Hangul widths',
          'Key collisions for the draft-only o key in the real TUI', 'Layout for more than seven desks', 'Screen-reader behaviour'] };
      fs.writeFileSync(record, JSON.stringify(summary, null, 2) + '\n');
    }
    console.log(JSON.stringify({ result: result.result, passed: result.passed.length, failed: result.failed,
      keyboardEvents: result.keyboardEvents, externalRequests: result.externalRequests,
      pageErrors: result.pageErrors, consoleErrors: result.consoleErrors, providerCalls: result.providerCalls,
      before: result.before, after: result.after, cleanup: result.cleanup, receipt: path.join(artifacts, 'validation.json') }));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
