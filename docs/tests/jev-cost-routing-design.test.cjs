'use strict';

// 문서 전용 QA. 기존 설치 Playwright/Chrome만 사용합니다.
// 예: MINDPLAYER_PLAYWRIGHT_MODULE=/절대경로/playwright node docs/tests/jev-cost-routing-design.test.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const vm = require('node:vm');
const http = require('node:http');
const { execFileSync } = require('node:child_process');
const { chromium } = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');

const root = path.resolve(__dirname, '../..');
const artifact = path.join(root, 'docs/plans/2026-10-15-jev-cost-routing-design.ko.html');
const html = fs.readFileSync(artifact, 'utf8');
const sha = data => crypto.createHash('sha256').update(data).digest('hex');

async function main() {
  const scripts = [...html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)];
  assert.equal(scripts.length, 1);
  new vm.Script(scripts[0][1]);
  assert.match(html, /<html lang="ko">/);
  assert(!/<(?:script|img|iframe)\b[^>]*src=/i.test(html), '외부 실행/이미지 의존성 없음');
  assert(!/<link\b[^>]*href="(?!data:)/i.test(html), '외부 stylesheet 없음');
  assert(!/\b(?:fetch|XMLHttpRequest|WebSocket|localStorage|sessionStorage)\s*[.(]/.test(scripts[0][1]), '네트워크/저장소 없음');
  assert(!/\bsk-(?:proj-)?[A-Za-z0-9_-]{16,}|\bAKIA[A-Z0-9]{16}|-----BEGIN (?:RSA |OPENSSH )?PRIVATE KEY-----/.test(html), 'credential pattern 없음');
  const ids = [...html.matchAll(/\bid="([^"]+)"/g)].map(m => m[1]);
  assert.equal(new Set(ids).size, ids.length, 'ID 중복 없음');
  for (const phrase of ['B. 요구 역량 → 코드 정책', 'D1', 'D6', 'AC1', 'AC10', '실제 Jev 호출', 'queued', 'helper']) assert(html.includes(phrase), `문서 근거 누락: ${phrase}`);
  console.log('PASS static: Korean, script syntax, unique IDs, no dependency/network/storage, source coverage');

  const screenshots = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-jev-design-'));
  const server = http.createServer((req, res) => {
    try {
      const pathname = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
      const filename = path.resolve(root, '.' + pathname);
      if (!filename.startsWith(root + path.sep) || !fs.statSync(filename).isFile()) throw new Error('not found');
      res.writeHead(200, { 'Content-Type': filename.endsWith('.html') ? 'text/html; charset=utf-8' : 'text/plain; charset=utf-8' });
      res.end(fs.readFileSync(filename));
    } catch { res.writeHead(404); res.end('Not found'); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = 'http://127.0.0.1:' + server.address().port;
  const errors = [], externalRequests = [];
  let browser;
  try {
    browser = await chromium.launch({ channel: 'chrome', headless: true });
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, reducedMotion: 'reduce' });
    page.on('pageerror', error => errors.push(String(error)));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    page.on('request', request => { if (!request.url().startsWith(origin + '/')) externalRequests.push(request.url()); });
    await page.goto(origin + '/docs/plans/2026-10-15-jev-cost-routing-design.ko.html');
    assert.equal(await page.title(), 'MindPlayer · Jev 비용효율 라우팅 설계');
    assert.equal(await page.locator('.terminal').count(), 2);
    assert.equal(await page.locator('details[open]').count(), 0, 'details 기본 닫힘');
    assert.equal(await page.locator('.terminal button, .terminal input, .terminal select').count(), 0, '터미널 신규 컨트롤 없음');
    assert.equal(await page.locator('.model-line.changed').count(), 1, '선택 모델 표시줄 한 곳만 변경');
    for (const view of ['#current-view', '#proposed-view']) {
      assert.equal(await page.locator(view + ' .pane').count(), 7);
      const rows = await page.locator(view + ' .pane').evaluateAll(panes => { const bands = new Map(); for (const pane of panes) { const y = pane.getBoundingClientRect().top; bands.set(y, (bands.get(y) || 0) + 1); } return [...bands.values()]; });
      assert.deepEqual(rows, [4, 3], '4-top/3-bottom');
      const footer = await page.locator(view).evaluate(el => { const t = el.querySelector('.terminal').getBoundingClientRect(); const q = el.querySelector('.quota-footer').getBoundingClientRect(); return { sameWidth: Math.abs(t.width - q.width) < 1, below: q.top >= el.querySelector('.pane-grid').getBoundingClientRect().bottom }; });
      assert(footer.sameWidth && footer.below, 'fullwidth quota footer');
    }
    assert.deepEqual(await page.locator('#current-view .pane-head').allTextContents(), await page.locator('#proposed-view .pane-head').allTextContents(), '7 pane identities');
    for (const index of [0, 2, 3, 4, 5, 6]) assert.equal(await page.locator('#current-view .pane').nth(index).innerHTML(), await page.locator('#proposed-view .pane').nth(index).innerHTML(), `unchanged pane ${index + 1}`);
    const viewport = await page.evaluate(() => ({ current: document.querySelector('#current-view').getBoundingClientRect().bottom, proposed: document.querySelector('#proposed-view').getBoundingClientRect().bottom, changed: document.querySelector('.model-line.changed').getBoundingClientRect().bottom, height: innerHeight, overflow: document.documentElement.scrollWidth > innerWidth }));
    assert(viewport.current < viewport.height && viewport.proposed < viewport.height && viewport.changed < viewport.height && !viewport.overflow, 'first viewport compact');
    await page.screenshot({ path: path.join(screenshots, 'desktop.png'), fullPage: true });

    await page.locator('#scenarios > summary').focus(); await page.keyboard.press('Enter'); assert(await page.locator('#scenarios').evaluate(el => el.open));
    for (const [key, expected] of Object.entries({ simple: 'A 요청', complex: 'B 요청', followup: 'B 유지', timeout: 'A 유지', unknown: 'B 유지' })) { const button = page.locator(`[data-scenario="${key}"]`); await button.focus(); await page.keyboard.press('Enter'); assert((await page.locator('#proposed-view [data-demo-model]').textContent()).endsWith(expected)); assert.equal(await page.locator('[data-scenario][aria-pressed="true"]').count(), 1); assert.equal(await page.locator('#current-view [data-demo-prompt]').textContent(), await page.locator('#proposed-view [data-demo-prompt]').textContent()); }
    await page.locator('#scenarios > summary').focus(); await page.keyboard.press('Space'); assert.equal(await page.locator('#scenarios').evaluate(el => el.open), false);
    for (const id of ['requirements', 'decisions', 'architecture', 'security', 'provider', 'acceptance', 'evidence', 'implementation-contract']) { const summary = page.locator(`#${id} > summary`); await summary.focus(); await page.keyboard.press('Enter'); assert(await page.locator(`#${id}`).evaluate(el => el.open)); await page.keyboard.press('Space'); assert.equal(await page.locator(`#${id}`).evaluate(el => el.open), false); }
    await page.setViewportSize({ width: 969, height: 822 }); await page.evaluate(() => scrollTo(0, 0)); assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false); await page.screenshot({ path: path.join(screenshots, 'compact.png'), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 }); await page.evaluate(() => scrollTo(0, 0)); assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false); const narrow = await page.locator('#proposed-view').evaluate(el => ({ needsScroll: el.scrollWidth > el.clientWidth, before: el.scrollLeft })); assert(narrow.needsScroll); await page.locator('#proposed-view').focus(); await page.keyboard.press('ArrowRight'); await page.waitForFunction(() => document.querySelector('#proposed-view').scrollLeft > 0); await page.screenshot({ path: path.join(screenshots, 'narrow.png'), fullPage: true });
    assert.deepEqual(externalRequests, []); assert.deepEqual(errors, []);
    console.log('PASS browser: desktop 1440/969, narrow 390, 4+3, fullwidth quota, one model-line delta');
    console.log('PASS interaction: 5 synthetic scenarios, keyboard details, horizontal scroll, reduced motion');
    console.log('PASS browser network/console: no external requests or errors');
    console.log(JSON.stringify({ verified_at: new Date().toISOString(), artifact_sha256: sha(html), source_revision: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(), browser: await browser.version(), screenshots, external_requests: externalRequests, errors, prototype_only: true, actual_jev_calls: 0 }, null, 2));
  } finally { if (browser) await browser.close(); await new Promise(resolve => server.close(resolve)); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
