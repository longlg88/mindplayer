'use strict';

// Documentation-only checks. Reuse an existing Playwright installation.
// No application launch, credentials, Jev/provider requests, or source writes.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { pathToFileURL } = require('node:url');
const { chromium } = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');
const root = path.resolve(__dirname, '../..');
const artifact = path.join(root, 'docs/report/2026-10-18-jev-code-guide.ko.html');
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');

async function main() {
  const html = fs.readFileSync(artifact, 'utf8');
  const originalHash = sha(html);
  assert.match(html, /<html lang="ko">/);
  assert(!/<(?:script|img|iframe|link)\b[^>]*(?:src|href)\s*=/i.test(html), 'no external assets');
  assert(!/@import|url\s*\(/i.test(html), 'no imported CSS or assets');
  assert(!/\bsk-(?:proj-)?[A-Za-z0-9_-]{16,}|\bAKIA[A-Z0-9]{16}|-----BEGIN (?:RSA |OPENSSH )?PRIVATE KEY-----/.test(html), 'no credential patterns');
  assert(!/\/Users\/|mp-smoke-\d|[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}/i.test(html), 'no personal paths, fixture IDs or email');
  const screenshots = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-jev-code-guide-'));
  const browser = await chromium.launch({ executablePath: process.env.MINDPLAYER_CHROME_BINARY || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1100 }, reducedMotion: 'reduce' });
    const page = await context.newPage();
    const errors = [], externalRequests = [];
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    await context.route(/^https?:/, route => { externalRequests.push(route.request().url()); return route.abort(); });
    await page.goto(pathToFileURL(artifact).href, { waitUntil: 'load' });
    const ids = await page.locator('[id]').evaluateAll(nodes => nodes.map(node => node.id));
    assert.equal(ids.length, new Set(ids).size, 'unique IDs');
    for (const href of await page.locator('a[href]').evaluateAll(nodes => nodes.map(node => node.getAttribute('href')))) {
      if (href.startsWith('#')) assert(ids.includes(href.slice(1)), `anchor ${href}`);
      else {
        assert(!/^[a-z]+:/i.test(href), `local-only source link ${href}`);
        assert(fs.statSync(path.resolve(path.dirname(artifact), href)).isFile(), `file exists ${href}`);
      }
    }
    const embedded = [...fs.readFileSync(path.join(root, 'crates/mindplayer-tui/src/local_router.rs'), 'utf8').matchAll(/include_str!\("\.\.\/\.\.\/\.\.\/tools\/local-model-router\/([^"/]+)"\)/g)].map(match => match[1]).sort();
    const documented = await page.locator('[data-asset]').evaluateAll(nodes => nodes.map(node => node.dataset.asset).sort());
    assert.equal(embedded.length, 19);
    assert.deepEqual(documented, embedded, 'one explanation for each embedded module');
    const excerpts = await page.locator('code[data-source]').evaluateAll(nodes => nodes.map(node => ({ source: node.dataset.source, lines: node.dataset.lines, text: node.textContent })));
    for (const excerpt of excerpts) {
      const [start, end] = excerpt.lines.split(':').map(Number);
      const expected = fs.readFileSync(path.join(root, excerpt.source), 'utf8').split('\n').slice(start - 1, end).join('\n');
      assert.equal(excerpt.text, expected, `exact source excerpt ${excerpt.source}:${excerpt.lines}`);
    }
    assert(excerpts.length >= 6);
    assert.equal(await page.locator('details.file').count(), 22);
    await page.locator('#file-search').fill('claude-broker.mjs');
    assert(await page.locator('details.file:visible').count() > 0, 'search finds matching consumer text');
    await page.locator('#file-search').fill('not-a-real-module-zzzz');
    assert.equal(await page.locator('details.file:visible').count(), 0);
    assert.match(await page.locator('#file-count').textContent(), /^0 \/ 22/);
    await page.locator('#file-search').fill('');
    await page.locator('#toggle-files').click();
    assert.equal(await page.locator('details.file[open]').count(), 22, 'expand all');
    await page.locator('#toggle-files').click();
    assert.equal(await page.locator('details.file[open]').count(), 0, 'collapse all');
    const summary = page.locator('details.file').first().locator('summary');
    await summary.focus(); await page.keyboard.press('Enter');
    assert(await page.locator('details.file').first().evaluate(node => node.open), 'keyboard opens disclosure');
    await page.keyboard.press('Space');
    assert(!(await page.locator('details.file').first().evaluate(node => node.open)), 'keyboard closes disclosure');
    const layouts = [];
    for (const width of [1440, 1024, 740, 390, 320]) {
      await page.setViewportSize({ width, height: width > 740 ? 1100 : 844 });
      await page.evaluate(() => { document.querySelectorAll('details').forEach(node => { node.open = false; }); scrollTo(0, 0); });
      const check = async state => {
        const metrics = await page.evaluate(() => ({ width: innerWidth, documentWidth: document.documentElement.scrollWidth,
          clippedText: [...document.querySelectorAll('h1,h2,h3,p,summary,.step,.family-model')].filter(node => node.getClientRects().length && !node.closest('table') && node.scrollWidth > node.clientWidth + 1).map(node => node.tagName + ':' + node.textContent.slice(0, 50)) }));
        assert(metrics.documentWidth <= width, `document fits ${width}, ${state}`);
        assert.deepEqual(metrics.clippedText, [], `text fits ${width}, ${state}`);
        return { ...metrics, state };
      };
      layouts.push(await check('collapsed'));
      if ([1440, 390].includes(width)) {
        await page.screenshot({ path: path.join(screenshots, `${width}-top.png`) });
        await page.locator('#flow').screenshot({ path: path.join(screenshots, `${width}-flow.png`) });
        await page.locator('#contracts').screenshot({ path: path.join(screenshots, `${width}-code.png`) });
      }
      await page.evaluate(() => document.querySelectorAll('details').forEach(node => { node.open = true; }));
      layouts.push(await check('expanded'));
    }
    assert.deepEqual(errors, []); assert.deepEqual(externalRequests, []);
    const reference = path.join(root, 'docs/report/2026-10-16-jev-decision-boundary-topology.ko.html');
    await page.setViewportSize({ width: 1440, height: 1100 });
    await page.goto(pathToFileURL(reference).href, { waitUntil: 'load' });
    await page.screenshot({ path: path.join(screenshots, 'reference-report-top.png') });
    assert.equal(sha(fs.readFileSync(artifact)), originalHash, 'artifact unchanged by QA');
    console.log(JSON.stringify({ verifiedAt: new Date().toISOString(), artifact: path.relative(root, artifact), sha256: originalHash,
      browser: await browser.version(), sourceExcerpts: excerpts.length, documentedRuntimeAssets: documented.length, fileGroups: 22,
      localLinks: 'all exist', interactions: ['search results/empty/reset', 'expand/collapse all', 'keyboard Enter/Space'],
      layouts, screenshots, consoleErrors: errors, externalRequests, actualProviderCalls: 0, applicationLaunched: false }, null, 2));
  } finally { await browser.close(); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
