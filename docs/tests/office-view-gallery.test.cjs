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
    const design = fs.readFileSync(path.join(repo, 'DESIGN.md'), 'utf8');
    const words = [...new Set(((source + '\n' + design).match(/[A-Za-z]+|[가-힣]+/g) || []).map(w => w.toLowerCase()))];
    assert.deepEqual(words.filter(w => banned.has(hash(w).slice(0, 16))).map(w => `${w.length} chars`), [], 'no game or franchise names in the page or DESIGN.md');
    result.passed.push('inline syntax, no network/storage APIs, no external URLs, no game or franchise names in the page or DESIGN.md (hash-compared)');
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
      const sim = () => office(() => window.__office2.sessions.map(s => ({ title: s.title, status: s.status, agent: s.agent, place: s.place, tool: s.tool, gone: s.gone, walking: s.walking, pod: !!s.pod, hover: s.hover, wild: window.__office2.isWild(s) })));
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
        const states = await sim(), rowText = await page.locator('#baseinfo .bi-row').allTextContents();
        states.forEach((s, k) => { assert.ok(rowText[k].includes(s.title), `row ${k} names ${s.title}`); const want = s.wild ? /야생 · category 없음/ : s.status === 'working' ? /일하는 중 \(/ : s.status === 'blocked' ? /지시를 기다립니다/ : s.status === 'done' ? /납품 완료/ : s.pod ? /짚 침대에서 자는 중/ : /먹이통에서 쉬는 중/; assert.match(rowText[k], want, `row ${k} sentence for ${s.status}`); });
        await page.locator('#baseinfo .bi-row').nth(1).click(); assert.equal(await page.locator('#card.open').count(), 1); assert.match(await page.locator('#card h3').textContent(), new RegExp(states[1].title));
        await office(() => window.__office2.closeCard());
        assert.ok(states.filter(s => s.agent === 'cursor').every(s => s.tool === null), 'cursor sessions carry no tool');
        for (const s of states.filter(x => x.status === 'working' && !x.wild)) assert.equal(s.place, s.tool ? {read:'field', shell:'quarry', mcp:'depot', edit:'bench'}[s.tool] : 'bench', `${s.title} stands at the station of its last tool`);
        const zones = await office(() => window.__office2.sessions.filter(s => !s.pod && !window.__office2.isWild(s)).map(s => ({ title: s.title, place: s.place, zone: (window.__office2.world.rooms[s.place] || {}).zone, floor: window.__office2.world.rooms[s.place] && window.__office2.world.rooms[s.place].zone && window.__office2.world.rooms[s.place].zone.floor, at: (() => { const z = window.__office2.world.rooms[s.place].zone; return s.pos.i >= z.i0 && s.pos.i < z.i0 + z.w && s.pos.j >= z.j0 && s.pos.j < z.j0 + z.d; })() })));
        for (const z of zones) assert.ok(z.at, `${z.title} stands inside the ${z.place} zone`);
        assert.equal(new Set(zones.map(z => z.floor)).size >= 5, true, 'stations stand on distinct floors');
        const parents = states.map((s, k) => [k, s]).filter(([, s]) => s.title);
        const handoff = await office(() => window.__office2.sessions.filter(s => s.parent != null).map(s => ({ title: s.title, parent: window.__office2.sessions[s.parent].title, agent: s.agent, parentAgent: window.__office2.sessions[s.parent].agent })));
        assert.equal(handoff.length, 3); assert.ok(handoff.some(h => h.agent !== h.parentAgent), 'a handoff child may run on another agent than its parent');
        for (const h of handoff) assert.ok(rowText.some(t => t.includes(h.title)), `handoff child ${h.title} is listed like any session`);
        result.passed.push('base info lists 24 companions, each with its title and the sentence of its status, and opens the card; every standing companion is inside the zone of its station; working companions stand at the station of their last tool; cursor has none; 3 handoff children are ordinary listed sessions');
        await office(() => window.__office2.openCard(window.__office2.sessions.find(s => s.title.includes('<v2>'))));
        assert.ok((await page.locator('#card h3').textContent()).includes('<v2>') && await page.locator('#card h3 v2').count() === 0, 'session text is escaped in the card');
        assert.ok(rowText.some(t => t.includes('<v2>')) && await page.locator('#baseinfo v2').count() === 0, 'session text is escaped in the list');
        await office(() => window.__office2.closeCard());
        result.passed.push('a title containing <v2> renders as text, not markup, in the card and the list');

        const order = await office(() => window.__office2.rollup().map(s => s.status));
        assert.deepEqual(order, [...order].sort((a, b) => (a === 'blocked' ? 1 : 0) - (b === 'blocked' ? 1 : 0)), 'done and ended rank before blocked, like status_rank');
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
        result.passed.push('N walks the queue in status_rank order (done and ended before blocked) and the first chip always names the N target; the card is read-only; ㅜ works as N');

        await office(() => window.__office2.openCard(window.__office2.sessions.find(s => s.agent === 'cursor')));
        assert.match(await page.locator('#card .reply').textContent(), /convo_log에 대화 turn이 없습니다/);
        const srBefore = await page.locator('#sr').textContent(); await office(() => window.__office2.closeCard()); assert.equal(await page.locator('#sr').textContent(), srBefore, 'closing a card leaves the live region text unchanged');
        for (const agent of ['codex', 'claude', 'kiro']) {
          const reply = await office(a => { const s = window.__office2.sessions.find(x => x.agent === a); window.__office2.openCard(s); return s.reply; }, agent);
          const norm = s => s.replace(/[\s`*|]/g, ''); // the card renders markdown, so compare the text without whitespace and marks
          assert.equal(norm(await page.locator('#card .reply').textContent()), norm(reply.replace(/^\|?-+\|.*$/gm, '')), `${agent} card shows its own last reply`);
          await office(() => window.__office2.closeCard());
        }
        result.passed.push('card shows the convo_log reply of a codex, a claude and a kiro session and states that cursor has none');

        await office(() => window.__office2.setCam(window.__office2.fitTarget()));
        const pt = await office(() => window.__office2.stagePoint(window.__office2.sessions.find(s => s.status === 'blocked' && !s.walking)));
        await page.locator('#stage').scrollIntoViewIfNeeded(); const box = await page.locator('#scene').boundingBox();
        await page.mouse.move(box.x + pt.x, box.y + pt.y);
        assert.ok((await sim()).some(s => s.hover && s.status === 'blocked'), 'hovering a waiting AI marks it');
        assert.equal(await page.locator('#tip').isVisible(), true);
        await page.mouse.move(box.x + 5, box.y + box.height - 5);
        await page.locator('#scene').dispatchEvent('pointerleave'); assert.equal(await page.locator('#tip').isVisible(), false, 'tooltip hides when the pointer leaves the scene');
        assert.ok(!(await sim()).some(s => s.hover), 'no companion stays marked after the pointer leaves');
        result.passed.push('hover marks the companion under the pointer and shows its tooltip; both clear when the pointer leaves');

        const target = await office(() => window.__office2.fitTarget());
        await office(() => window.__office2.closeCard()); await office(c => window.__office2.setCam({...c, y:c.y + 200}), target); await press('0'); await office(() => window.__office2.easeFrames(240));
        const cam = await office(() => window.__office2.cam);
        assert.ok(Math.abs(cam.x - target.x) < 1 && Math.abs(cam.y - target.y) < 1 && Math.abs(cam.z - target.z) < .01, `0 returns the camera on x, y and zoom ${JSON.stringify({cam, target})}`);
        result.passed.push('after a vertical pan, 0 brings the camera back on x, y and zoom');

        const today = await page.locator('.tab[aria-selected="true"] .name').textContent(); assert.match(today, /오늘/, 'the page opens on the day\'s terrain and marks that tab');
        assert.match(await page.locator('#hud .chip.title').textContent(), /오늘/, 'the HUD title chip marks the day\'s terrain');
        const fx = await office(() => [[49, 50, 65, 80, 90, null].map(window.__office2.ctxTier), [99, 100, 500, 2000, 5000, 10000].map(window.__office2.rankOf), [23, 24, 72, 168].map(window.__office2.ageOf), [{hours:9, tools:0}, {hours:0.5, tools:0}, {hours:3, tools:0}, {hours:2, tools:2000}].map(window.__office2.farewell)]);
        assert.deepEqual(fx, [[0, 1, 2, 3, 4, 0], [0, 1, 2, 3, 4, 5], [0, 1, 2, 3], ['휴… 종료', '신나게 종료!', '수고했습니다', '휴… 종료']], 'context tiers, ranks, age marks and farewell lines follow the stated thresholds');
        const mdOut = await office(() => window.__office2.md('<script>x</script> **b** `c` [l](https://example.test/a)\n\n- a\n  - b\n\n```\nq<y\n```\n\n| h |\n|---|\n| v |\n\n1 예   2 아니요'));
        assert.ok(!mdOut.includes('<script>') && mdOut.includes('&lt;script&gt;') && mdOut.includes('<b>b</b>') && mdOut.includes('<code>c</code>') && mdOut.includes('href="https://example.test/a"') && mdOut.includes('<ul><li>a<ul><li>b</li></ul></li></ul>') && mdOut.includes('<pre>q&lt;y</pre>') && mdOut.includes('<th>h</th>') && mdOut.includes('class="choices"'), `markdown renders escaped text, bold, code, links, nested lists, fences, tables and choice lines: ${mdOut}`);
        await office(() => window.__office2.openCard(window.__office2.sessions.find(s => s.title === '히스토리 정리'))); assert.equal(await page.locator('#card .reply table').count(), 1, 'a reply with a markdown table renders a table in the card'); await office(() => window.__office2.closeCard());
        await press('?'); assert.equal(await page.locator('#help.open').count(), 1, '? opens the shortcut help'); await press('Escape'); assert.equal(await page.locator('#help.open').count(), 0, 'Esc closes the help first');
        // wild sessions, capture and release
        const wild0 = await office(() => window.__office2.sessions.filter(s => window.__office2.isWild(s)).map(s => ({ title: s.title, outside: !window.__office2.inPlateau(Math.floor(s.pos.i), Math.floor(s.pos.j)), land: window.__office2.landOutside(window.__office2.world, Math.floor(s.pos.i), Math.floor(s.pos.j)) })));
        assert.equal(wild0.length, 2, 'two uncategorized sessions start wild'); assert.ok(wild0.every(w => w.outside && w.land), 'wild sessions stand on land outside the palisade');
        assert.ok((await page.locator('#baseinfo .bi-row').allTextContents()).filter(t => t.includes('야생')).length === 2, 'the list says 야생 for both');
        await office(() => window.__office2.advance(8)); assert.deepEqual(await office(() => window.__office2.validateWorld()), [], 'wandering keeps wild sessions on land');
        await page.locator('#b-catch').click(); await page.waitForTimeout(motion === 'reduce' ? 400 : 3400); await office(() => window.__office2.advance(40));
        const caught = await office(() => { const s = window.__office2.sessions.find(x => x.title === '구독 정리'); return { cat: s.cat, inside: window.__office2.inPlateau(Math.floor(s.pos.i), Math.floor(s.pos.j)), place: s.place, walking: s.walking, capture: !!s.capture }; });
        assert.ok(caught.cat !== 'none' && caught.inside && caught.place === 'wait' && !caught.walking && !caught.capture, `the captured session is inside at its station ${JSON.stringify(caught)}`);
        assert.deepEqual(await office(() => window.__office2.validateWorld()), [], 'layout valid after the capture');
        await page.locator('#b-release').click(); await office(() => window.__office2.advance(60));
        const released = await office(() => window.__office2.sessions.filter(s => window.__office2.isWild(s)).map(s => ({ title: s.title, outside: !window.__office2.inPlateau(Math.floor(s.pos.i), Math.floor(s.pos.j)) })));
        assert.equal(released.length, 2, 'release makes one captured session wild again'); assert.ok(released.every(w => w.outside), 'released sessions stand outside');
        assert.deepEqual(await office(() => window.__office2.validateWorld()), [], 'layout valid after the release');
        await press('d'); assert.equal(await page.locator('#dex.open').count(), 1, 'D opens the dex'); assert.equal(await page.locator('#dex .cell').count(), 16, 'the dex has 16 species cells');
        const known = await page.locator('#dex .cell:not(.unknown)').count(); assert.match(await page.locator('#b-dex').textContent(), new RegExp(`${known}/16`), 'the dex chip count matches the panel'); await press('Escape'); assert.equal(await page.locator('#dex.open').count(), 0);
        result.passed.push('two uncategorized sessions roam outside; assigning a category captures one into the base and clearing it releases one; the 16-cell dex opens with D and its chip count matches');
        result.passed.push('today\'s terrain is marked; context/rank/age/farewell thresholds hold; the markdown renderer escapes first; ? toggles the help overlay');
        await page.locator('#stage').focus(); await page.keyboard.press('Tab');
        assert.equal(await page.evaluate(() => document.activeElement.id), 'b-dex', 'Tab moves focus from the stage to its first control, the dex chip');
        await page.keyboard.press('Tab'); assert.equal(await page.evaluate(() => document.activeElement.className.includes('bi-row')), true, 'the next Tab reaches the base info list');
        result.passed.push('Tab moves focus from the stage to the dex chip, then into the base info list');
      }

      for (let k = 0; k < 5; k++) {
        await choose(k);
        const idleBefore = (await sim()).filter(s => s.status === 'idle').length;
        const starter = (await sim()).find(s => s.status === 'idle' && !s.walking && !s.gone), leaver = (await sim()).find(s => s.status === 'done' && !s.gone).title;
        await office(s => window.__office2.openCard(window.__office2.sessions.find(x => x.title === s)), leaver);
        await page.locator('#b-start').click();
        await office(() => window.__office2.advance(30));
        const mid = (await sim()).find(s => s.title === starter.title); assert.ok(mid.status === 'working' && mid.place === 'bench' && !mid.walking, `scene ${k + 1}: the starter has arrived before the end demo`);
        assert.match((await page.locator('#baseinfo .bi-row').allTextContents()).find(t => t.includes(starter.title)), /일하는 중/, `scene ${k + 1}: the list sentence updates when the walk ends`);
        await page.locator('#b-end').click();
        await office(() => window.__office2.advance(30));
        const after = await sim();
        assert.equal(after.filter(s => s.status === 'idle').length, idleBefore - 1, `scene ${k + 1}: one companion stopped resting`);
        const st = after.find(s => s.title === starter.title), lv = after.find(s => s.title === leaver);
        assert.ok(st.status === 'working' && st.place === 'bench' && !st.pod && !st.walking, `scene ${k + 1}: "${starter.title}" walked from ${starter.pod ? 'its bed' : 'the feed box'} to a bench`);
        assert.ok(lv.status === 'ended' && lv.place === 'terminal' && !lv.gone, `scene ${k + 1}: "${leaver}" stands by the terminal as an ended session`);
        assert.equal(await page.locator('#card.open').count(), 1, `scene ${k + 1}: the card of the ended companion stays open`);
        assert.match(await page.locator('#card .badge').textContent(), /ended/, `scene ${k + 1}: the open card now shows ended`);
        assert.ok(!after.some(s => s.walking && !s.wild), `scene ${k + 1}: nobody is stuck walking (wild sessions roam by design)`);
        assert.deepEqual(await office(() => window.__office2.validateWorld()), [], `scene ${k + 1}: layout still valid after transitions`);
        assert.equal(await page.locator('#baseinfo .bi-row').count(), 24, `scene ${k + 1}: base info keeps the ended companion`);
        assert.match((await page.locator('#baseinfo .bi-row').allTextContents()).find(t => t.includes(leaver)), /종료됨/, `scene ${k + 1}: the list sentence says ended`);
        assert.ok((await office(() => window.__office2.rollup().map(s => s.title))).includes(leaver), `scene ${k + 1}: the ended session stays in the queue`);
        await office(() => window.__office2.closeCard());
        if (k === 0) { // overflow control: three more starters than listed bench spots, the last one must spill into an aisle
          for (let n = 0; n < 4; n++) { await page.locator('#b-start').click(); await office(() => window.__office2.advance(30)); }
          const crowd = await office(() => window.__office2.validateWorld()); assert.deepEqual(crowd, [], 'starters beyond the listed spots take clear tiles without crowding');
          const benchers = await office(() => window.__office2.sessions.filter(s => s.status === 'working' && s.place === 'bench').map(s => { const w = window.__office2.world, z = w.rooms.bench.zone, i = Math.floor(s.pos.i), j = Math.floor(s.pos.j), inZone = i >= z.i0 && i < z.i0 + z.w && j >= z.j0 && j < z.j0 + z.d; const foreign = !inZone && Object.values(w.rooms).some(r => r.zone && r.zone !== z && i >= r.zone.i0 && i < r.zone.i0 + r.zone.w && j >= r.zone.j0 && j < r.zone.j0 + r.zone.d); return { title: s.title, inZone, foreign }; }));
          assert.ok(benchers.length >= 7, `seven companions work at benches after the overflow, two more than the listed spots (${benchers.length})`);
          assert.ok(benchers.every(b => !b.foreign), 'no bench worker stands inside another station\'s zone');
          assert.ok(benchers.length > await office(() => window.__office2.world.rooms.bench.spots.length), 'more bench workers than listed spots, so extra tiles were used');
          const spill = await office(() => { const o = window.__office2, w = o.world, z = w.rooms.bench.zone, added = []; for (let i = z.i0; i < z.i0 + z.w; i++) for (let j = z.j0; j < z.j0 + z.d; j++) { const k = i + ',' + j; if (!w.blocked.has(k)) { w.blocked.add(k); added.push(k); } }
            const sp = o.freeSpot(w, w.rooms.bench); for (const k of added) w.blocked.delete(k); return sp ? { zone: o.zoneAt(sp[0], sp[1]) ? o.zoneAt(sp[0], sp[1]).key : null, inside: o.inPlateau(sp[0], sp[1]) } : null; });
          assert.ok(spill && spill.zone === null && spill.inside, `with the bench zone full, freeSpot spills into an aisle tile, never another zone (${JSON.stringify(spill)})`);
          const order2 = await office(() => window.__office2.rollup().map(s => s.status)); assert.ok(order2.includes('ended') && order2.indexOf('blocked') > order2.lastIndexOf('ended'), 'with an ended session present, ended still ranks before blocked');
          result.passed.push('overflow control: repeated starts beyond the five listed bench spots keep the layout contract, spill only into aisles, and the queue keeps ended before blocked');
        }
        if (motion === 'no-preference' && k === 1) { const shot = path.join(artifacts, 'scene2-after-transitions.png'); await page.locator('#stage').screenshot({ path: shot }); result.screenshots.push(shot); }
      }
      result.passed.push(`${motion}: in all five scenes a resting companion walks to a bench and a done companion ends by the terminal, staying listed and queued`);

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
          'Rendering cost with real session counts above 24', 'Screen-reader behaviour of the canvas scene', 'The key that opens this window from MindPlayer',
          'The daily terrain seed on a day other than the test day', 'Copied design or UI text beyond the banned-name hash list (reviewed by eye only)'] };
      fs.writeFileSync(record, JSON.stringify(summary, null, 2) + '\n');
    }
    console.log(JSON.stringify({ result: result.result, passed: result.passed.length, failed: result.failed, keyboardEvents: result.keyboardEvents,
      externalRequests: result.externalRequests, pageErrors: result.pageErrors, consoleErrors: result.consoleErrors, providerCalls: result.providerCalls,
      before: result.before, after: result.after, cleanup: result.cleanup, receipt: path.join(artifacts, 'validation.json') }));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
