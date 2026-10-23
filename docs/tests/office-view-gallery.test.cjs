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
    // names of the referenced game and franchise, kept out of this public repo as sha256 prefixes of the lower-cased word
    const banned = new Set(['652ccbed27d3820b', '3da82e606d9fc4cd', 'a1a38b5bae5866d7', 'f010e37626b41704', '5a52204403899c2b', '90799fdcfdd3a1e3', '92726c47e1ed0ad5', 'e09ba57969cc21f0', '6518d70d64a1a20d', '874b24615791f65d',
      'ad9454ef8287c81f', '6d9a3440020a9e07', '5944ae849448011c', '9318b7d37aff5ca9', '38868dc905f4d6d6', '40722801a3b84d5c', '51fb5adcf0573798', '2e773265de9d3308', 'ae8a6466de8b6f77', '516c267ca186892b']);
    const words = [...new Set((source.match(/[A-Za-z]+|[가-힣]+/g) || []).map(w => w.toLowerCase()))];
    assert.deepEqual(words.filter(w => banned.has(hash(w).slice(0, 16))).map(w => `${w.length} chars`), [], 'no game or franchise names in the page');
    result.passed.push('inline syntax, no network/storage APIs, no external URLs, no game or franchise names (hash-compared)');
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
      assert.equal(await office(() => window.__office2.blinkAt(0, 0)), motion !== 'reduce', 'eyes never close when motion is reduced');
      const choose = async k => { await office(k => { window.__office2.selectScene(k); window.__office2.settle(); }, k); await page.waitForTimeout(120); };
      const sim = () => office(() => window.__office2.sessions.map(s => ({ title: s.title, status: s.status, agent: s.agent, place: s.place, tool: s.tool, gone: s.gone, walking: s.walking, pod: !!s.pod, hover: s.hover })));
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
        assert.equal(frames.size, 5); result.passed.push('five distinct scenes; every station spot and the terminal step is walkable, everyone reaches a bench and the terminal, and no two standing companions share a screen box');

        await choose(0);
        const control = await office(() => { const w = window.__office2, free = w.sessions.filter(s => !s.pod); const saved = {...free[1].pos}; free[1].pos = {...free[0].pos}; const bad = w.validateWorld(); free[1].pos = saved; return bad; });
        assert.ok(control.some(b => b.startsWith('crowd')), 'crowding check reports two companions placed on one spot');
        const fence = await office(() => { const w = window.__office2, k = Math.floor(w.world.exitStep.i) + ',' + Math.floor(w.world.exitStep.j); w.world.blocked.add(k); const bad = w.validateWorld(); w.world.blocked.delete(k); return bad; });
        assert.ok(fence.includes('terminal step') && fence.some(b => b.startsWith('no path to terminal')), 'reachability check reports a fenced-off terminal');
        assert.deepEqual(await office(() => window.__office2.validateWorld()), [], 'controls restore a valid layout');
        result.passed.push('controls: the crowding check fails when two standing companions share a spot; the path check fails when the terminal step is fenced off');

        const rows = await page.locator('#baseinfo .bi-row').count(); assert.equal(rows, 24, 'base info lists every live companion');
        const states = await sim();
        for (const [status, text] of [['working', /열심히 일하는 중/], ['blocked', /지시를 기다립니다/], ['done', /납품 완료/]]) {
          const k = states.findIndex(s => s.status === status); assert.match(await page.locator('#baseinfo .bi-row').nth(k).textContent(), text, `${status} row sentence`);
        }
        const sleeper = states.findIndex(s => s.pod); assert.match(await page.locator('#baseinfo .bi-row').nth(sleeper).textContent(), /짚 침대에서 자는 중/);
        await page.locator('#baseinfo .bi-row').nth(1).click(); assert.equal(await page.locator('#card.open').count(), 1); assert.match(await page.locator('#card h3').textContent(), new RegExp(states[1].title));
        await office(() => window.__office2.closeCard());
        assert.ok(states.filter(s => s.agent === 'cursor').every(s => s.tool === null), 'cursor sessions carry no tool');
        for (const s of states.filter(x => x.status === 'working')) assert.equal(s.place, s.tool ? {read:'field', shell:'quarry', mcp:'depot', edit:'bench'}[s.tool] : 'bench', `${s.title} stands at the station of its last tool`);
        result.passed.push('base info lists 24 companions with one status sentence each and opens the card; working companions stand at the station of their last tool; cursor has none');

        const order = await office(() => window.__office2.rollup().map(s => s.status));
        assert.deepEqual(order, [...order].sort((a, b) => (a === 'done' ? 0 : 1) - (b === 'done' ? 0 : 1)));
        const q = await office(() => window.__office2.rollup().map(s => s.title));
        const chip = async () => (await page.locator('.q.next').textContent()).includes(await office(() => window.__office2.nextSession().title));
        assert.ok(await chip(), 'queue chip shows the N target before any card is open');
        await press('n'); assert.equal(await page.locator('#card.open').count(), 1); assert.match(await page.locator('#card h3').textContent(), new RegExp(q[0]));
        assert.ok(await chip(), 'queue chip follows the N target after the first press');
        await press('n'); assert.match(await page.locator('#card h3').textContent(), new RegExp(q[1]), 'second N moves to the next session');
        assert.equal(await page.locator('#card button').count(), 1, 'card has only a close button: no reply, stop or key buttons');
        assert.match(await page.locator('#card').textContent(), /읽기 전용/);
        await press('Escape'); assert.equal(await page.locator('#card.open').count(), 0);
        await press('ㅜ', 'KeyN'); assert.equal(await page.locator('#card.open').count(), 1, 'Korean layout ㅜ on the N key opens a card');
        assert.match(await page.locator('#card h3').textContent(), new RegExp(q[0]));
        await press('Escape');
        result.passed.push('N walks the queue in status_rank order and the first chip always names the N target; the card is read-only; ㅜ works as N');

        await office(() => window.__office2.openCard(window.__office2.sessions.find(s => s.agent === 'cursor')));
        assert.match(await page.locator('#card .reply').textContent(), /convo_log에 대화 turn이 없습니다/);
        const srBefore = await page.locator('#sr').textContent(); await office(() => window.__office2.closeCard()); assert.equal(await page.locator('#sr').textContent(), srBefore, 'closing a card leaves the live region text unchanged');
        const kiroReply = await office(() => { const s = window.__office2.sessions.find(x => x.agent === 'kiro'); window.__office2.openCard(s); return s.reply; });
        assert.equal((await page.locator('#card .reply').textContent()).trim(), kiroReply.trim(), 'kiro card shows its own last reply');
        await office(() => window.__office2.closeCard());
        result.passed.push('card shows convo_log replies for claude/codex/kiro and states that cursor has none');

        await office(() => window.__office2.setCam(window.__office2.fitTarget()));
        const pt = await office(() => window.__office2.stagePoint(window.__office2.sessions.find(s => s.status === 'blocked' && !s.walking)));
        await page.locator('#stage').scrollIntoViewIfNeeded(); const box = await page.locator('#scene').boundingBox();
        await page.mouse.move(box.x + pt.x, box.y + pt.y);
        assert.ok((await sim()).some(s => s.hover && s.status === 'blocked'), 'hovering a waiting AI marks it');
        assert.equal(await page.locator('#tip').isVisible(), true);
        await page.mouse.move(box.x + 5, box.y + box.height - 5);
        result.passed.push('hover marks the AI under the pointer and shows its tooltip');

        const target = await office(() => window.__office2.fitTarget());
        await office(() => window.__office2.closeCard()); await office(c => window.__office2.setCam({...c, y:c.y + 200}), target); await press('0'); await office(() => window.__office2.easeFrames(240));
        const cam = await office(() => window.__office2.cam);
        assert.ok(Math.abs(cam.x - target.x) < 1 && Math.abs(cam.y - target.y) < 1 && Math.abs(cam.z - target.z) < .01, `0 returns the camera on x, y and zoom ${JSON.stringify({cam, target})}`);
        result.passed.push('after a vertical pan, 0 brings the camera back on x, y and zoom');

        await page.locator('#stage').focus(); await page.keyboard.press('Tab');
        assert.notEqual(await page.evaluate(() => document.activeElement.id), 'stage');
        result.passed.push('Tab leaves the stage');
      }

      for (let k = 0; k < 5; k++) {
        await choose(k);
        const idleBefore = (await sim()).filter(s => s.status === 'idle').length;
        const starter = (await sim()).find(s => s.status === 'idle' && !s.walking && !s.gone), leaver = (await sim()).find(s => s.status === 'done' && !s.gone).title;
        await office(s => window.__office2.openCard(window.__office2.sessions.find(x => x.title === s)), leaver);
        await page.locator('#b-start').click();
        await office(() => window.__office2.advance(30));
        await page.locator('#b-end').click();
        await office(() => window.__office2.advance(30));
        const after = await sim();
        assert.equal(after.filter(s => s.status === 'idle').length, idleBefore - 1, `scene ${k + 1}: one companion stopped resting`);
        const st = after.find(s => s.title === starter.title), lv = after.find(s => s.title === leaver);
        assert.ok(st.status === 'working' && st.place === 'bench' && !st.pod && !st.walking, `scene ${k + 1}: "${starter.title}" walked from ${starter.pod ? 'its bed' : 'the feed box'} to a bench`);
        assert.ok(lv.status === 'ended' && lv.gone, `scene ${k + 1}: "${leaver}" returned into the terminal`);
        assert.equal(await page.locator('#card.open').count(), 0, `scene ${k + 1}: the card of the departed companion closed`);
        assert.ok(!after.some(s => s.walking), `scene ${k + 1}: nobody is stuck walking`);
        assert.deepEqual(await office(() => window.__office2.validateWorld()), [], `scene ${k + 1}: layout still valid after transitions`);
        assert.equal(await page.locator('#baseinfo .bi-row').count(), 23, `scene ${k + 1}: base info drops the departed companion`);
        if (motion === 'no-preference' && k === 1) { const shot = path.join(artifacts, 'scene2-after-transitions.png'); await page.locator('#stage').screenshot({ path: shot }); result.screenshots.push(shot); }
      }
      result.passed.push(`${motion}: in all five scenes a resting companion walks to a bench and a done companion returns into the terminal`);

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
        notVerified: ['A live MindPlayer state feed (the read-only 127.0.0.1 GET is a plan)', 'Collecting the last tool for every session (codex needs a parser extension)',
          'Rendering cost with real session counts above 24', 'Screen-reader behaviour of the canvas scene', 'The key that opens this window from MindPlayer'] };
      fs.writeFileSync(record, JSON.stringify(summary, null, 2) + '\n');
    }
    console.log(JSON.stringify({ result: result.result, passed: result.passed.length, failed: result.failed, keyboardEvents: result.keyboardEvents,
      externalRequests: result.externalRequests, pageErrors: result.pageErrors, consoleErrors: result.consoleErrors, providerCalls: result.providerCalls,
      before: result.before, after: result.after, cleanup: result.cleanup, receipt: path.join(artifacts, 'validation.json') }));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
