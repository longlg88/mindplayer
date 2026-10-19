'use strict';

// 시작 시 1회 선택하는 HTML 시안만 검증한다. 실제 앱 / CLI / Jev 검증이 아니다.
// MINDPLAYER_PLAYWRIGHT_MODULE=/existing/playwright node docs/tests/startup-routing-gallery.test.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const http = require('node:http');
const vm = require('node:vm');
const crypto = require('node:crypto');

const relativeHtml = 'docs/plans/2026-10-17-startup-routing-gallery.ko.html';
const htmlPath = path.resolve(__dirname, '../..', relativeHtml);
const ids = Array.from({ length: 10 }, (_, index) => index + 1);
const defaults = { screen: 'startup', selected: 'native', runMode: null, ready: 'ready',
  consent: false, query: '', sessionCount: 0, phase: 'input' };
const sha256 = value => crypto.createHash('sha256').update(value).digest('hex');

async function main() {
  const source = fs.readFileSync(htmlPath, 'utf8');
  const started = Date.now();
  const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-startup-gallery-'));
  const receiptPath = path.join(artifacts, 'validation.json');
  const receipt = {
    schemaVersion: 1, result: 'failed', verifiedAt: new Date().toISOString(),
    scope: 'standalone startup-choice HTML mock only; keyboard simulation, not a real MindPlayer process',
    source: { html: relativeHtml, sha256: sha256(source) },
    ideaCount: 10, viewports: [1440, 1280, 390], groups: [], screenshots: [],
    browserErrors: [], externalRequests: [], keyboardEvents: 0,
    actualJevCalls: 0, actualModelCalls: 0, authenticationVerified: false,
    appCodeChanged: false, appInstalled: false,
    notVerified: ['real terminal rendering/IME', 'Rust/CLI integration', 'real process lifetime',
      'provider authentication, inference, model/effort application', 'subjective design quality'],
  };
  const pass = name => { receipt.groups.push(name); console.log(`PASS ${name}`); };
  let browser;
  const server = http.createServer((request, response) => {
    if (request.url === '/gallery') {
      response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' }); response.end(source);
    } else if (request.url === '/favicon.ico') {
      response.writeHead(204); response.end();
    } else { response.writeHead(404); response.end(); }
  });

  try {
    assert.match(source, /<html\b[^>]*lang=["']ko["']/);
    const scripts = [...source.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)];
    assert.ok(scripts.length > 0, 'inline startup interaction script');
    for (const [index, script] of scripts.entries()) new vm.Script(script[1], { filename: `${relativeHtml}:${index}` });
    assert.doesNotMatch(source, /<(?:script|link|img|iframe)\b[^>]+(?:src|href)=["']https?:/i);
    assert.doesNotMatch(scripts.map(script => script[1]).join('\n'),
      /\b(?:fetch|XMLHttpRequest|WebSocket|EventSource|localStorage|sessionStorage|indexedDB|sendBeacon)\b/);
    assert.doesNotMatch(source, /(?:sk-[A-Za-z0-9_-]{18,}|vck_[A-Za-z0-9_-]{15,}|Bearer\s+[A-Za-z0-9._-]{20,})/);
    pass('static: Korean, inline JS syntax, no external assets/API/storage or credential patterns');

    const { chromium } = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');
    await new Promise((resolve, reject) => {
      server.once('error', reject); server.listen(0, '127.0.0.1', resolve);
    });
    const base = `http://127.0.0.1:${server.address().port}`;
    browser = await chromium.launch({ channel: 'chrome', headless: true });
    receipt.browser = { name: 'Google Chrome via installed Playwright', version: browser.version(), headless: true };
    const context = await browser.newContext({ viewport: { width: 1440, height: 1050 }, reducedMotion: 'reduce' });
    await context.route('**/*', route => {
      const url = route.request().url();
      if (url === `${base}/gallery` || url === `${base}/favicon.ico`) return route.continue();
      receipt.externalRequests.push(url); return route.abort();
    });
    const page = await context.newPage();
    page.setDefaultTimeout(5000);
    page.on('pageerror', error => receipt.browserErrors.push(String(error)));
    page.on('console', message => { if (message.type() === 'error') receipt.browserErrors.push(message.text()); });
    const keyboard = await context.newCDPSession(page);
    await page.goto(`${base}/gallery`);
    await page.waitForFunction(() => window.MindPlayerStartupGallery);
    const article = id => page.locator(`article.idea[data-idea="${id}"]`);
    const terminal = id => article(id).locator('.terminal');
    const state = id => page.evaluate(id => window.MindPlayerStartupGallery.state(id), id);
    const reset = async (id, options = {}) => {
      await page.evaluate(({ id, options }) => window.MindPlayerStartupGallery.reset(id, options), { id, options });
      await terminal(id).focus(); return state(id);
    };
    const key = (id, value) => { receipt.keyboardEvents++; return terminal(id).press(value); };
    const text = id => terminal(id).locator('.line').evaluateAll(lines => lines.map(line => line.textContent).join('\n'));
    async function type(id, value) {
      await terminal(id).focus();
      for (const character of value) {
        if (character.codePointAt(0) > 127) {
          // Trusted keydown exercises the character-grid handler; this is not an OS IME test.
          receipt.keyboardEvents++;
          await keyboard.send('Input.dispatchKeyEvent', { type: 'keyDown', key: character, text: character });
          await keyboard.send('Input.dispatchKeyEvent', { type: 'keyUp', key: character });
        } else await key(id, character === ' ' ? 'Space' : character);
      }
    }
    async function settle(id, phase) {
      await page.evaluate(({ id, phase }) => window.MindPlayerStartupGallery.settle(id, phase), { id, phase });
      assert.equal((await state(id)).phase, phase);
    }
    async function phase(id, expected) {
      await page.waitForFunction(({ id, expected }) => window.MindPlayerStartupGallery.state(id).phase === expected,
        { id, expected }, { timeout: 6000 });
    }
    async function enterRun(id, runMode) {
      await reset(id);
      if (runMode === 'jev') {
        await key(id, 'ArrowRight'); await key(id, 'Enter');
        assert.equal((await state(id)).screen, 'consent');
        await key(id, 'c');
      }
      await key(id, 'Enter');
      const current = await state(id);
      assert.equal(current.screen, 'list'); assert.equal(current.runMode, runMode);
    }
    async function screenshot(name, target = page) {
      if (target !== page) await target.scrollIntoViewIfNeeded();
      await page.evaluate(async () => {
        await document.fonts.ready;
        await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      });
      const output = path.join(artifacts, `${name}.png`);
      await target.screenshot({ path: output, ...(target === page ? { fullPage: true } : {}) });
      receipt.screenshots.push(output);
    }

    assert.deepEqual(await page.evaluate(() => window.MindPlayerStartupGallery.geometry), { columns: 88, rows: 29 });
    assert.equal(await page.locator('article.idea').count(), 10);
    assert.deepEqual(await page.locator('article.idea').evaluateAll(nodes => nodes.map(node => Number(node.dataset.idea))), ids);
    const patterns = await page.locator('article.idea').evaluateAll(nodes => nodes.map(node => node.dataset.pattern));
    assert.ok(patterns.every(pattern => typeof pattern === 'string' && pattern.trim().length > 0));
    assert.equal(new Set(patterns).size, 10, '10 unique startup UI patterns');
    assert.equal(await page.locator('form,input,select,button,textarea,[role="radio"],[role="radiogroup"],[contenteditable="true"]').count(), 0);
    assert.match(await text(1), /실제[^\n]{0,60}호출 없음/);
    receipt.compositions = [];
    for (const id of ids) {
      const initial = await reset(id);
      for (const [field, value] of Object.entries(defaults)) assert.deepEqual(initial[field], value, `idea ${id}: default ${field}`);
      const tabindex = await terminal(id).getAttribute('tabindex');
      assert.notEqual(tabindex, null, 'explicit terminal tabindex');
      assert.ok(Number(tabindex) >= 0, 'keyboard focusable terminal');
      const grid = await terminal(id).evaluate(node => ({
        tags: [...node.querySelectorAll('*')].map(element => element.tagName),
        lines: [...node.querySelectorAll('.line')].map(line => line.textContent),
        columns: [...node.querySelectorAll('.line')].map(line => [...line.children]
          .reduce((total, span) => total + Number(span.style.width.match(/\*\s*(\d+)/)?.[1]), 0)),
        overflow: [...node.querySelectorAll('.line')].some(line => [...line.children]
          .reduce((total, span) => total + span.getBoundingClientRect().width, 0) > node.getBoundingClientRect().width + 1),
      }));
      assert.equal(grid.lines.length, 29, `idea ${id}: 29 rows`);
      assert.ok(grid.tags.every(tag => tag === 'SPAN' || tag === 'DIV'), 'character grid, not browser widgets');
      assert.ok(grid.lines.every(line => !/[\r\n\t]/.test(line)));
      assert.ok(grid.columns.every(columns => columns === 88), `idea ${id}: 88 logical columns: ${grid.columns}`);
      assert.equal(grid.overflow, false, `idea ${id}: character runs stay inside the grid`);
      receipt.compositions.push({ id, pattern: patterns[id - 1], sha256: sha256(grid.lines.join('\n')) });
    }
    assert.equal(new Set(receipt.compositions.map(item => item.sha256)).size, 10);
    const beforeClone = await state(1);
    await page.evaluate(() => { const copy = window.MindPlayerStartupGallery.state(1); copy.selected = 'jev'; copy.sessionCount = 999; });
    assert.deepEqual(await state(1), beforeClone, 'state snapshots do not mutate the fixture');
    pass('10 unique patterns, default snapshots, cloned state, 88×29 focusable character grids, mock scope');

    for (const id of ids) {
      await reset(id);
      for (const value of ['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp']) {
        const previous = (await state(id)).selected;
        await key(id, value);
        assert.notEqual((await state(id)).selected, previous, `idea ${id}: ${value} toggles selection`);
      }
      for (const runMode of ['native', 'jev']) {
        await enterRun(id, runMode);
        assert.equal((await state(id)).sessionCount, 0, 'choosing process mode does not open a session');
        for (const value of ['ArrowRight', 'c']) {
          await key(id, value);
          assert.equal((await state(id)).screen, 'list');
          assert.equal((await state(id)).runMode, runMode, 'list cannot replace the once-per-run choice');
        }
        await key(id, 'Enter');
        assert.equal((await state(id)).screen, 'session', 'existing session opens without another chooser');
        assert.equal((await state(id)).entry, 'existing');
        assert.equal((await state(id)).sessionCount, 1);
        assert.equal((await state(id)).runMode, runMode);
        await type(id, '한글 첫 요청'); await key(id, 'Enter');
        assert.equal((await state(id)).current, '한글 첫 요청', 'Korean keydown text reaches the submitted prompt');
        assert.equal((await state(id)).phase, runMode === 'jev' ? 'judging' : 'running');
        await settle(id, 'completed');
        await type(id, '후속 요청'); await key(id, 'Enter');
        assert.equal((await state(id)).phase, runMode === 'jev' ? 'judging' : 'running');
        assert.equal((await state(id)).runMode, runMode);
        await settle(id, 'completed');
        await key(id, 'Control+x'); assert.equal((await state(id)).screen, 'list');
        await key(id, 'n');
        assert.equal((await state(id)).screen, 'session', 'new session also bypasses the startup chooser');
        assert.equal((await state(id)).entry, 'new');
        assert.equal((await state(id)).sessionCount, 2);
        assert.equal((await state(id)).runMode, runMode);
        await type(id, '새 세션 요청'); await key(id, 'Enter');
        assert.equal((await state(id)).phase, runMode === 'jev' ? 'judging' : 'running');
        await settle(id, 'completed');
        await key(id, 'Control+x'); await key(id, 'Enter');
        assert.equal((await state(id)).screen, 'session');
        assert.equal((await state(id)).runMode, runMode, 'reopening does not ask for process mode again');
      }
      pass(`idea ${id}: arrows; native/Jev once-per-run choice; existing/new, follow-up and reopening keep runMode`);
    }

    const sessionView = current => ({ screen: current.screen, entry: current.entry, runMode: current.runMode,
      current: current.current, phase: current.phase, draft: current.draft,
      turns: current.turns, sessionCount: current.sessionCount });
    await enterRun(1, 'jev'); await key(1, 'Enter');
    await type(1, '첫 작업'); await key(1, 'Enter');
    await type(1, '이어쓸 초안');
    // Freeze only this mock turn so timer advancement cannot race the reattach comparison.
    await settle(1, 'judging');
    const existingBefore = sessionView(await state(1));
    assert.equal(existingBefore.current, '첫 작업');
    assert.equal(existingBefore.draft, '이어쓸 초안');
    assert.equal(existingBefore.entry, 'existing'); assert.equal(existingBefore.sessionCount, 1);
    await key(1, 'Control+x'); assert.equal((await state(1)).screen, 'list');
    await key(1, 'Enter');
    assert.deepEqual(sessionView(await state(1)), existingBefore,
      'list Enter reattaches the busy existing session without resetting phase, prompt, draft or count');
    pass('regression: Jev existing-session reattach preserves current, busy phase, unsent draft and sessionCount 1');

    await enterRun(2, 'native'); await key(2, 'Enter');
    await type(2, '기존 작업'); await key(2, 'Enter');
    assert.equal((await state(2)).phase, 'running');
    await key(2, 'Control+x'); await key(2, 'n');
    const newSession = await state(2);
    assert.equal(newSession.entry, 'new'); assert.equal(newSession.phase, 'input');
    assert.equal(newSession.current, ''); assert.equal(newSession.sessionCount, 2);
    await type(2, '새 세션 초안');
    const newBefore = sessionView(await state(2));
    await page.waitForTimeout(1250); // The old native entry completes after 1000 ms, in the background.
    assert.deepEqual(sessionView(await state(2)), newBefore,
      'an existing-entry timer must not mutate the new entry phase, prompt or draft');
    await key(2, 'Control+x'); await key(2, 'Enter');
    const completedExisting = await state(2);
    assert.equal(completedExisting.screen, 'session'); assert.equal(completedExisting.entry, 'existing');
    assert.equal(completedExisting.phase, 'completed'); assert.equal(completedExisting.current, '기존 작업');
    assert.equal(completedExisting.sessionCount, 2, 'reopening an existing record does not create a third session');
    pass('regression: native timer completes its background entry without changing the new input view');

    await key(2, 'Control+x'); await key(2, 'n');
    assert.deepEqual(sessionView(await state(2)), newBefore, 'the separate new-entry draft survives switching records');
    await key(2, 'Control+x'); assert.equal((await state(2)).screen, 'list');
    await key(2, 'Control+x');
    assert.deepEqual(sessionView(await state(2)), newBefore,
      'Ctrl-X live reattach keeps the current new entry, draft and count without resetting');
    pass('regression: Ctrl-X live reattach preserves the new entry and unsent draft with sessionCount 2');

    for (const id of ids) {
      await reset(id); await key(id, 'ArrowRight'); await key(id, 'Enter');
      let current = await state(id);
      assert.equal(current.screen, 'consent'); assert.equal(current.runMode, null);
      await key(id, 'Enter');
      current = await state(id);
      assert.equal(current.screen, 'consent', 'no consent means no process mode'); assert.equal(current.runMode, null);
      await key(id, 'c'); assert.equal((await state(id)).consent, true);
      await key(id, 'c'); assert.equal((await state(id)).consent, false);
      await key(id, 'Escape');
      current = await state(id);
      assert.equal(current.screen, 'startup'); assert.equal(current.runMode, null); assert.equal(current.sessionCount, 0);
      for (const ready of ['missing', 'denied']) {
        await reset(id, { ready }); await key(id, 'ArrowRight'); await key(id, 'Enter');
        current = await state(id);
        assert.equal(current.screen, 'blocked', `idea ${id}/${ready}: Jev fails closed`);
        assert.equal(current.runMode, null); assert.equal(current.sessionCount, 0);
        await key(id, 'Escape');
        current = await state(id);
        assert.equal(current.screen, 'startup'); assert.equal(current.runMode, null, 'no automatic native fallback');
        await reset(id, { ready }); await key(id, 'Enter');
        current = await state(id);
        assert.equal(current.screen, 'list'); assert.equal(current.runMode, 'native', 'explicit native remains available');
      }
      for (const value of id === 10 ? ['Escape'] : ['Escape', 'q']) {
        await reset(id); await key(id, value);
        current = await state(id);
        assert.equal(current.screen, 'quit'); assert.equal(current.runMode, null); assert.equal(current.sessionCount, 0);
      }
    }
    pass('all 10: consent toggle/cancel, missing/denied block without fallback, explicit native, Esc quit; ideas 1–9 q quit');

    await reset(10);
    await key(10, 'q');
    let current = await state(10);
    assert.equal(current.query, 'q'); assert.equal(current.screen, 'startup', 'search q is text, not quit');
    assert.equal(current.runMode, null);
    await key(10, 'Escape'); assert.equal((await state(10)).screen, 'quit', 'Esc quits even with a search query');
    await reset(10);
    await type(10, 'nc한');
    current = await state(10);
    assert.equal(current.query, 'nc한'); assert.equal(current.screen, 'startup');
    assert.equal(current.runMode, null); assert.equal(current.consent, false, 'startup c is search text, not consent');
    await key(10, 'Backspace'); assert.equal((await state(10)).query, 'nc', 'Backspace removes one Hangul character');
    await key(10, 'Backspace'); await key(10, 'Backspace'); assert.equal((await state(10)).query, '');
    await type(10, 'zzzz'); await key(10, 'Enter');
    assert.equal((await state(10)).screen, 'startup', 'no matching command cannot start the process');
    assert.equal((await state(10)).runMode, null);
    for (let index = 0; index < 4; index++) await key(10, 'Backspace');
    await type(10, 'codex'); await key(10, 'ArrowDown');
    assert.equal((await state(10)).selected, 'jev', 'search arrows select only a matching route');
    await key(10, 'Enter'); assert.equal((await state(10)).screen, 'consent');
    await key(10, 'c'); assert.equal((await state(10)).consent, true);
    await key(10, 'Enter'); assert.equal((await state(10)).runMode, 'jev');
    await reset(10); await type(10, 'native'); await key(10, 'ArrowRight');
    assert.equal((await state(10)).selected, 'native');
    await key(10, 'Enter'); assert.equal((await state(10)).runMode, 'native');
    pass('idea 10: printable n/c/q search, Unicode Backspace, no-match block, filtered arrows and consent-screen-only c');

    await enterRun(1, 'jev'); await key(1, 'Enter');
    for (const value of ['input', 'judging', 'requested', 'running', 'completed', 'error', 'interrupted']) await settle(1, value);
    await type(1, '자동 상태 전이'); await key(1, 'Enter');
    for (const value of ['judging', 'requested', 'running', 'completed']) await phase(1, value);
    await enterRun(2, 'native'); await key(2, 'Enter');
    await type(2, '네이티브 자동 완료'); await key(2, 'Enter');
    assert.equal((await state(2)).phase, 'running'); await phase(2, 'completed');
    assert.equal((await state(2)).runMode, 'native');
    pass('fixture phases and real mock timers: Jev judging/requested/running/completed; native bypasses Jev');

    assert.equal(await page.locator('.journey-grid .journey').count(), 6, 'six common lifecycle frames');
    const layout = page.locator('.pane-layout-preview[data-layout="4+3"]');
    assert.equal(await layout.count(), 1);
    assert.equal(await layout.locator('.pane-row.top > *').count(), 4);
    assert.equal(await layout.locator('.pane-row.bottom > *').count(), 3);
    const pane = await layout.locator('.pane-row.top > *').first().boundingBox();
    const footer = await layout.locator('.layout-footer').boundingBox();
    assert.ok(pane && footer && footer.width > pane.width * 3.5, 'shared full-width usage footer');
    pass('static common journey and unchanged 4+3 pane context');

    await page.locator('#reset').click();
    for (const id of ids) {
      const current = await state(id);
      for (const [field, value] of Object.entries(defaults)) assert.deepEqual(current[field], value, `reset link idea ${id}: ${field}`);
    }
    pass('reset link returns all 10 ideas to unselected-run startup defaults');
    for (const width of receipt.viewports) {
      await page.setViewportSize({ width, height: 1050 }); await page.evaluate(() => scrollTo(0, 0));
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true,
        `${width}px: no page-level horizontal overflow; internal terminal scrolling is allowed`);
      for (const id of ids) {
        const bounds = await terminal(id).boundingBox();
        assert.ok(bounds && bounds.width > 0 && bounds.height > 0, `${width}px idea ${id}: terminal remains visible`);
      }
      await screenshot(`gallery-${width}`);
    }
    await page.setViewportSize({ width: 1440, height: 1050 });
    for (const id of ids) await screenshot(`idea-${String(id).padStart(2, '0')}`, article(id));
    assert.deepEqual(receipt.externalRequests, [], 'zero external requests');
    assert.deepEqual(receipt.browserErrors, [], 'zero browser/console errors');
    assert.equal(fs.readFileSync(htmlPath, 'utf8'), source, 'source stayed frozen during verification');
    pass('1440/1280/390 responsive captures, 10 individual ideas, zero external calls or browser errors');
    receipt.result = 'passed';
  } catch (error) {
    receipt.failure = error.stack || String(error); throw error;
  } finally {
    try { if (browser) await browser.close(); }
    finally {
      if (server.listening) await new Promise(resolve => server.close(resolve));
      receipt.elapsedMs = Date.now() - started;
      receipt.receiptPath = receiptPath;
      fs.writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`);
      console.log(JSON.stringify(receipt, null, 2));
    }
  }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
