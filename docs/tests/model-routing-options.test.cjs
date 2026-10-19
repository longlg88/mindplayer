'use strict';

// Documentation-only QA. Uses an already installed Playwright module and Chrome.
// Example: MINDPLAYER_PLAYWRIGHT_MODULE=/absolute/path/to/playwright node docs/tests/model-routing-options.test.cjs
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
const artifact = path.join(root, 'docs/model-routing-options.ko.html');
const html = fs.readFileSync(artifact, 'utf8');
const archived = {
  'sense-model-topology.ko.html': '9f078af245813950d06c3e0e57cc2863c87b84e2df37a84386da274b07696ae5',
  'sense-model-ui.ko.html': 'f931bdcf155cd899602f0ef4ee5b0224908b9bf094a687dc0482ca6a59b710bd',
  'jev-execution-topology.html': '32ad7e3394867ea6890320ef627432975e0126f4df79c6b0c8f9bb80ef660cad',
  'jev-model-auto-simple.ko.html': '6a923bfac42dcb91b4f3fb02e83f14d960741a2463313965e4f932f1d4187f63'
};
const sha = data => crypto.createHash('sha256').update(data).digest('hex');

async function main() {
  const scripts = [...html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)];
  assert.equal(scripts.length, 1);
  for (const [, source] of scripts) new vm.Script(source);
  assert.match(html, /<html lang="ko">/);
  assert(!/<(?:script|img|iframe)\b[^>]*src=/i.test(html), 'No script/image/frame asset dependencies');
  assert(!/<link\b[^>]*href="(?!data:)/i.test(html), 'Only a self-contained empty favicon');
  assert(!/\b(?:fetch|XMLHttpRequest|WebSocket|localStorage|sessionStorage)\s*[.(]/.test(scripts[0][1]), 'No runtime transport or storage');
  assert(!/\bsk-(?:proj-)?[A-Za-z0-9_-]{16,}|\bAKIA[A-Z0-9]{16}|-----BEGIN (?:RSA |OPENSSH )?PRIVATE KEY-----/.test(html), 'Credential pattern scan');
  const ids = [...html.matchAll(/\bid="([^"]+)"/g)].map(m => m[1]);
  assert.equal(new Set(ids).size, ids.length, 'Unique element IDs');
  const links = [...html.matchAll(/<a\b[^>]*href="([^"]+)"/g)].map(m => m[1]);
  for (const link of links) {
    if (/^https:\/\//.test(link) || link.endsWith('.validation.json')) continue;
    const filename = path.resolve(path.dirname(artifact), link.split('#')[0]);
    assert(fs.existsSync(filename), 'Existing local link: ' + link);
  }
  for (const [filename, checksum] of Object.entries(archived)) {
    assert(!fs.existsSync(path.join(root, 'docs', filename)), 'Retired active path absent: ' + filename);
    assert.equal(sha(fs.readFileSync(path.join(root, 'docs/archive/2026-10-15-model-routing', filename))), checksum);
  }
  console.log('PASS static: Korean, script syntax, IDs, links, no runtime network/storage, credential patterns');
  console.log('PASS archive: 4 exact original checksums, 4 active paths removed, recoverable originals retained');

  const screenshots = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-model-options-'));
  const server = http.createServer((req, res) => {
    try {
      const pathname = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
      const filename = path.resolve(root, '.' + pathname);
      if (!filename.startsWith(root + path.sep) || !fs.statSync(filename).isFile()) throw new Error('Not found');
      const type = filename.endsWith('.html') ? 'text/html; charset=utf-8' : 'text/plain; charset=utf-8';
      res.writeHead(200, { 'Content-Type': type });
      res.end(fs.readFileSync(filename));
    } catch {
      res.writeHead(404);
      res.end('Not found');
    }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = 'http://127.0.0.1:' + server.address().port;
  const errors = [];
  const externalRequests = [];
  let browser;
  try {
    browser = await chromium.launch({ channel: 'chrome', headless: true });
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, reducedMotion: 'reduce' });
    page.on('pageerror', error => errors.push(String(error)));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    page.on('request', request => { if (!request.url().startsWith(origin + '/')) externalRequests.push(request.url()); });
    await page.goto(origin + '/docs/model-routing-options.ko.html');
    assert.equal(await page.title(), 'MindPlayer · PIM 없이, 입력마다 알맞은 모델로');
    assert.equal(await page.locator('.terminal').count(), 2);
    assert.equal(await page.locator('details[open]').count(), 0, 'All explanatory disclosures start closed');
    assert.equal(await page.locator('.terminal button, .terminal input, .terminal select').count(), 0, 'No product model controls');
    assert.equal(await page.locator('.model-line.changed').count(), 1);
    for (const view of ['#current-view', '#proposed-view']) {
      assert.equal(await page.locator(view + ' .pane').count(), 7);
      const rows = await page.locator(view + ' .pane').evaluateAll(panes => {
        const bands = new Map();
        for (const pane of panes) { const top = pane.getBoundingClientRect().top; bands.set(top, (bands.get(top) || 0) + 1); }
        return [...bands.values()];
      });
      assert.deepEqual(rows, [4, 3]);
      const footer = await page.locator(view).evaluate(el => {
        const terminal = el.querySelector('.terminal').getBoundingClientRect();
        const quota = el.querySelector('.quota-footer').getBoundingClientRect();
        return { sameWidth: Math.abs(terminal.width - quota.width) < 1, belowGrid: quota.top >= el.querySelector('.pane-grid').getBoundingClientRect().bottom };
      });
      assert(footer.sameWidth && footer.belowGrid, 'Full-width quota footer below 4+3 grid');
    }
    assert.deepEqual(await page.locator('#current-view .pane-head').allTextContents(), await page.locator('#proposed-view .pane-head').allTextContents());
    for (const index of [0, 2, 3, 4, 5, 6]) {
      assert.equal(await page.locator('#current-view .pane').nth(index).innerHTML(), await page.locator('#proposed-view .pane').nth(index).innerHTML());
    }
    const firstViewport = await page.evaluate(() => ({
      current: document.querySelector('#current-view').getBoundingClientRect().bottom,
      proposed: document.querySelector('#proposed-view').getBoundingClientRect().bottom,
      delta: document.querySelector('.model-line.changed').getBoundingClientRect().bottom,
      height: innerHeight,
      overflow: document.documentElement.scrollWidth > innerWidth
    }));
    assert(firstViewport.current < firstViewport.height && firstViewport.proposed < firstViewport.height && firstViewport.delta < firstViewport.height);
    assert.equal(firstViewport.overflow, false);
    await page.screenshot({ path: path.join(screenshots, 'desktop.png'), fullPage: true });

    // Keyboard-only native disclosure and ordinary button interaction.
    await page.locator('#scenarios > summary').focus();
    await page.keyboard.press('Enter');
    assert(await page.locator('#scenarios').evaluate(el => el.open));
    for (const [key, expected] of Object.entries({ simple: 'A 요청', complex: 'B 요청', followup: 'B 요청', timeout: 'A 유지', unknown: 'B 미확인' })) {
      const button = page.locator('[data-scenario="' + key + '"]');
      await button.focus();
      await page.keyboard.press('Enter');
      assert((await page.locator('#proposed-view [data-demo-model]').textContent()).endsWith(expected));
      assert.equal(await page.locator('[data-scenario][aria-pressed="true"]').count(), 1);
      assert.equal(await page.locator('#current-view [data-demo-prompt]').textContent(), await page.locator('#proposed-view [data-demo-prompt]').textContent());
      assert.equal(await page.locator('#current-view [data-demo-model]').textContent(), 'Codex · 모델 A');
    }
    await page.locator('#scenarios > summary').focus();
    await page.keyboard.press('Space');
    assert.equal(await page.locator('#scenarios').evaluate(el => el.open), false);
    for (const id of ['option-rules', 'option-llm', 'option-hybrid', 'execution-boundary', 'judge-transport', 'failure-policy', 'evaluation', 'evidence']) {
      const summary = page.locator('#' + id + ' > summary');
      await summary.focus();
      await page.keyboard.press('Enter');
      assert(await page.locator('#' + id).evaluate(el => el.open));
      await page.keyboard.press('Space');
      assert.equal(await page.locator('#' + id).evaluate(el => el.open), false);
    }
    await page.locator('#option-hybrid > summary').click();
    await page.screenshot({ path: path.join(screenshots, 'recommendation-expanded.png'), fullPage: true });
    await page.locator('#option-hybrid > summary').click();

    await page.setViewportSize({ width: 969, height: 822 });
    await page.evaluate(() => scrollTo(0, 0));
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    assert(await page.locator('.model-line.changed').evaluate(el => el.getBoundingClientRect().bottom < innerHeight));
    await page.screenshot({ path: path.join(screenshots, 'compact.png'), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await page.evaluate(() => scrollTo(0, 0));
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    const narrow = await page.locator('#proposed-view').evaluate(el => ({ needsScroll: el.scrollWidth > el.clientWidth, before: el.scrollLeft }));
    assert(narrow.needsScroll);
    await page.locator('#proposed-view').focus();
    await page.keyboard.press('ArrowRight');
    await page.waitForFunction(() => document.querySelector('#proposed-view').scrollLeft > 0);
    await page.screenshot({ path: path.join(screenshots, 'narrow.png'), fullPage: true });
    for (const view of ['#current-view', '#proposed-view']) {
      const rows = await page.locator(view + ' .pane').evaluateAll(panes => {
        const bands = new Map();
        for (const pane of panes) { const y = pane.getBoundingClientRect().top; bands.set(y, (bands.get(y) || 0) + 1); }
        return [...bands.values()];
      });
      assert.deepEqual(rows, [4, 3]);
    }
    assert.deepEqual(externalRequests, [], 'No page-initiated external requests');
    assert.deepEqual(errors, [], 'No browser console or page errors');
    console.log('PASS browser: desktop 1440/969, narrow 390, 4+3, quota, unchanged panes, no model controls');
    console.log('PASS interactions: 5 examples, Enter/Space disclosures, keyboard horizontal scrolling, reduced motion');
    console.log('PASS browser network/console: no external requests or errors');
    console.log(JSON.stringify({
      verified_at: new Date().toISOString(), artifact_sha256: sha(html),
      source_revision: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
      browser: await browser.version(), screenshots, external_requests: externalRequests, errors,
      prototype_only: true, actual_inference_calls: 0, production_model_changes: 0
    }, null, 2));
  } finally {
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
  }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
