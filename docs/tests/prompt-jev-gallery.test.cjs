'use strict';
// Offline HTML interaction checks only. Never starts MindPlayer/CLI/providers.
// MINDPLAYER_PLAYWRIGHT_MODULE=/existing/playwright node docs/tests/prompt-jev-gallery.test.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');
const crypto = require('node:crypto');
const file = path.resolve(__dirname, '../plans/2026-10-19-prompt-jev-gallery.ko.html');
const hash = text => crypto.createHash('sha256').update(text).digest('hex');

async function main() {
  const source = fs.readFileSync(file, 'utf8');
  const before = hash(source);
  const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-prompt-jev-qa-'));
  const result = { scope: 'offline standalone HTML synthetic interactions, not native runtime proof',
    verifiedAt: new Date().toISOString(), source: file, before, after: null, result: 'failed',
    passed: [], failed: [], screenshots: [], geometry: [], keyboardEvents: 0,
    externalRequests: 0, pageErrors: 0, consoleErrors: 0, providerCalls: 0,
    artifacts, cleanup: { browser: false, server: false } };
  let browser, server;
  try {
    for (const m of source.matchAll(/<script>([\s\S]*?)<\/script>/g)) new vm.Script(m[1]);
    assert.doesNotMatch(source, /\b(?:fetch|XMLHttpRequest|WebSocket|localStorage|sessionStorage)\s*[.(]/);
    assert.doesNotMatch(source, /defaultMode/);
    result.passed.push('inline syntax and no network/storage APIs');
    const { chromium } = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');
    server = http.createServer((req, res) => {
      if (req.url === '/favicon.ico') { res.writeHead(204).end(); return; }
      if (req.url !== '/') { res.writeHead(404).end(); return; }
      res.setHeader('Content-Type', 'text/html; charset=utf-8'); res.end(source);
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const origin = `http://127.0.0.1:${server.address().port}`;
    browser = await chromium.launch({ channel: 'chrome', headless: true });
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, reducedMotion: 'reduce' });
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin !== origin) { result.externalRequests++; await route.abort(); }
      else await route.continue();
    });
    const page = await context.newPage();
    page.on('pageerror', () => result.pageErrors++);
    page.on('console', msg => { if (msg.type() === 'error') result.consoleErrors++; });
    await page.goto(origin);
    const get = () => page.evaluate(() => gallery.state());
    const press = async key => { await page.locator('#proposed').focus(); await page.keyboard.press(key); result.keyboardEvents++; };
    const wait = phase => page.waitForFunction(value => gallery.state().phase === value, phase);
    const choose = async (id, session = 'a') => {
      await page.locator(`nav button[data-idea="${id}"]`).click();
      await page.locator(`.sessions button[data-session="${session}"]`).click();
      await page.locator('[data-action="reset"]').click();
    };
    const sendJev = async id => {
      const before = (await get()).dispatches;
      if (id === 2) await press('F7');
      else if (id === 3) {
        if (!(await get()).popup) await press('Enter');
        await press('ArrowDown'); assert.equal((await get()).dispatches, before);
        await press('Enter');
      } else { await press(id === 5 ? 'F7' : 'F6'); assert.equal((await get()).dispatches, before); await press('Enter'); }
    };
    const sendNative = async id => {
      const before = (await get()).dispatches;
      if (id === 2) await press('F8');
      else if (id === 3) { if (!(await get()).popup) await press('Enter'); await press('ArrowUp'); await press('Enter'); }
      else { if (id === 5) await press('F8'); else { await press('F6'); await press('F6'); } assert.equal((await get()).dispatches, before); await press('Enter'); }
    };
    assert.deepEqual(await page.evaluate(() => gallery.counts()), { proposals: 5, panes: 7 });
    assert.equal(await page.locator('nav [data-idea]').count(), 5);
    assert.equal(await page.locator('.pane-row.top .mini-pane').count(), 4);
    assert.equal(await page.locator('.pane-row.bottom .mini-pane').count(), 3);
    assert.equal(await page.locator('details[open]').count(), 0);
    assert.equal(await page.locator('#proposed').getAttribute('aria-pressed'), null);
    result.passed.push('exact five proposals, 4+3 panes and closed explanatory disclosure');

    await choose(5);
    await press('Enter');
    assert.equal((await get()).dispatches, 0, 'Every prompt must wait for an explicit use/nonuse choice; no session default');
    result.passed.push('input-start proposal has no implicit session default');

    const unique = new Set();
    for (let id = 1; id <= 5; id++) {
      await choose(id);
      unique.add(await page.locator('#proposed').textContent());
      if (id === 3) {
        assert.doesNotMatch(await page.locator('#proposed').textContent(), /Enter로 전송 방식 선택/);
        assert.match(await page.locator('#proposed .line').nth(17).textContent(), /^│ {12}│ {62}│ {10}│$/);
      }
      const shot = path.join(artifacts, `${id}-desktop.png`);
      await page.screenshot({ path: shot, fullPage: true }); result.screenshots.push(shot);
      assert.equal((await get()).dispatches, 0);
    }
    assert.equal(unique.size, 5); result.passed.push('five visually distinct initial terminal states');

    for (const session of ['a', 'b']) for (let id = 1; id <= 5; id++) {
      await choose(id, session);
      const original = await get();
      assert.equal(original.mode, null);
      assert.equal(original.prompt, 1);
      await press('Enter');
      if (id === 3) await press('Enter');
      assert.equal((await get()).dispatches, 0);
      await sendJev(id);
      assert.equal((await get()).phase, 'judging');
      assert.equal((await get()).nativeModel, original.nativeModel);
      await wait('accepted');
      assert.ok((await get()).selection); assert.equal((await get()).nativeModel, original.nativeModel);
      await wait('running'); const selected = await get();
      assert.equal(selected.nativeModel, selected.selection.model);
      assert.equal(selected.nativeEffort, selected.selection.effort);
      await wait('completed');
      const completed = await get(); assert.equal(completed.mode, null); assert.equal(completed.draft, '');
      await press('x');
      const second = await get();
      assert.equal(second.mode, null); assert.equal(second.prompt, 2); assert.equal(second.dispatches, 1);
      assert.equal(second.phase, 'input');
      if (id === 5) { await press('F8'); await press('x'); await press('F6'); }
      await press('Enter');
      if (id === 3) await press('Enter');
      assert.equal((await get()).dispatches, 1, 'Second typed prompt cannot inherit the first Jev choice');
      if (session === 'a') { const shot = path.join(artifacts, `${id}-second-prompt.png`); await page.screenshot({ path: shot, fullPage: true }); result.screenshots.push(shot); }
      await sendNative(id);
      const native = await get(); assert.equal(native.phase, 'running'); assert.equal(native.sentMode, 'native');
      assert.equal(native.nativeModel, selected.nativeModel); assert.equal(native.nativeEffort, selected.nativeEffort);
      await wait('completed'); assert.equal((await get()).dispatches, 2);
      await press('y');
      assert.equal((await get()).mode, null); assert.equal((await get()).prompt, 3);
      if (id === 5) { await press('F7'); await press('y'); await press('F6'); }
      await press('Enter');
      if (id === 3) await press('Enter');
      assert.equal((await get()).dispatches, 2, 'Third typed prompt cannot inherit the second native choice');
      await sendJev(id); await wait('completed');
      assert.equal((await get()).dispatches, 3);
      result.passed.push(`idea ${id} session ${session}: three typed prompts Jev/native/Jev, fresh choice EVERY time, native keeps last model+effort`);
    }

    for (let id = 1; id <= 5; id++) {
      await choose(id); const initial = await get();
      await page.locator('[data-action="failure"]').click();
      await sendJev(id); await wait('error');
      const failed = await get();
      assert.equal(failed.dispatches, 1); assert.equal(failed.selection, null);
      assert.equal(failed.nativeModel, initial.nativeModel); assert.equal(failed.nativeEffort, initial.nativeEffort);
      assert.equal(failed.draft, initial.draft);
      assert.match(await page.locator('#proposed').textContent(), /모델 선택 없음/);
      await page.waitForTimeout(250); assert.equal((await get()).dispatches, 1);
      result.passed.push(`idea ${id}: failed judgment preserves draft/config, no silent native fallback`);
    }

    for (const id of [1, 3]) {
      await choose(id); const initial = await get(); await sendJev(id); await press('Escape');
      const cancelled = await get(); assert.equal(cancelled.phase, 'cancelled'); assert.equal(cancelled.draft, initial.draft);
      await page.waitForTimeout(2150); assert.equal((await get()).phase, 'cancelled');
      assert.equal((await get()).nativeModel, initial.nativeModel);
      result.passed.push(`idea ${id}: cancellation blocks late simulated callbacks`);
    }
    await choose(1); await press('Enter'); assert.equal((await get()).dispatches, 0);
    await choose(2); await press('Enter'); assert.equal((await get()).dispatches, 0);
    await choose(3); const draft = (await get()).draft; await press('Escape');
    assert.equal((await get()).popup, false); assert.equal((await get()).draft, draft); assert.equal((await get()).dispatches, 0);
    result.passed.push('unselected and modal cancellation never dispatch');

    await choose(4); await press('F6'); await press('F7'); assert.equal((await get()).includeRecent, false);
    assert.match(await page.locator('#proposed').textContent(), /참조 부족/); assert.equal((await get()).dispatches, 0);
    await choose(5); const heldDraft = (await get()).draft;
    await press('F7'); assert.equal((await get()).mode, 'jev'); assert.equal((await get()).dispatches, 0);
    await page.locator('.sessions button[data-session="b"]').click();
    await page.locator('[data-action="reset"]').click(); assert.equal((await get()).mode, null);
    await page.locator('.sessions button[data-session="a"]').click(); assert.equal((await get()).mode, 'jev');
    await press('F6'); assert.equal((await get()).mode, null); assert.equal((await get()).draft, heldDraft);
    result.passed.push('context omission warning, drafts isolated across sessions, input-start reselect preserves draft');

    for (const kind of ['paste', 'compositionend']) {
      await choose(1); await sendNative(1); await wait('completed');
      await page.locator('#proposed').evaluate((el, type) => {
        if (type === 'compositionend') el.dispatchEvent(new CompositionEvent(type, { bubbles: true, data: '후속 질문' }));
        else { const data = new DataTransfer(); data.setData('text/plain', '후속 질문'); el.dispatchEvent(new ClipboardEvent(type, { bubbles: true, cancelable: true, clipboardData: data })); }
      }, kind);
      assert.equal((await get()).mode, null); assert.equal((await get()).prompt, 2);
      assert.equal((await get()).draft, '후속 질문');
      await press('Enter'); assert.equal((await get()).dispatches, 1);
      result.passed.push(`synthetic ${kind} handler starts fresh prompt without inheriting prior choice`);
    }
    await choose(5); await press('F7'); await page.locator('[data-action="followup"]').click();
    assert.equal((await get()).mode, null); assert.equal((await get()).prompt, 2);
    await press('Enter'); assert.equal((await get()).dispatches, 0);
    result.passed.push('follow-up document control also resets mode without session defaults');

    for (const width of [1440, 1280, 768, 390, 320]) {
      await page.setViewportSize({ width, height: 900 }); await choose(4);
      const measured = await page.evaluate(() => {
        const el = document.querySelector('.proposed .terminal-scroll');
        const keys = document.querySelector('#key-hint').getBoundingClientRect();
        return { viewport: innerWidth, page: document.documentElement.scrollWidth,
          terminalClient: el.clientWidth, terminalScroll: el.scrollWidth, keysWidth: keys.width };
      });
      result.geometry.push(measured); assert.ok(measured.page <= width, JSON.stringify(measured));
      assert.ok(measured.terminalClient > 0); assert.ok(measured.keysWidth <= width);
      if (width <= 390) assert.ok(measured.terminalScroll > measured.terminalClient, JSON.stringify(measured));
      if (measured.terminalScroll > measured.terminalClient) { await page.locator('.proposed .terminal-scroll').evaluate(el => el.scrollLeft = 120); assert.ok(await page.locator('.proposed .terminal-scroll').evaluate(el => el.scrollLeft > 0)); }
      if (width === 390 || width === 1280) { const shot = path.join(artifacts, `4-${width}px.png`); await page.screenshot({ path: shot, fullPage: true }); result.screenshots.push(shot); }
      result.passed.push(`viewport ${width}: no document overflow, fixed terminal cell surface remains scrollable`);
    }
    await page.setViewportSize({ width: 1440, height: 1000 }); await choose(1);
    await page.locator('[data-action="focus"]').click(); await press('Tab');
    assert.notEqual(await page.evaluate(() => document.activeElement.id), 'proposed');
    result.passed.push('Tab exits prototype keyboard surface');
    assert.equal(result.externalRequests, 0); assert.equal(result.pageErrors, 0); assert.equal(result.consoleErrors, 0);
    assert.equal(hash(fs.readFileSync(file)), before); result.after = before; result.result = 'passed';
  } catch (error) {
    result.failed.push({ name: error.name, message: error.message }); throw error;
  } finally {
    if (browser) { await browser.close(); result.cleanup.browser = true; }
    if (server) { await new Promise(resolve => server.close(resolve)); result.cleanup.server = true; }
    // Task-owned diagnostic artifacts only, retained for review.
    fs.writeFileSync(path.join(artifacts, 'validation.json'), JSON.stringify(result, null, 2) + '\n');
    console.log(JSON.stringify({ result: result.result, passed: result.passed.length, failed: result.failed,
      keyboardEvents: result.keyboardEvents, externalRequests: result.externalRequests,
      pageErrors: result.pageErrors, consoleErrors: result.consoleErrors, providerCalls: result.providerCalls,
      before: result.before, after: result.after, cleanup: result.cleanup, receipt: path.join(artifacts, 'validation.json') }));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
