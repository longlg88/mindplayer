'use strict';

// HTML 시안만 검증한다. Rust / 실제 CLI / Jev / 설치 검증이 아니다.
// MINDPLAYER_PLAYWRIGHT_MODULE=/existing/playwright node docs/tests/session-routing-gallery.test.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const http = require('node:http');
const vm = require('node:vm');
const crypto = require('node:crypto');

const relativeHtml = 'docs/plans/2026-10-17-session-routing-gallery.ko.html';
const htmlPath = path.resolve(__dirname, '../..', relativeHtml);
const ids = Array.from({ length: 12 }, (_, index) => index + 1);
const groups = [];
const screenshots = [];
const screenshotPixels = [];
const sha256 = value => crypto.createHash('sha256').update(value).digest('hex');
const pass = name => { groups.push(name); console.log(`PASS ${name}`); };
const queueSize = state => Array.isArray(state.queue) ? state.queue.length : Number(state.queue);

async function main() {
  const source = fs.readFileSync(htmlPath, 'utf8');
  assert.match(source, /<html\b[^>]*lang=["']ko["']/);
  const scripts = [...source.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)];
  assert.ok(scripts.length > 0, 'inline interaction script');
  for (const [index, script] of scripts.entries()) new vm.Script(script[1], { filename: `${relativeHtml}:${index}` });
  assert.doesNotMatch(source, /<(?:script|link|img|iframe)\b[^>]+(?:src|href)=["']https?:/i);
  assert.doesNotMatch(scripts.map(script => script[1]).join('\n'), /\b(?:fetch|XMLHttpRequest|WebSocket|EventSource|localStorage|sessionStorage|indexedDB|sendBeacon)\b/);
  assert.doesNotMatch(source, /(?:sk-[A-Za-z0-9_-]{18,}|vck_[A-Za-z0-9_-]{15,}|Bearer\s+[A-Za-z0-9._-]{20,})/);
  pass('static: Korean, inline JS syntax, no external assets/API/storage or credential patterns');

  const { chromium } = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');
  const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-routing-gallery-'));
  const server = http.createServer((request, response) => {
    if (request.url === '/gallery') {
      response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
      response.end(source);
    } else if (request.url === '/favicon.ico') {
      response.writeHead(204); response.end();
    } else { response.writeHead(404); response.end(); }
  });
  let browser;
  const errors = [], externalRequests = [];
  try {
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const base = `http://127.0.0.1:${server.address().port}`;
    browser = await chromium.launch({ channel: 'chrome', headless: true });
    const context = await browser.newContext({ viewport: { width: 1440, height: 1050 }, reducedMotion: 'reduce' });
    await context.route('**/*', route => {
      const url = route.request().url();
      if (url === `${base}/gallery` || url === `${base}/favicon.ico`) return route.continue();
      externalRequests.push(url);
      return route.abort();
    });
    const page = await context.newPage();
    const keyboard = await context.newCDPSession(page);
    page.on('pageerror', error => errors.push(String(error)));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    await page.goto(`${base}/gallery`);
    await page.waitForFunction(() => window.MindPlayerGallery && typeof window.MindPlayerGallery.state === 'function');
    const article = id => page.locator(`article.idea[data-idea="${id}"]`);
    const terminal = id => article(id).locator('.terminal');
    const state = id => page.evaluate(id => window.MindPlayerGallery.state(id), id);
    const reset = async (id, options = {}) => {
      options = { entry: 'existing', mode: 'native', ready: 'ready', consent: false, ...options };
      await page.evaluate(({ id, options }) => window.MindPlayerGallery.reset(id, options), { id, options });
      await terminal(id).focus();
      return state(id);
    };
    const key = (id, value) => terminal(id).press(value);
    const consent = id => key(id, [5, 6].includes(id) ? 'F4' : 'c');
    const openJev = id => key(id, id === 9 ? 'F8' : 'Enter');
    const terminalText = id => terminal(id).locator('.line').evaluateAll(lines =>
      lines.map(line => line.textContent).join('\n'));
    async function type(id, text) {
      await terminal(id).focus();
      for (const character of text) {
        if (character.codePointAt(0) > 127) {
          // Playwright press() has no Hangul mapping; CDP emits trusted keydown
          // instead of silently testing insertText without the input handler.
          await keyboard.send('Input.dispatchKeyEvent', { type: 'keyDown', key: character, text: character });
          await keyboard.send('Input.dispatchKeyEvent', { type: 'keyUp', key: character });
        } else await key(id, character === ' ' ? 'Space' : character);
      }
    }
    async function phase(id, expected, timeout = 6000) {
      await page.waitForFunction(({ id, expected }) => window.MindPlayerGallery.state(id).phase === expected,
        { id, expected }, { timeout });
      return state(id);
    }
    async function complete(id) {
      const fixture = await page.evaluate(id => {
        if (typeof window.MindPlayerGallery.settle !== 'function') return false;
        window.MindPlayerGallery.settle(id, 'completed');
        return true;
      }, id);
      if (fixture) assert.equal((await state(id)).phase, 'completed');
      else await phase(id, 'completed');
    }
    async function screenshot(name, target = page) {
      const output = path.join(artifacts, `${name}.png`);
      if (target !== page) await target.scrollIntoViewIfNeeded();
      await page.evaluate(async () => {
        await document.fonts.ready;
        await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      });
      const png = await target.screenshot({ path: output, ...(target === page ? { fullPage: true } : {}) });
      if (target !== page) {
        const pixels = await page.evaluate(async encoded => {
          const image = new Image(); image.src = `data:image/png;base64,${encoded}`;
          await image.decode();
          const canvas = document.createElement('canvas'); canvas.width = image.width; canvas.height = image.height;
          const context = canvas.getContext('2d'); context.drawImage(image, 0, 0);
          const data = context.getImageData(0, 0, image.width, image.height).data;
          let dark = 0;
          for (let index = 0; index < data.length; index += 4) {
            if (data[index] < 90 && data[index + 1] < 100 && data[index + 2] < 120) dark++;
          }
          return { width: image.width, height: image.height, darkFraction: dark / (image.width * image.height) };
        }, png.toString('base64'));
        assert.ok(pixels.darkFraction > 0.5, `${name}: terminal pixels painted, not a blank gallery card`);
        screenshotPixels.push({ name, ...pixels });
      }
      screenshots.push(output);
    }

    assert.equal(await page.locator('article.idea').count(), 12);
    assert.deepEqual(await page.locator('article.idea').evaluateAll(nodes => nodes.map(node => Number(node.dataset.idea))), ids);
    assert.ok((await page.locator('h1').innerText()).trim().length < 80, 'minimal gallery heading');
    assert.equal(await page.locator('input,select,button,textarea,[contenteditable="true"]').count(), 0);
    const structures = [];
    const gridErrors = [];
    for (const id of ids) {
      assert.equal(await terminal(id).getAttribute('tabindex'), '0');
      assert.equal(await terminal(id).getAttribute('role'), 'application');
      const grid = await terminal(id).evaluate(node => ({
        lines: [...node.querySelectorAll('.line')].map(line => line.textContent),
        tags: [...node.querySelectorAll('*')].map(element => element.tagName),
        columns: [...node.querySelectorAll('.line')].map(line =>
          [...line.children].reduce((total, span) => total + Number(span.style.width.match(/\*\s*(\d+)/)?.[1]), 0)),
        overflow: [...node.querySelectorAll('.line')].some(line =>
          [...line.children].reduce((total, span) => total + span.getBoundingClientRect().width, 0) > node.getBoundingClientRect().width + 1),
      }));
      assert.equal(grid.lines.length, 29, `idea ${id}: 29 terminal rows`);
      assert.ok(grid.tags.every(tag => tag === 'SPAN' || tag === 'DIV'), `idea ${id}: text grid only`);
      assert.ok(grid.lines.every(line => !/[\r\n\t]/.test(line)), `idea ${id}: no embedded grid row breaks`);
      grid.columns.forEach((columns, row) => {
        if (columns !== 88) gridErrors.push({ id, row, columns });
      });
      if (grid.overflow) gridErrors.push({ id, overflow: true });
      structures.push({ id, screen: sha256(grid.lines.join('\n')), lines: grid.lines });
      const initial = await reset(id);
      assert.equal(initial.screen, 'choice');
      assert.equal(initial.mode, 'native');
      assert.equal(initial.consent, false);
      assert.equal(initial.turns, 0);
      assert.equal(queueSize(initial), 0);
    }
    assert.equal(new Set(structures.map(item => item.screen)).size, 12, '12 different rendered terminal compositions');
    const clone = await state(1);
    await page.evaluate(() => { const copy = window.MindPlayerGallery.state(1); copy.mode = 'jev'; if (Array.isArray(copy.queue)) copy.queue.push('mutated'); });
    assert.deepEqual(await state(1), clone, 'state() returns a detached clone');
    if (gridErrors.length) console.error('FAIL 88-column grid: ' + JSON.stringify(gridErrors));
    else pass('grid: exactly 88 logical cells × 29 rows without cell overflow');
    pass('gallery: 12 focusable application grids, distinct rendered compositions, explicit reset snapshots, cloned state');
    await screenshot('desktop-1440-initial');

    for (const id of ids) {
      for (const entry of ['existing', 'new']) {
        await reset(id, { entry });
        assert.equal((await state(id)).entry, entry);
        await key(id, 'Enter');
        assert.equal((await state(id)).screen, 'session', `idea ${id}/${entry}: native entry`);
        await type(id, '한글 첫 요청');
        await key(id, 'Enter');
        let current = await state(id);
        assert.equal(current.mode, 'native');
        assert.equal(current.phase, 'running');
        assert.equal(current.turns, 1);
        await complete(id);
        await type(id, '다음 요청');
        await key(id, 'Enter');
        current = await state(id);
        assert.equal(current.phase, 'running', 'native follow-up bypasses Jev');
        assert.equal(current.turns, 2);

        await reset(id, { entry });
        await key(id, 'ArrowRight');
        assert.equal((await state(id)).mode, 'jev');
        await openJev(id);
        current = await state(id);
        assert.equal(current.screen, 'choice', 'Jev key readiness does not imply consent');
        assert.equal(current.turns, 0);
        await consent(id);
        assert.equal((await state(id)).consent, true);
        await consent(id);
        assert.equal((await state(id)).consent, false);
        await consent(id);
        await openJev(id);
        assert.equal((await state(id)).screen, 'session');
        await type(id, '새 판단 요청');
        await key(id, 'Enter');
        current = await state(id);
        assert.equal(current.mode, 'jev');
        assert.equal(current.phase, 'judging');
        assert.equal(current.turns, 1);
        await complete(id);
        await type(id, '후속 판단 요청');
        await key(id, 'Enter');
        current = await state(id);
        assert.equal(current.phase, 'judging', 'every follow-up gets a fresh judgment');
        assert.equal(current.turns, 2);
      }
      pass(`idea ${id}: both entries, native + follow-up, Jev consent + follow-up`);
    }

    for (const id of ids) {
      for (const entry of ['existing', 'new']) {
        for (const ready of ['missing', 'denied', 'unsupported']) {
          await reset(id, { entry, mode: 'jev', ready, consent: true });
          await openJev(id);
          let current = await state(id);
          assert.equal(current.screen, 'choice', `idea ${id}/${entry}/${ready}: fail closed`);
          assert.equal(current.turns, 0);
          assert.equal(queueSize(current), 0);
          await key(id, 'ArrowLeft');
          await key(id, 'Enter');
          current = await state(id);
          assert.equal(current.screen, 'session');
          assert.equal(current.mode, 'native', 'explicit native selection still works');
        }
      }
      await reset(id, { entry: 'existing' });
      await key(id, 'ArrowDown');
      assert.equal((await state(id)).mode, 'jev');
      await key(id, 'ArrowUp');
      assert.equal((await state(id)).mode, 'native');
      await key(id, 'Escape');
      assert.equal((await state(id)).screen, 'list');
      assert.equal((await state(id)).turns, 0);
      await key(id, 'n');
      assert.equal((await state(id)).entry, 'new');
      assert.equal((await state(id)).screen, 'choice');
      await key(id, 'Escape');
      assert.equal((await state(id)).screen, 'list');
      await key(id, 'Enter');
      assert.equal((await state(id)).screen, 'choice');
      await reset(id, { entry: 'existing' });
      if ([5, 6].includes(id)) {
        await key(id, 'Escape');
        assert.equal((await state(id)).screen, 'list');
      }
      await key(id, 'n');
      assert.equal((await state(id)).entry, 'new');
      assert.equal((await state(id)).screen, 'choice');
    }
    pass('all 12: both entries × missing/denied/unsupported, explicit native recovery, arrow keys, Esc cancellation, n new entry');

    for (const id of [5, 6]) {
      await reset(id);
      await type(id, 'nc');
      let current = await state(id);
      assert.equal(current.screen, 'choice');
      assert.equal(current.entry, 'existing', 'search n does not create a new session');
      assert.equal(current.consent, false, 'search c does not toggle consent');
      assert.equal(current.query, 'nc');
      await type(id, 'zzzz');
      await key(id, 'Enter');
      current = await state(id);
      assert.equal(current.screen, 'choice', 'no search match cannot open a session');
      assert.equal(current.turns, 0);
      assert.match(await terminalText(id), /일치하는/);
      for (let index = 0; index < 4; index++) await key(id, 'Backspace');
      assert.equal((await state(id)).query, 'nc');
      await key(id, 'ArrowRight');
      assert.equal((await state(id)).mode, 'native', 'filtered arrows cannot select a nonmatching route');
      await key(id, 'Backspace');
      await key(id, 'Backspace');
      assert.equal((await state(id)).query, '');
      await key(id, 'ArrowRight');
      assert.equal((await state(id)).mode, 'jev');
      await consent(id);
      assert.equal((await state(id)).consent, true, 'F4 toggles consent without entering query text');
      assert.equal((await state(id)).query, '');
      await openJev(id);
      assert.equal((await state(id)).screen, 'session');
    }
    pass('ideas 5/6: n/c search text, no-match block, Backspace filtering, cleared arrows and F4 consent');

    await reset(1, { mode: 'jev', consent: true });
    await openJev(1);
    await type(1, '자동 상태 전이');
    await key(1, 'Enter');
    await phase(1, 'judging');
    await phase(1, 'requested');
    await phase(1, 'running');
    await phase(1, 'completed');
    await type(1, '실행 첫째');
    await key(1, 'Enter');
    await phase(1, 'judging');
    for (const text of ['개별 큐 둘째', '개별 큐 셋째']) {
      await type(1, text);
      await key(1, 'Enter');
    }
    assert.equal(queueSize(await state(1)), 2, 'busy Enter keeps two separate queued inputs');
    assert.equal((await state(1)).turns, 2, 'queued inputs are not yet running');
    await type(1, '아직 보내지 않은 초안');
    await page.waitForFunction(() => {
      const current = window.MindPlayerGallery.state(1);
      return current.turns === 3 && current.phase === 'judging';
    }, null, { timeout: 6000 });
    assert.equal(queueSize(await state(1)), 1);
    assert.equal((await state(1)).draft, '아직 보내지 않은 초안', 'queued start preserves the unsent draft');
    assert.equal((await state(1)).current, '개별 큐 둘째');
    assert.ok((await terminalText(1)).includes('개별 큐 둘째'), 'first queued prompt rendered separately');
    await page.waitForFunction(() => {
      const current = window.MindPlayerGallery.state(1);
      return current.turns === 4 && current.phase === 'judging';
    }, null, { timeout: 6000 });
    assert.equal(queueSize(await state(1)), 0);
    assert.equal((await state(1)).draft, '아직 보내지 않은 초안');
    assert.equal((await state(1)).current, '개별 큐 셋째');
    assert.ok((await terminalText(1)).includes('개별 큐 셋째'), 'second queued prompt rendered separately');
    await phase(1, 'completed');
    pass('automatic lifecycle: judging → requested → running → completed; individual FIFO queue gets new judgments');

    await reset(2);
    await key(2, 'Enter');
    await type(2, '자동 네이티브 완료');
    await key(2, 'Enter');
    assert.equal((await state(2)).phase, 'running');
    await phase(2, 'completed');
    assert.equal((await state(2)).mode, 'native');
    pass('native automatic lifecycle: running → completed without Jev');

    for (const mode of ['native', 'jev']) {
      await reset(1, { mode, consent: true });
      if (mode === 'jev') await openJev(1);
      else await key(1, 'Enter');
      await key(1, 'Escape');
      assert.equal((await state(1)).screen, 'session', 'idle Esc keeps the native session context');
      assert.equal((await state(1)).phase, 'input');
      await type(1, '중단할 요청');
      await key(1, 'Enter');
      await type(1, '보존할 대기 요청');
      await key(1, 'Enter');
      await type(1, '보존할 초안');
      await key(1, 'Escape');
      const interrupted = await state(1);
      assert.equal(interrupted.screen, 'session');
      assert.equal(interrupted.phase, 'interrupted');
      assert.equal(interrupted.queuePaused, true);
      assert.deepEqual(interrupted.queue, ['보존할 대기 요청']);
      assert.equal(interrupted.draft, '보존할 초안');
      // Longer than the Jev completion + queued-start timers (2700 + 750 ms).
      await page.waitForTimeout(3700);
      assert.deepEqual(await state(1), interrupted, 'Esc cancels timers and prevents pending queue autorun');
      await key(1, 'Control+x');
      assert.equal((await state(1)).screen, 'list', 'only Ctrl-X returns a live session to the list');
      await key(1, 'Enter');
      assert.deepEqual(await state(1), interrupted, 'reattach preserves the interrupted session and paused queue');
    }
    pass('live Esc: native/Jev stay in session, interrupted + paused queue/draft persist with no automatic execution');

    for (const id of [1, 9, 10]) {
      await reset(id, { mode: 'jev', consent: true });
      await openJev(id);
      await type(id, '진행 중인 요청');
      await key(id, 'Enter');
      await type(id, '대기 요청');
      await key(id, 'Enter');
      // Freeze a synthetic busy state to isolate reattach from elapsed timers.
      await page.evaluate(id => window.MindPlayerGallery.settle(id, 'running'), id);
      const preserved = current => ({ mode: current.mode, phase: current.phase, turns: current.turns,
        queue: current.queue, queuePaused: current.queuePaused, entry: current.entry, consent: current.consent });
      const before = preserved(await state(id));
      await key(id, 'Control+x');
      assert.equal((await state(id)).screen, 'list');
      for (const value of ['ArrowRight', 'F8', 'F6', 'n']) {
        await key(id, value);
        const current = await state(id);
        assert.equal(current.screen, 'list', `idea ${id}: ${value} cannot reopen or replace a live session`);
        assert.deepEqual(preserved(current), before);
      }
      await key(id, 'Enter');
      assert.equal((await state(id)).screen, 'session');
      assert.deepEqual(preserved(await state(id)), before);
    }
    pass('reattach: Ctrl-X/list arrows/F8/F6/n/Enter preserve live mode, phase, turns and queue');

    assert.ok(await page.locator('.journey').count() >= 1, 'shared lifecycle frames');
    const layout = page.locator('.pane-layout-preview[data-layout="4+3"]');
    assert.equal(await layout.count(), 1);
    assert.equal(await layout.locator('.pane-row.top > *').count(), 4);
    assert.equal(await layout.locator('.pane-row.bottom > *').count(), 3);
    const pane = await layout.locator('.pane-row.top > *').first().boundingBox();
    const footer = await layout.locator('.layout-footer').boundingBox();
    assert.ok(pane && footer && footer.width > pane.width * 3.5, 'one full-width usage footer');
    pass('shared context: lifecycle journey, 4+3 panes and full-width usage footer');

    for (const id of ids) await reset(id);
    for (const width of [1440, 1280, 390]) {
      await page.setViewportSize({ width, height: 1050 });
      await page.evaluate(() => scrollTo(0, 0));
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true, `${width}px: no page horizontal overflow`);
      for (const id of ids) {
        const bounds = await terminal(id).boundingBox();
        assert.ok(bounds && bounds.width > 0 && bounds.height > 0, `${width}px idea ${id}: terminal visible`);
      }
      await screenshot(`gallery-${width}`);
    }
    await page.setViewportSize({ width: 1440, height: 1050 });
    for (const id of ids) await screenshot(`idea-${String(id).padStart(2, '0')}`, article(id));
    assert.deepEqual(errors, [], 'browser/console errors');
    assert.deepEqual(externalRequests, [], 'external requests');
    assert.equal(fs.readFileSync(htmlPath, 'utf8'), source, 'HTML did not change during verification');
    pass('responsive: 1440/1280/390, 12 idea captures, zero external requests or browser errors');

    const receipt = {
      schemaVersion: 1, verifiedAt: new Date().toISOString(), result: gridErrors.length ? 'failed' : 'passed',
      scope: 'standalone HTML UI gallery only; not terminal runtime, Rust, CLI, Jev, authentication or installation',
      source: { html: relativeHtml, sha256: sha256(source) },
      browser: { name: 'Google Chrome via installed Playwright', version: browser.version(), headless: true },
      groups, ideaCount: 12, viewports: [1440, 1280, 390], screenshots, screenshotPixels,
      gridErrors,
      externalRequests: externalRequests.length, browserErrors: errors.length,
      actualJevCalls: 0, actualModelCalls: 0, appCodeChanged: false, appInstalled: false,
      compositionHashes: structures.map(({ id, screen }) => ({ id, sha256: screen })),
      notVerified: ['real terminal rendering', 'Rust integration', 'actual provider/model/effort application', 'cost or quality improvements'],
    };
    const output = process.env.MINDPLAYER_ROUTING_GALLERY_RECEIPT;
    if (output) fs.writeFileSync(output, `${JSON.stringify(receipt, null, 2)}\n`);
    console.log(JSON.stringify(receipt, null, 2));
    if (gridErrors.length) process.exitCode = 1;
  } finally {
    if (browser) await browser.close();
    if (server.listening) await new Promise(resolve => server.close(resolve));
  }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
