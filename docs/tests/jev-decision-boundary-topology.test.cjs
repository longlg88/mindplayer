'use strict';

// Document-only QA. Reuse installed Playwright; never call Jev.
// MINDPLAYER_PLAYWRIGHT_MODULE=/absolute/path/playwright node docs/tests/jev-decision-boundary-topology.test.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { pathToFileURL } = require('node:url');
const { execFileSync } = require('node:child_process');
const { chromium } = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');
const root = path.resolve(__dirname, '../..');
const artifact = path.join(root, 'docs/report/2026-10-16-jev-decision-boundary-topology.ko.html');

async function main() {
  const html = fs.readFileSync(artifact, 'utf8');
  const sha = data => crypto.createHash('sha256').update(data).digest('hex');
  const sha256 = sha(html);
  assert.match(html, /<html lang="ko">/);
  assert(!/<(?:script|img|iframe|link)\b[^>]*(?:src|href)\s*=/i.test(html), 'no runtime dependencies');
  assert(!/@import|url\s*\(|on(?:click|load|error)\s*=/i.test(html), 'no imported CSS, assets or handlers');
  assert(!/\bsk-(?:proj-)?[A-Za-z0-9_-]{16,}|\bAKIA[A-Z0-9]{16}|-----BEGIN (?:RSA |OPENSSH )?PRIVATE KEY-----/.test(html), 'no credential patterns');
  assert(!/\b[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}\b/i.test(html), 'no personal email');
  const ids = [...html.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
  assert.equal(ids.length, new Set(ids).size, 'unique IDs');
  for (const match of html.matchAll(/href="([^"]+)"/g)) {
    const href = match[1];
    if (href.startsWith('#')) assert(ids.includes(href.slice(1)), `anchor: ${href}`);
    else if (!href.startsWith('https://')) assert(fs.statSync(path.resolve(path.dirname(artifact), href)).isFile(), `local reference: ${href}`);
  }
  for (const expected of ['판단은 Jev', 'model + effort', '193', 'previousDecision', 'Gateway', '최저 비용', '실제 Jev 판단 정확도 미검증']) assert(html.includes(expected), `required content: ${expected}`);
  console.log('PASS static: standalone Korean HTML, unique IDs, local links, credential scan, evidence labels');
  const screenshots = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-jev-topology-'));
  const browser = await chromium.launch({ executablePath: process.env.MINDPLAYER_CHROME_BINARY || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1100 }, reducedMotion: 'reduce' });
    const page = await context.newPage();
    const errors = [], externalRequests = [];
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    await context.route(/^https?:/, route => { externalRequests.push(route.request().url()); return route.abort(); });
    await page.goto(pathToFileURL(artifact).href, { waitUntil: 'load' });
    assert.equal(await page.locator('.map .node').count(), 8);
    assert.equal(await page.locator('h1').textContent(), '기준은 MindPlayer.판단은 Jev. 실행은 기존 세션.');
    for (const id of ['rubric', 'examples', 'alternatives', 'context', 'guards', 'evidence', 'references']) {
      const summary = page.locator(`#${id}>summary`);
      const initial = await page.locator(`#${id}`).evaluate(el => el.open);
      await summary.focus(); await page.keyboard.press('Enter');
      assert.equal(await page.locator(`#${id}`).evaluate(el => el.open), !initial, `keyboard toggle: ${id}`);
      await page.keyboard.press('Space');
      assert.equal(await page.locator(`#${id}`).evaluate(el => el.open), initial);
    }
    await page.locator('#examples>summary').click();
    assert.equal(await page.locator('#examples .example').count(), 6);
    await page.locator('#examples>summary').click();
    const layouts = [];
    for (const width of [1440, 969, 390, 320]) {
      await page.setViewportSize({ width, height: width > 700 ? 1100 : 844 });
      await page.evaluate(() => scrollTo(0, 0));
      const layout = await page.evaluate(() => ({ width: innerWidth, documentWidth: document.documentElement.scrollWidth,
        nodes: [...document.querySelectorAll('.map .node')].map(el => {
          const r = el.getBoundingClientRect(), boundary = el.closest('.boundary').getBoundingClientRect();
          // Arrows intentionally extend into the gap; inspect text, not pseudo-element overflow.
          return { id: el.id, x: r.x, y: r.y, right: r.right, bottom: r.bottom,
            boundaryContains: r.x >= boundary.x && r.right <= boundary.right && r.y >= boundary.y && r.bottom <= boundary.bottom,
            textFits: [...el.querySelectorAll('h3,p')].every(text => text.scrollWidth <= text.clientWidth + 1) };
        }) }));
      assert(layout.documentWidth <= width, `document fits at ${width}`);
      for (const node of layout.nodes) {
        assert(node.x >= 0 && node.right <= width, `${node.id} fits at ${width}`);
        assert(node.textFits, `${node.id} text fits at ${width}`);
        assert(node.boundaryContains, `${node.id} stays inside its ownership boundary at ${width}`);
      }
      for (let a = 0; a < layout.nodes.length; a++) for (let b = a + 1; b < layout.nodes.length; b++) {
        const x = layout.nodes[a], y = layout.nodes[b];
        assert(!(Math.min(x.right, y.right) > Math.max(x.x, y.x) + 1 && Math.min(x.bottom, y.bottom) > Math.max(x.y, y.y) + 1), `no overlap ${x.id}/${y.id} at ${width}`);
      }
      layouts.push({ width, documentWidth: layout.documentWidth, primaryNodes: layout.nodes.length });
      if ([1440, 390].includes(width)) {
        await page.screenshot({ path: path.join(screenshots, `${width}-top.png`) });
        await page.locator('#topology').screenshot({ path: path.join(screenshots, `${width}-map.png`) });
      }
    }
    await page.evaluate(() => document.querySelectorAll('details').forEach(el => { el.open = true; }));
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    const table = page.locator('#alternatives .table-wrap');
    assert(await table.evaluate(el => el.scrollWidth > el.clientWidth), 'comparison table scrolls locally');
    await table.focus(); await page.keyboard.press('ArrowRight');
    await page.waitForFunction(() => document.querySelector('#alternatives .table-wrap').scrollLeft > 0);
    assert.deepEqual(errors, []); assert.deepEqual(externalRequests, []);
    assert.equal(sha(fs.readFileSync(artifact)), sha256, 'stable artifact');
    console.log('PASS browser: 1440/969/390/320, 8 nodes, no overlaps/clipping/document overflow, offline');
    console.log('PASS interaction: 7 keyboard disclosures, 6 examples, comparison keyboard scroll');
    console.log(JSON.stringify({ verifiedAt: new Date().toISOString(), artifact: path.relative(root, artifact), sha256, sourceRevision: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(), browser: await browser.version(), screenshots, layouts, consoleErrors: errors, externalRequests, actualJevCalls: 0, applicationCodeChanged: false }, null, 2));
  } finally { await browser.close(); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
