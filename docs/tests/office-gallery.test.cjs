'use strict';

// Synthetic HTML only. Writes screenshots/receipt to a task-owned temp directory;
// reads but never changes source. Closes browser/server in finally. No CLI/auth calls.
// MINDPLAYER_PLAYWRIGHT_MODULE=/existing/playwright node docs/tests/office-gallery.test.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const http = require('node:http');
const crypto = require('node:crypto');
const vm = require('node:vm');

const htmlPath = path.resolve(__dirname, '../plans/2026-10-23-mindplayer-office-gallery.ko.html');
const source = fs.readFileSync(htmlPath, 'utf8');
const hash = crypto.createHash('sha256').update(source).digest('hex');
const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-office-gallery-'));
const receipt = { scope: 'HTML mock only; no real TTY/provider verification', sourceHash: hash,
  passed: 0, failed: 0, cases: [], screenshots: [], consoleErrors: [], externalRequests: [], artifacts };
let browser, server;
async function check(name, fn) {
  try { await fn(); receipt.passed++; receipt.cases.push({ name, result: 'pass' }); }
  catch (error) { receipt.failed++; receipt.cases.push({ name, result: 'fail', message: error.message }); }
}

async function main() {
  await check('inline JavaScript syntax', () => {
    const scripts = [...source.matchAll(/<script>([\s\S]*?)<\/script>/g)];
    assert.equal(scripts.length, 1); scripts.forEach(s => new vm.Script(s[1]));
  });
  const { chromium } = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');
  server = http.createServer((req, res) => {
    if (req.url !== '/' && req.url !== '/favicon.ico') { res.writeHead(404); res.end(); return; }
    res.writeHead(200, { 'content-type': req.url === '/' ? 'text/html; charset=utf-8' : 'image/x-icon' });
    res.end(req.url === '/' ? source : '');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch({ channel: 'chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 }, reducedMotion: 'reduce' });
  page.on('pageerror', error => receipt.consoleErrors.push(error.message));
  page.on('console', msg => { if (msg.type() === 'error') receipt.consoleErrors.push(msg.text()); });
  await page.route('**/*', route => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) { receipt.externalRequests.push(url.origin); return route.abort(); }
    return route.continue();
  });
  await page.goto(origin, { waitUntil: 'networkidle' });
  await check('five genuinely different office scenes', async () => {
    assert.equal(await page.locator('.tab').count(), 5);
    assert.equal(await page.locator('.card').count(), 5);
    const thumbnails = await page.locator('.card canvas').evaluateAll(nodes => nodes.map(n => n.toDataURL()));
    assert.equal(new Set(thumbnails).size, 5);
  });
  for (let idea = 1; idea <= 5; idea++) {
    await page.locator(`.tab[data-idea="${idea}"]`).click();
    await check(`idea ${idea}: seven unique identities and desks`, async () => {
      const ids = await page.locator('.session-row').evaluateAll(nodes => nodes.map(n => n.dataset.sessionId));
      assert.equal(ids.length, 7); assert.equal(new Set(ids).size, 7);
      assert.equal(await page.locator('.desk-hit').count(), 7);
      assert.equal(await page.locator('.tab[aria-pressed=true]').count(), 1);
      assert.equal(await page.locator('.card[aria-pressed=true]').count(), 1);
    });
    await check(`idea ${idea}: keyboard selection, native 4+3, quota and return`, async () => {
      await page.locator('#terminal').focus();
      await page.keyboard.press('ArrowRight');
      assert.equal((await page.evaluate(() => officeDemo.getState())).selected, 1);
      const quotaBefore = await page.locator('#quota').innerText();
      await page.keyboard.press('Enter');
      assert.equal((await page.evaluate(() => officeDemo.getState())).mode, 'native');
      assert.equal(await page.locator('.pane-row.top .native-pane').count(), 4);
      assert.equal(await page.locator('.pane-row.bottom .native-pane').count(), 3);
      assert.match(await page.locator('.native-pane.focused').innerText(), /테스트 보강/);
      const geometry = await page.locator('#quota').evaluate(node => ({
        width: node.getBoundingClientRect().width,
        parent: node.parentElement.getBoundingClientRect().width
      }));
      assert.ok(Math.abs(geometry.width - geometry.parent) <= 3);
      assert.equal(await page.locator('#quota').innerText(), quotaBefore);
      await page.keyboard.press('Escape');
      assert.equal((await page.evaluate(() => officeDemo.getState())).mode, 'office');
      assert.equal((await page.evaluate(() => officeDemo.getState())).selected, 1);
    });
    await check(`idea ${idea}: Ctrl-G trace keeps session`, async () => {
      await page.locator('#terminal').focus(); await page.keyboard.press('Control+g');
      assert.match(await page.locator('#traceStage').innerText(), /session-b02/);
      await page.keyboard.press('Escape');
      await page.keyboard.press('Enter'); await page.keyboard.press('Control+g');
      await page.keyboard.press('Escape');
      assert.equal((await page.evaluate(() => officeDemo.getState())).mode, 'native');
      assert.match(await page.locator('.native-pane.focused').innerText(), /테스트 보강/);
      await page.keyboard.press('Escape');
    });
    await check(`idea ${idea}: focus view never duplicates selected character identity`, async () => {
      const labels = await page.locator('.desk-hit').evaluateAll(nodes => nodes.map(n => n.getAttribute('aria-label')));
      const numbers = labels.map(label => label.match(/책상 (\d+):/)[1]);
      assert.equal(new Set(numbers).size, 7);
    });
    await page.locator('#terminal').focus(); await page.keyboard.press('ArrowLeft');
    const shot = path.join(artifacts, `office-${idea}-1440.png`);
    await page.screenshot({ path: shot, fullPage: true }); receipt.screenshots.push(shot);
  }
  await check('144×42 real half-block cells use canvas pixel colors', async () => {
    await page.locator('#renderToggle').click();
    assert.equal(await page.locator('#cellView span').count(), 144 * 42);
    const result = await page.evaluate(() => {
      const pixels = document.querySelector('#scene').getContext('2d').getImageData(0, 0, 144, 84).data;
      const nodes = document.querySelectorAll('#cellView span');
      return [0, 75, 2010, 6047].every(i => {
        const y = Math.floor(i / 144) * 2, x = i % 144;
        const top = (y * 144 + x) * 4, bottom = ((y + 1) * 144 + x) * 4;
        return nodes[i].textContent === '▀' && nodes[i].style.color === `rgb(${pixels[top]}, ${pixels[top+1]}, ${pixels[top+2]})`
          && nodes[i].style.backgroundColor === `rgb(${pixels[bottom]}, ${pixels[bottom+1]}, ${pixels[bottom+2]})`;
      });
    });
    assert.equal(result, true);
    const layout = await page.locator('#cellView span').first().evaluate(node => ({
      cell: node.getBoundingClientRect().toJSON(),
      scene: document.querySelector('#drawing').getBoundingClientRect().toJSON()
    }));
    assert.ok(Math.abs(layout.cell.width * 144 - layout.scene.width) < 1);
    assert.ok(Math.abs(layout.cell.height * 42 - layout.scene.height) < 1);
    const shot = path.join(artifacts, 'office-halfblock-1440.png');
    await page.screenshot({ path: shot, fullPage: true }); receipt.screenshots.push(shot);
    await page.locator('#renderToggle').click();
  });
  await check('late signals are unknown, not idle/completed', async () => {
    await page.selectOption('#scenario', 'stale');
    assert.equal((await page.evaluate(() => officeDemo.getState())).sessions[0].status, 'unknown');
    assert.match(await page.locator('.session-row.selected').innerText(), /\? 미확인/);
    assert.match(await page.locator('#message').innerText(), /신호 지연/);
  });
  for (const [scenario, status] of [['attention', 'blocked'], ['completion', 'completed'], ['ended', 'ended']]) {
    await check(`scenario ${scenario}: explicit state without moving desks`, async () => {
      const before = (await page.evaluate(() => officeDemo.getState())).positions;
      await page.selectOption('#scenario', scenario);
      const state = await page.evaluate(() => officeDemo.getState());
      assert.equal(state.sessions[0].status, status); assert.deepEqual(state.positions, before);
      if (status === 'completed') assert.match(await page.locator('#sessionRail').innerText(), /작업 성공은 별도/);
    });
  }
  await check('77 sessions, duplicate titles, explicit paging and identity retained', async () => {
    await page.selectOption('#scenario', 'dense');
    let state = await page.evaluate(() => officeDemo.getState());
    assert.equal(state.sessions.length, 77); assert.equal(new Set(state.sessions.map(s => s.id)).size, 77);
    assert.equal(state.sessions[0].title, state.sessions[1].title);
    await page.locator('#terminal').focus(); for (let i = 0; i < 8; i++) await page.keyboard.press('ArrowRight');
    state = await page.evaluate(() => officeDemo.getState());
    assert.equal(state.selected, 8); assert.equal(state.visible[0], 'dense-session-008');
    assert.match(await page.locator('#message').innerText(), /화면 밖 70개/);
  });
  await check('empty does not fabricate session or usage', async () => {
    await page.selectOption('#scenario', 'empty');
    assert.equal(await page.locator('.session-row').count(), 0);
    assert.equal(await page.locator('.desk-hit').count(), 0);
    assert.equal(await page.locator('#nativeButton').isDisabled(), true);
    assert.match(await page.locator('#quota').innerText(), /kiro — 조회 안 됨/);
    await page.locator('#terminal').focus(); await page.keyboard.press('Enter');
    assert.equal((await page.evaluate(() => officeDemo.getState())).mode, 'office');
  });
  await check('native pane order independent of selection; dense view does not pull first-page peers', async () => {
    await page.selectOption('#scenario', 'dense');
    const before = (await page.evaluate(() => officeDemo.getState())).nativePaneIds;
    await page.locator('#terminal').focus();
    for (let i = 0; i < 8; i++) await page.keyboard.press('ArrowRight');
    await page.keyboard.press('Enter');
    const paneIds = await page.locator('.native-pane').evaluateAll(nodes => nodes.map(n => n.dataset.sessionId));
    assert.deepEqual(paneIds, before.slice(7, 14));
    assert.equal(await page.locator('.native-pane.focused').getAttribute('data-session-id'), 'dense-session-009');
    assert.deepEqual((await page.evaluate(() => officeDemo.getState())).nativePaneIds, before);
    await page.keyboard.press('Escape');
  });
  await check('current reconstruction does not claim proposed unknown status', async () => {
    assert.doesNotMatch(await page.locator('.current-list').innerText(), /\?/);
  });
  await page.selectOption('#scenario', 'normal');
  await check('click then Enter / native button then Escape preserves keyboard ownership', async () => {
    await page.locator('.session-row').nth(2).click();
    await page.keyboard.press('Enter');
    assert.equal((await page.evaluate(() => officeDemo.getState())).mode, 'native');
    assert.match(await page.locator('.native-pane.focused').innerText(), /배포 준비/);
    await page.keyboard.press('Escape');
    await page.locator('#nativeButton').click(); await page.keyboard.press('Escape');
    assert.equal((await page.evaluate(() => officeDemo.getState())).mode, 'office');
  });
  await check('Tab can leave the interactive office; scene is not a keyboard trap', async () => {
    await page.locator('.desk-hit').last().focus();
    for (let i = 0; i < 8; i++) await page.keyboard.press('Tab');
    assert.equal(await page.locator('#terminal').evaluate(node => node.contains(document.activeElement)), false);
  });
  for (const width of [1280, 850, 390, 320]) {
    await page.setViewportSize({ width, height: 1050 });
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await check(`${width}px: page fits, scene and quota visible`, async () => {
      const box = await page.evaluate(() => ({
        viewport: innerWidth, scroll: document.documentElement.scrollWidth,
        scene: document.querySelector('#drawing').getBoundingClientRect().toJSON(),
        quota: document.querySelector('#quota').getBoundingClientRect().toJSON()
      }));
      assert.ok(box.scroll <= box.viewport + 1, JSON.stringify(box));
      assert.ok(box.scene.width > 100 && box.scene.right <= width + 1);
      assert.ok(box.quota.width > 100 && box.quota.right <= width + 1);
    });
    const shot = path.join(artifacts, `office-responsive-${width}.png`);
    await page.screenshot({ path: shot, fullPage: true }); receipt.screenshots.push(shot);
  }
  await check('no page errors, no external requests, no source writes', () => {
    assert.deepEqual(receipt.consoleErrors, []); assert.deepEqual(receipt.externalRequests, []);
    assert.equal(crypto.createHash('sha256').update(fs.readFileSync(htmlPath)).digest('hex'), hash);
  });
}
main().catch(error => { receipt.failed++; receipt.cases.push({ name: 'harness', result: 'fail', message: error.message }); })
  .finally(async () => {
    if (browser) await browser.close();
    if (server) await new Promise(resolve => server.close(resolve));
    fs.writeFileSync(path.join(artifacts, 'validation.json'), JSON.stringify(receipt, null, 2));
    console.log(JSON.stringify({ passed: receipt.passed, failed: receipt.failed, artifacts,
      failures: receipt.cases.filter(c => c.result === 'fail') }, null, 2));
    process.exitCode = receipt.failed ? 1 : 0;
  });
