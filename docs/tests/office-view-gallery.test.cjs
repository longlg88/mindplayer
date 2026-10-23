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

async function main() {
  const source = fs.readFileSync(file, 'utf8');
  const before = hash(source);
  const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-office-qa-'));
  const result = { scope: 'offline standalone HTML synthetic interactions, not native runtime proof',
    verifiedAt: new Date().toISOString(), before, after: null, result: 'failed', passed: [], failed: [], screenshots: [], geometry: [],
    keyboardEvents: 0, scenes: 0, syntheticSessions: 0, externalRequests: 0, pageErrors: 0, consoleErrors: 0, providerCalls: 0, cleanup: { browser: false, server: false } };
  let browser, server;
  try {
    for (const m of source.matchAll(/<script>([\s\S]*?)<\/script>/g)) new vm.Script(m[1]);
    assert.doesNotMatch(source, /\b(?:fetch|XMLHttpRequest|WebSocket|localStorage|sessionStorage)\s*[.(]/);
    assert.doesNotMatch(source, /https?:\/\//);
    assert.doesNotMatch(source, /Vision|Ultron|Marvel|비전 캐릭터|울트론/, 'no franchise names or characters in the page');
    result.passed.push('inline syntax, no network/storage APIs, no external URLs, no franchise names');
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
      await page.waitForTimeout(300);
      const office = (expr, arg) => page.evaluate(expr, arg);
      await office(() => window.__office2.pause(true));
      const choose = async k => { await office(k => { window.__office2.selectScene(k); window.__office2.settle(); }, k); await page.waitForTimeout(120); };
      const sim = () => office(() => window.__office2.sessions.map(s => ({ title: s.title, status: s.status, agent: s.agent, atDesk: s.atDesk, gone: s.gone, walking: s.walking, pod: !!s.pod, hover: s.hover })));
      const press = async (key, code) => { await page.locator('#stage').focus(); if (code) await page.locator('#stage').dispatchEvent('keydown', { key, code, bubbles: true }); else await page.keyboard.press(key); result.keyboardEvents++; };

      if (motion === 'no-preference') {
        result.scenes = await page.locator('.tab').count(); assert.equal(result.scenes, 5);
        result.syntheticSessions = (await sim()).length; assert.equal(result.syntheticSessions, 24);
        const frames = new Set();
        for (let k = 0; k < 5; k++) {
          await choose(k);
          assert.deepEqual(await office(() => window.__office2.validateWorld()), [], `scene ${k + 1} layout contract`);
          frames.add(hash(await page.evaluate(() => document.getElementById('scene').toDataURL())));
          const shot = path.join(artifacts, `scene${k + 1}.png`); await page.locator('#stage').screenshot({ path: shot }); result.screenshots.push(shot);
        }
        assert.equal(frames.size, 5); result.passed.push('five distinct scenes; every seat aisle, room spot, exit and desk path is walkable and no two standing AIs share a screen box');

        await choose(0);
        const control = await office(() => { const w = window.__office2, free = w.sessions.filter(s => !s.atDesk && !s.pod); const saved = {...free[1].pos}; free[1].pos = {...free[0].pos}; const bad = w.validateWorld(); free[1].pos = saved; return bad; });
        assert.ok(control.some(b => b.startsWith('crowd')), 'crowding check reports two AIs placed on one spot');
        result.passed.push('control: the crowding check fails when two standing AIs share a spot');

        const order = await office(() => window.__office2.rollup().map(s => s.status));
        assert.deepEqual(order, [...order].sort((a, b) => (a === 'done' ? 0 : 1) - (b === 'done' ? 0 : 1)));
        const next = await office(() => window.__office2.nextSession().title);
        await press('n'); assert.match(await page.locator('#card').textContent(), new RegExp(next));
        assert.match(await page.locator('.q.next').textContent(), new RegExp(next));
        assert.equal(await page.locator('#card button').count(), 1, 'card has only a close button: no reply, stop or key buttons');
        assert.match(await page.locator('#card').textContent(), /읽기 전용/);
        await press('Escape'); assert.equal(await page.locator('#card.open').count(), 0);
        await press('ㅜ', 'KeyN'); assert.match(await page.locator('#card').textContent(), new RegExp(next), 'Korean layout ㅜ on the N key opens the next card');
        await press('Escape');
        result.passed.push('N and the queue chip point at the same next session (done first, then blocked); the card is read-only; ㅜ works as N');

        await office(() => window.__office2.openCard(window.__office2.sessions.find(s => s.agent === 'cursor')));
        assert.match(await page.locator('#card').textContent(), /대화 기록을 모으지 않습니다/);
        await office(() => window.__office2.openCard(window.__office2.sessions.find(s => s.agent === 'kiro')));
        assert.match(await page.locator('#card').textContent(), /convo_log/);
        await office(() => window.__office2.closeCard());
        result.passed.push('card shows convo_log replies for claude/codex/kiro and states that cursor has none');

        await office(() => window.__office2.settle());
        const pt = await office(() => window.__office2.stagePoint(window.__office2.sessions.find(s => s.status === 'blocked' && s.atDesk)));
        const box = await page.locator('#scene').boundingBox();
        await page.mouse.move(box.x + pt.x, box.y + pt.y);
        assert.ok((await sim()).some(s => s.hover && s.status === 'blocked'), 'hovering a waiting AI marks it');
        assert.equal(await page.locator('#tip').isVisible(), true);
        await page.mouse.move(box.x + 5, box.y + box.height - 5);
        result.passed.push('hover marks the AI under the pointer and shows its tooltip');

        await page.locator('#stage').focus(); await page.keyboard.press('Tab');
        assert.notEqual(await page.evaluate(() => document.activeElement.id), 'stage');
        result.passed.push('Tab leaves the stage');
      }

      for (let k = 0; k < 5; k++) {
        await choose(k);
        const idleBefore = (await sim()).filter(s => s.status === 'idle' && s.pod).length;
        await page.locator('#b-start').click();
        await office(() => window.__office2.advance(30));
        await page.locator('#b-end').click();
        await office(() => window.__office2.advance(30));
        const after = await sim();
        assert.equal(after.filter(s => s.status === 'idle' && s.pod).length, idleBefore - 1, `scene ${k + 1}: one AI left its seat`);
        const started = after.find(s => s.status === 'working' && !s.walking && s.atDesk && s.title === (after.find(x => x.status === 'working' && x.atDesk) || {}).title);
        assert.ok(started, `scene ${k + 1}: an AI is working at a desk`);
        assert.ok(after.some(s => s.status === 'ended' && s.gone), `scene ${k + 1}: the ended AI left through the exit`);
        assert.ok(!after.some(s => s.walking), `scene ${k + 1}: nobody is stuck walking`);
        assert.deepEqual(await office(() => window.__office2.validateWorld()), [], `scene ${k + 1}: layout still valid after transitions`);
        if (motion === 'no-preference' && k === 1) { const shot = path.join(artifacts, 'scene2-after-transitions.png'); await page.locator('#stage').screenshot({ path: shot }); result.screenshots.push(shot); }
      }
      result.passed.push(`${motion}: in all five scenes an idle AI walks to its console and a done AI leaves through the exit`);

      if (motion === 'no-preference') for (const width of [1440, 1280, 768, 390]) {
        await page.setViewportSize({ width, height: 900 }); await page.waitForTimeout(150);
        const m = await page.evaluate(() => ({ viewport: innerWidth, page: document.documentElement.scrollWidth, sheet: document.getElementById('sheet').clientHeight, stage: document.getElementById('stage').clientHeight }));
        result.geometry.push(m); assert.ok(m.page <= width, JSON.stringify(m)); assert.ok(m.sheet > 200 && m.stage > 400, JSON.stringify(m));
        if (width === 390) { const shot = path.join(artifacts, 'page-390px.png'); await page.screenshot({ path: shot, fullPage: true }); result.screenshots.push(shot); }
        result.passed.push(`viewport ${width}: no document overflow, sheet and stage keep their size`);
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
        result: result.result, passedContracts: result.passed.length, checked: result.passed, keyboardEvents: result.keyboardEvents, scenes: result.scenes,
        syntheticSessions: result.syntheticSessions, motionModes: ['no-preference', 'reduce'], viewportWidths: result.geometry.map(g => g.viewport),
        documentOverflow: Math.max(...result.geometry.map(g => g.page - g.viewport)), externalRequests: result.externalRequests,
        pageErrors: result.pageErrors, consoleErrors: result.consoleErrors, providerCalls: result.providerCalls,
        sourceSha256Before: result.before, sourceSha256After: result.after, cleanup: result.cleanup,
        diagnosticReceipt: `<os-tmpdir>/${path.basename(artifacts)}/validation.json`,
        notVerified: ['A live MindPlayer state feed (the read-only 127.0.0.1 GET is a plan)', 'Collecting the last tool for every session',
          'Rendering cost with real session counts above 24', 'Screen-reader behaviour of the canvas scene', 'The key that opens this window from MindPlayer'] };
      fs.writeFileSync(record, JSON.stringify(summary, null, 2) + '\n');
    }
    console.log(JSON.stringify({ result: result.result, passed: result.passed.length, failed: result.failed, keyboardEvents: result.keyboardEvents,
      externalRequests: result.externalRequests, pageErrors: result.pageErrors, consoleErrors: result.consoleErrors, providerCalls: result.providerCalls,
      before: result.before, after: result.after, cleanup: result.cleanup, receipt: path.join(artifacts, 'validation.json') }));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
