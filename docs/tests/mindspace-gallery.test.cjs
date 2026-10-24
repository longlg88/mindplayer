'use strict';

// Run only after the HTML/assets owner announces a source freeze.
// MINDPLAYER_PLAYWRIGHT_MODULE=/existing/playwright \
//   node docs/tests/mindspace-gallery.test.cjs
// Optional MINDSPACE_EXPECTED_SOURCE_SHA256 pins the owner's announced HTML hash.
// Synthetic design checks only; never starts MindPlayer, a provider, or a CLI.
// Reads frozen source; writes only screenshots/JSON in its own temporary directory.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');

const htmlPath = path.resolve(__dirname, '../plans/2026-10-23-mindplayer-mindspace-gallery.ko.html');
const assetPaths = Array.from({ length: 5 }, (_, index) =>
  path.resolve(__dirname, `../plans/assets/mindspace-2026-10-23/office-0${index + 1}.png`));
const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-mindspace-gallery-'));
const sha256 = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const receipt = {
  scope: 'Synthetic HTML design only; not product, native TTY, provider, or task-success evidence',
  startedAt: new Date().toISOString(), finishedAt: null, passed: 0, failed: 0,
  artifacts, sourceHash: null, sources: [], cases: [], screenshots: [], images: [], geometry: [],
  consoleErrors: [], pageErrors: [], externalRequests: [], unexpectedRequests: [],
  failedRequests: [], badResponses: [], serverErrors: [],
  cleanup: { browser: false, server: false }
};
let browser, server, page;
let source;
const frozenFiles = new Map();

async function check(name, fn) {
  try {
    await fn();
    receipt.passed++;
    receipt.cases.push({ name, result: 'pass' });
  } catch (error) {
    receipt.failed++;
    receipt.cases.push({ name, result: 'fail', message: error.message, stack: error.stack });
  }
}

const state = () => page.evaluate(() => window.mindspaceDemo.getState());
const sceneImage = () => page.locator('img#sceneImage, #sceneImage img');
const sessionButtons = () => page.locator('.session-button[data-session-id]');
const buttonIds = () => sessionButtons().evaluateAll(nodes => nodes.map(node => node.dataset.sessionId));
const paneIds = () => page.locator('.native-pane').evaluateAll(nodes => nodes.map(node => node.dataset.sessionId));

async function waitForView(view) {
  await page.waitForFunction(expected => window.mindspaceDemo.getState().view === expected, view);
}

async function office() {
  for (let attempt = 0; attempt < 2 && (await state()).view !== 'office'; attempt++) {
    await page.keyboard.press('Escape');
  }
  await waitForView('office');
}

async function scenario(value) {
  await office();
  await page.locator('#scenario').selectOption(value);
  await page.waitForFunction(expected => window.mindspaceDemo.getState().scenario === expected, value);
}

async function idea(value) {
  await office();
  await page.locator(`.idea-tab[data-idea="${value}"]`).click();
  await page.waitForFunction(expected => Number(window.mindspaceDemo.getState().idea) === expected, value);
}

async function selectSession(id) {
  const index = (await buttonIds()).indexOf(id);
  assert.ok(index >= 0, `Session ${id} has no visible selection button`);
  await sessionButtons().nth(index).click();
  await page.waitForFunction(expected => window.mindspaceDemo.getState().selectedSessionId === expected, id);
}

async function reachSession(id) {
  const current = await state();
  const target = current.sessions.findIndex(session => session.id === id);
  let index = current.sessions.findIndex(session => session.id === current.selectedSessionId);
  assert.ok(target >= 0 && index >= 0, `Cannot navigate to session ${id}`);
  await page.locator('#app').focus();
  while (index !== target) {
    await page.keyboard.press(index < target ? 'ArrowRight' : 'ArrowLeft');
    index += index < target ? 1 : -1;
  }
  await selectSession(id);
}

async function sceneReady() {
  await sceneImage().waitFor({ state: 'visible' });
  await sceneImage().evaluate(image => {
    if (!image.complete || !image.naturalWidth) return image.decode();
  });
  // Let responsive geometry settle without introducing wall-clock sleeps.
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
}

async function screenshot(name, fullPage = false) {
  const file = path.join(artifacts, `${name}.png`);
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: file, fullPage, animations: 'disabled', caret: 'hide' });
  receipt.screenshots.push({ file, viewport: page.viewportSize(), fullPage });
}

function assertUniqueIds(ids, name) {
  assert.ok(ids.every(id => typeof id === 'string' && id.trim()), `${name}: IDs must be nonempty strings`);
  assert.equal(new Set(ids).size, ids.length, `${name}: duplicate session IDs`);
}

function assertSessionShape(session) {
  for (const key of ['id', 'provider', 'account', 'title', 'status']) {
    assert.ok(typeof session[key] === 'string' && session[key].trim(), `Session ${session.id}: missing ${key}`);
  }
}

function assertStatusText(text, status) {
  const labels = {
    running: /running|실행\s*중|진행\s*중|작업\s*중/i,
    working: /working|실행\s*중|진행\s*중|작업\s*중/i,
    idle: /idle|유휴|대기/i,
    blocked: /blocked|입력\s*(?:필요|대기)|승인\s*(?:필요|대기)|확인\s*필요|차단/i,
    completed: /completed|(?:응답|턴)\s*완료/i,
    unknown: /unknown|미확인|알\s*수\s*없/i,
    ended: /ended|종료/i
  };
  if (labels[status]) assert.match(text, labels[status], `Detail does not explicitly identify status ${status}`);
  else assert.ok(text.toLowerCase().includes(status.toLowerCase()), `Detail omits status ${status}`);
}

async function assertDetail(selected) {
  const text = await page.locator('#sessionDetail').innerText();
  for (const field of ['id', 'provider', 'account']) {
    assert.ok(text.toLowerCase().includes(selected[field].toLowerCase()), `Detail omits selected ${field}: ${selected[field]}`);
  }
  assertStatusText(text, selected.status);
}

async function assertQuota(view) {
  const geometry = await page.locator('#quota').evaluate(node => {
    const app = document.querySelector('#app');
    const style = getComputedStyle(app);
    const bounds = element => element.getBoundingClientRect().toJSON();
    return { quota: bounds(node), app: bounds(app), insideApp: app.contains(node),
      insetLeft: parseFloat(style.borderLeftWidth) + parseFloat(style.paddingLeft),
      insetRight: parseFloat(style.borderRightWidth) + parseFloat(style.paddingRight) };
  });
  receipt.geometry.push({ kind: 'quota', view, viewport: page.viewportSize(), ...geometry });
  assert.equal(geometry.insideApp, true, JSON.stringify(geometry));
  assert.ok(Math.abs(geometry.quota.left - (geometry.app.left + geometry.insetLeft)) <= 3, JSON.stringify(geometry));
  assert.ok(Math.abs(geometry.quota.right - (geometry.app.right - geometry.insetRight)) <= 3, JSON.stringify(geometry));
  assert.ok(geometry.quota.height > 0 && geometry.quota.bottom <= geometry.app.bottom + 1, JSON.stringify(geometry));
}

async function assertFits(label, includeImage = true) {
  const geometry = await page.evaluate(withImage => {
    const bounds = node => node ? node.getBoundingClientRect().toJSON() : null;
    const app = document.querySelector('#app');
    return { viewport: innerWidth, scroll: document.documentElement.scrollWidth,
      app: bounds(app), appClient: app.clientWidth, appScroll: app.scrollWidth,
      quota: bounds(document.querySelector('#quota')),
      scene: withImage ? bounds(document.querySelector('img#sceneImage, #sceneImage img')) : null,
      sessions: [...document.querySelectorAll('.session-button[data-session-id]')]
        .filter(node => node.getClientRects().length > 0).map(bounds) };
  }, includeImage);
  receipt.geometry.push({ kind: 'fit', label, viewport: page.viewportSize(), ...geometry });
  assert.ok(geometry.scroll <= geometry.viewport + 1, JSON.stringify(geometry));
  assert.ok(geometry.appScroll <= geometry.appClient + 1, JSON.stringify(geometry));
  for (const [name, rect] of Object.entries({ app: geometry.app, quota: geometry.quota,
    ...(includeImage ? { scene: geometry.scene } : {}) })) {
    assert.ok(rect && rect.width > 100 && rect.height > 0, `${name}: ${JSON.stringify(geometry)}`);
    assert.ok(rect.left >= -1 && rect.right <= geometry.viewport + 1, `${name}: ${JSON.stringify(geometry)}`);
  }
  for (const rect of geometry.sessions) {
    assert.ok(rect.width > 0 && rect.height > 0 && rect.left >= -1 && rect.right <= geometry.viewport + 1,
      `Session button is clipped: ${JSON.stringify(geometry)}`);
  }
}

async function main() {
  for (const file of [htmlPath, ...assetPaths]) {
    const bytes = fs.readFileSync(file);
    frozenFiles.set(file, bytes);
    receipt.sources.push({ file, bytes: bytes.length, before: sha256(bytes), after: null });
  }
  source = frozenFiles.get(htmlPath).toString('utf8');
  receipt.sourceHash = sha256(frozenFiles.get(htmlPath));
  if (process.env.MINDSPACE_EXPECTED_SOURCE_SHA256) {
    assert.equal(receipt.sourceHash, process.env.MINDSPACE_EXPECTED_SOURCE_SHA256, 'HTML differs from the announced freeze');
  }
  await check('inline JavaScript parses without executing source in Node', () => {
    const scripts = [...source.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script\s*>/gi)]
      .filter(match => !/\btype\s*=\s*["'](?:application\/ld\+json|application\/json)["']/i.test(match[1]));
    assert.ok(scripts.length > 0, 'No inline gallery behavior found');
    for (const match of scripts) {
      assert.doesNotMatch(match[1], /\bsrc\s*=/i, 'Gallery must be standalone');
      new vm.Script(match[2], { filename: path.basename(htmlPath) });
    }
  });
  for (const file of assetPaths) {
    await check(`${path.basename(file)} is a high-resolution PNG`, () => {
      const bytes = frozenFiles.get(file);
      assert.deepEqual(bytes.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
      assert.equal(bytes.toString('ascii', 12, 16), 'IHDR');
      assert.ok(bytes.readUInt32BE(16) >= 1024, 'PNG source width must be at least 1024');
    });
  }
  await check('five PNG source files contain distinct bytes', () => {
    assert.equal(new Set(assetPaths.map(file => sha256(frozenFiles.get(file)))).size, 5);
  });

  const routes = new Map([
    ['/', { bytes: frozenFiles.get(htmlPath), type: 'text/html; charset=utf-8' }],
    [`/${path.basename(htmlPath)}`, { bytes: frozenFiles.get(htmlPath), type: 'text/html; charset=utf-8' }],
    ...assetPaths.map(file => [`/assets/mindspace-2026-10-23/${path.basename(file)}`,
      { bytes: frozenFiles.get(file), type: 'image/png' }])
  ]);
  server = http.createServer((req, res) => {
    if (req.url === '/favicon.ico' && req.method === 'GET') { res.writeHead(204).end(); return; }
    const entry = routes.get(req.url);
    if (!entry || !['GET', 'HEAD'].includes(req.method)) {
      receipt.unexpectedRequests.push({ url: req.url, method: req.method, via: 'server' });
      res.writeHead(404).end(); return;
    }
    res.writeHead(200, { 'content-type': entry.type, 'cache-control': 'no-store',
      'content-length': entry.bytes.length, 'x-content-type-options': 'nosniff' });
    res.end(req.method === 'HEAD' ? undefined : entry.bytes);
  });
  server.on('error', error => receipt.serverErrors.push(error.message));
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => { server.removeListener('error', reject); resolve(); });
  });
  const origin = `http://127.0.0.1:${server.address().port}`;
  receipt.origin = origin;
  const { chromium } = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');
  browser = await chromium.launch({ channel: 'chrome', headless: true });
  receipt.runtime = { node: process.version, playwrightModule: process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright',
    chromeChannel: 'chrome', chromeVersion: browser.version() };
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 },
    reducedMotion: 'reduce', serviceWorkers: 'block', acceptDownloads: false });
  context.setDefaultTimeout(6000);
  context.setDefaultNavigationTimeout(15000);
  context.on('page', opened => {
    opened.on('pageerror', error => receipt.pageErrors.push(error.message));
    opened.on('console', message => { if (message.type() === 'error') receipt.consoleErrors.push(message.text()); });
    opened.on('requestfailed', request => receipt.failedRequests.push({ url: request.url(), error: request.failure()?.errorText }));
    opened.on('response', response => {
      if (response.status() >= 400) receipt.badResponses.push({ url: response.url(), status: response.status() });
    });
  });
  await context.route('**/*', async route => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.origin !== origin) {
      receipt.externalRequests.push({ url: url.href, type: request.resourceType() });
      await route.abort('blockedbyclient'); return;
    }
    const entry = routes.get(url.pathname);
    const expectedType = entry?.type === 'image/png' ? 'image' : 'document';
    if (url.search || url.hash || !['GET', 'HEAD'].includes(request.method()) ||
      (url.pathname !== '/favicon.ico' && (!entry || request.resourceType() !== expectedType))) {
      receipt.unexpectedRequests.push({ url: url.href, method: request.method(), type: request.resourceType(), via: 'browser' });
      await route.abort('blockedbyclient'); return;
    }
    await route.continue();
  });
  await context.routeWebSocket('**/*', async socket => {
    const url = new URL(socket.url());
    (url.origin === origin ? receipt.unexpectedRequests : receipt.externalRequests).push({ url: url.href, type: 'websocket' });
    await socket.close({ code: 1008, reason: 'Frozen HTML and PNG files only' });
  });
  page = await context.newPage();
  await page.goto(`${origin}/${path.basename(htmlPath)}`, { waitUntil: 'load' });
  await page.waitForFunction(() => typeof window.mindspaceDemo?.getState === 'function');
  await sceneReady();

  await check('gallery exposes the documented state and session identities', async () => {
    const current = await state();
    assert.equal(current.view, 'office');
    assert.equal(current.scenario, 'normal');
    assert.ok([1, 2, 3, 4, 5].includes(Number(current.idea)));
    assert.equal(current.sessions.length, 7);
    current.sessions.forEach(assertSessionShape);
    for (const field of ['nativePaneIds', 'visibleSessionIds']) {
      assert.ok(Array.isArray(current[field]), `Missing ${field}`);
      assertUniqueIds(current[field], field);
      assert.ok(current[field].every(id => current.sessions.some(session => session.id === id)), `${field} contains fabricated IDs`);
    }
    assert.ok(current.sessions.some(session => session.id === current.selectedSessionId));
  });
  await check('five accessible idea tabs and concept cards identify all five layouts', async () => {
    for (const selector of ['.idea-tab[data-idea]', '.concept-card[data-idea]']) {
      assert.deepEqual(await page.locator(selector).evaluateAll(nodes => nodes.map(node => node.dataset.idea).sort()), ['1', '2', '3', '4', '5']);
    }
    assert.deepEqual(await page.locator('.idea-tab').evaluateAll(nodes => nodes.map(node => node.tagName)), Array(5).fill('BUTTON'));
    assert.equal(await page.locator('.idea-tab[aria-pressed="true"]').count(), 1);
    assert.equal(await page.locator('.idea-tab[aria-pressed="false"]').count(), 4);
  });
  await check('the app is a keyboard-focusable region', async () => {
    assert.equal(await page.locator('#app').getAttribute('tabindex'), '0');
    assert.equal(await page.locator('#app').getAttribute('role'), 'region');
  });
  await check('scenario selector exposes all six synthetic fixtures', async () => {
    const options = await page.locator('#scenario option').evaluateAll(nodes => nodes.map(node => node.value));
    for (const value of ['normal', 'attention', 'completion', 'stale', 'empty', 'dense']) assert.ok(options.includes(value), `Missing ${value}`);
  });
  await check('loaded scene and thumbnail images are local high-resolution PNG artwork', async () => {
    const images = await page.locator('img[src]').evaluateAll(nodes => nodes.map(node => ({
      src: node.currentSrc, complete: node.complete, width: node.naturalWidth, height: node.naturalHeight
    })));
    assert.ok(images.length >= 6, 'Expected the main scene and five gallery previews');
    for (const image of images) {
      assert.equal(image.complete, true, JSON.stringify(image));
      assert.ok(image.width >= 1024 && image.height > 0, JSON.stringify(image));
      const url = new URL(image.src);
      assert.equal(url.origin, origin);
      assert.match(url.pathname, /^\/assets\/mindspace-2026-10-23\/office-0[1-5]\.png$/);
    }
  });

  for (let layout = 1; layout <= 5; layout++) {
    await check(`idea ${layout}: tab selects the matching layout`, async () => {
      await scenario('normal'); await idea(layout); await sceneReady();
      assert.equal(Number((await state()).idea), layout);
      assert.equal(await page.locator('.idea-tab[aria-pressed="true"]').getAttribute('data-idea'), String(layout));
      assert.equal(await page.locator('.idea-tab[aria-pressed="true"]').count(), 1);
    });
    await check(`idea ${layout}: concept card selects the matching layout`, async () => {
      await idea(layout === 5 ? 1 : layout + 1);
      await page.locator(`.concept-card[data-idea="${layout}"]`).click();
      assert.equal(Number((await state()).idea), layout);
    });
    await check(`idea ${layout}: scene decodes the corresponding local high-resolution PNG`, async () => {
      await idea(layout); await sceneReady();
      assert.equal(await sceneImage().count(), 1);
      const image = await sceneImage().evaluate(async node => {
        const canvas = document.createElement('canvas');
        canvas.width = node.naturalWidth; canvas.height = node.naturalHeight;
        const drawing = canvas.getContext('2d', { willReadFrequently: true });
        drawing.drawImage(node, 0, 0);
        const pixels = drawing.getImageData(0, 0, canvas.width, canvas.height).data;
        const digest = await crypto.subtle.digest('SHA-256', pixels);
        return { src: node.getAttribute('src'), currentSrc: node.currentSrc, complete: node.complete,
          naturalWidth: node.naturalWidth, naturalHeight: node.naturalHeight,
          pixelHash: [...new Uint8Array(digest)].map(value => value.toString(16).padStart(2, '0')).join('') };
      });
      receipt.images.push({ idea: layout, ...image });
      assert.equal(image.complete, true);
      assert.ok(image.naturalWidth >= 1024 && image.naturalHeight > 0, JSON.stringify(image));
      assert.ok(image.src && !/^(?:[a-z][a-z\d+.-]*:|\/)/i.test(image.src), `PNG src must be relative: ${image.src}`);
      assert.equal(new URL(image.currentSrc).origin, origin);
      assert.equal(new URL(image.currentSrc).pathname, `/assets/mindspace-2026-10-23/office-0${layout}.png`);
      assert.equal(new URL(image.src, page.url()).href, image.currentSrc);
    });
    await check(`idea ${layout}: office shows seven distinct selectable session IDs`, async () => {
      const current = await state(); const ids = await buttonIds();
      assert.equal(ids.length, 7); assertUniqueIds(ids, 'visible buttons');
      assert.deepEqual(ids, current.visibleSessionIds);
      assert.ok(ids.every(id => current.sessions.some(session => session.id === id)));
    });
    await check(`idea ${layout}: ArrowRight selects the next session identity`, async () => {
      const before = await state();
      const index = before.sessions.findIndex(session => session.id === before.selectedSessionId);
      await page.locator('#app').focus(); await page.keyboard.press('ArrowRight');
      assert.equal((await state()).selectedSessionId, before.sessions[(index + 1) % before.sessions.length].id);
    });
    await check(`idea ${layout}: ArrowLeft selects the preceding session identity`, async () => {
      const before = await state();
      const index = before.sessions.findIndex(session => session.id === before.selectedSessionId);
      await page.locator('#app').focus(); await page.keyboard.press('ArrowLeft');
      assert.equal((await state()).selectedSessionId, before.sessions[(index - 1 + before.sessions.length) % before.sessions.length].id);
    });
    await check(`idea ${layout}: clicking a session shows its ID, provider, account and status`, async () => {
      const selected = (await state()).sessions[2];
      await selectSession(selected.id); await assertDetail(selected);
    });
    await check(`idea ${layout}: Enter after a session click opens that native pane`, async () => {
      await office(); const selected = (await state()).sessions[2]; await selectSession(selected.id);
      await page.keyboard.press('Enter'); await waitForView('native');
      assert.equal(await page.locator('.native-pane.focused').count(), 1);
      assert.equal(await page.locator('.native-pane.focused').getAttribute('data-session-id'), selected.id);
      assert.equal((await state()).selectedSessionId, selected.id);
    });
    await check(`idea ${layout}: native view preserves the four-top, three-bottom composition`, async () => {
      if ((await state()).view !== 'native') await page.locator('#nativeButton').click();
      await waitForView('native');
      assert.equal(await page.locator('.native-row.top .native-pane').count(), 4);
      assert.equal(await page.locator('.native-row.bottom .native-pane').count(), 3);
      assert.deepEqual(await paneIds(), (await state()).nativePaneIds);
    });
    await check(`idea ${layout}: Escape returns to office with the selected identity`, async () => {
      const selected = (await state()).selectedSessionId;
      await page.keyboard.press('Escape'); await waitForView('office');
      assert.equal((await state()).selectedSessionId, selected);
    });
    for (const previousView of ['office', 'native']) {
      await check(`idea ${layout}: Escape from Ctrl-G trace restores ${previousView}`, async () => {
        await office();
        if (previousView === 'native') { await page.locator('#nativeButton').click(); await waitForView('native'); }
        const selected = (await state()).selectedSessionId;
        await page.locator('#app').focus(); await page.keyboard.press('Control+g'); await waitForView('trace');
        assert.equal((await state()).selectedSessionId, selected);
        await page.keyboard.press('Escape'); await waitForView(previousView);
        assert.equal((await state()).selectedSessionId, selected);
      });
    }
    await check(`idea ${layout}: quota spans the office app footer`, async () => {
      await office(); await assertQuota('office');
    });
    for (const width of [1440, 390]) {
      await check(`idea ${layout}: ${width}px layout fits the viewport`, async () => {
        await office(); await page.setViewportSize({ width, height: 1000 }); await sceneReady();
        await assertFits(`idea ${layout}`); await assertQuota('office');
      });
      await check(`idea ${layout}: ${width}px screenshot is captured`, async () => {
        await screenshot(`idea-0${layout}-${width}x1000`, width === 390);
      });
    }
    await page.setViewportSize({ width: 1440, height: 1000 });
  }
  await check('all five displayed scenes have distinct image sources and rendered pixels', () => {
    assert.equal(receipt.images.length, 5);
    assert.equal(new Set(receipt.images.map(image => image.src)).size, 5);
    assert.equal(new Set(receipt.images.map(image => image.pixelHash)).size, 5);
  });
  await check('five desktop layouts have distinct scene and session-list compositions', () => {
    const layouts = receipt.geometry.filter(entry => entry.kind === 'fit' &&
      entry.label.startsWith('idea ') && entry.viewport.width === 1440);
    assert.equal(layouts.length, 5);
    const signatures = layouts.map(layout => JSON.stringify([
      layout.scene.left - layout.app.left, layout.scene.width, layout.scene.height,
      layout.sessions[0].left - layout.app.left, layout.sessions[0].width,
      layout.sessions[0].top - layout.app.top
    ].map(value => Math.round(value))));
    assert.equal(new Set(signatures).size, 5, 'Changing artwork alone does not establish five different UI layouts');
  });

  await check('native button opens the selected session without reordering panes', async () => {
    await scenario('normal'); const before = await state(); const expected = before.nativePaneIds;
    assert.equal(expected.length, 7); assertUniqueIds(expected, 'native pane set');
    for (const selected of [before.sessions[0], before.sessions[4], before.sessions[6]]) {
      await selectSession(selected.id); await page.locator('#nativeButton').click(); await waitForView('native');
      assert.deepEqual(await paneIds(), expected);
      assert.deepEqual((await state()).nativePaneIds, expected);
      assert.equal(await page.locator('.native-pane.focused').getAttribute('data-session-id'), selected.id);
      await page.keyboard.press('Escape'); await waitForView('office');
    }
  });
  await check('native view retains the office quota content and full-width footer', async () => {
    await office(); const quota = await page.locator('#quota').innerText();
    await page.locator('#nativeButton').click(); await waitForView('native');
    assert.equal(await page.locator('#quota').innerText(), quota); await assertQuota('native');
    await screenshot('native-1440x1000');
  });
  await check('back button returns native view to office without changing selection', async () => {
    await office(); await page.locator('#nativeButton').click(); await waitForView('native');
    const selected = (await state()).selectedSessionId;
    await page.locator('#backButton').click(); await waitForView('office');
    assert.equal((await state()).selectedSessionId, selected);
  });
  await check('Escape after the native button restores office keyboard ownership', async () => {
    await scenario('normal'); const sessions = (await state()).sessions;
    await selectSession(sessions[2].id); await page.locator('#nativeButton').click(); await waitForView('native');
    await page.keyboard.press('Escape'); await waitForView('office');
    await page.keyboard.press('ArrowRight');
    assert.equal((await state()).selectedSessionId, sessions[3].id);
  });
  await check('back button restores native view after trace', async () => {
    await office(); await page.locator('#nativeButton').click(); await waitForView('native');
    const selected = (await state()).selectedSessionId;
    await page.locator('#app').focus(); await page.keyboard.press('Control+g'); await waitForView('trace');
    await screenshot('trace-1440x1000');
    await page.locator('#backButton').click(); await waitForView('native');
    assert.equal((await state()).selectedSessionId, selected);
  });

  await check('attention fixture explicitly identifies a session requiring input', async () => {
    await scenario('attention'); const current = await state();
    const selected = current.sessions.find(session => session.id === current.selectedSessionId);
    assert.equal(selected?.status, 'blocked'); await assertDetail(selected);
  });
  await check('stale fixture exposes unknown status instead of idle or completed', async () => {
    await scenario('stale'); const current = await state();
    const selected = current.sessions.find(session => session.id === current.selectedSessionId);
    assert.equal(selected?.status, 'unknown'); await assertDetail(selected);
    assert.match(await page.locator('#app').innerText(), /신호\s*지연|오래된\s*신호|stale/i);
    await screenshot('stale-1440x1000');
  });
  await check('completion fixture distinguishes response completion from task success', async () => {
    await scenario('completion'); const current = await state();
    const selected = current.sessions.find(session => session.id === current.selectedSessionId);
    assert.equal(selected?.status, 'completed'); await assertDetail(selected);
    const text = await page.locator('#app').innerText();
    assert.match(text, /(?:응답|턴)\s*완료/);
    assert.match(text, /작업\s*성공[^\n.!。]*(?:별도|아님|아니|보장|의미하지|미확인|확인\s*필요)|(?:별도|아님|아니|보장|의미하지|미확인)[^\n.!。]*작업\s*성공/);
  });

  await check('dense fixture retains 77 unique IDs even when titles are duplicated', async () => {
    await scenario('dense'); const current = await state();
    assert.equal(current.sessions.length, 77); current.sessions.forEach(assertSessionShape);
    assertUniqueIds(current.sessions.map(session => session.id), 'dense sessions');
    assert.ok(new Set(current.sessions.map(session => session.title)).size < 77, 'Fixture must exercise duplicate titles');
  });
  await check('dense fixture paginates all 77 session identities without duplicates or omissions', async () => {
    await scenario('dense'); const initial = await state();
    await reachSession(initial.sessions[0].id);
    const nativeOrder = [...initial.nativePaneIds];
    assert.equal(nativeOrder.length, 77); assertUniqueIds(nativeOrder, 'dense native order');
    const pages = new Map(); const visited = new Set();
    for (let index = 0; index < initial.sessions.length; index++) {
      const current = await state(); const visible = await buttonIds();
      assert.deepEqual(visible, current.visibleSessionIds);
      assert.ok(visible.length > 0 && visible.length <= 7, `Expected paged groups of at most seven: ${visible}`);
      assertUniqueIds(visible, 'dense page');
      assert.ok(visible.includes(current.selectedSessionId), 'Selected identity is missing from its page');
      assert.equal(current.selectedSessionId, initial.sessions[index].id, 'Arrow navigation lost identity/order');
      assert.equal(visited.has(current.selectedSessionId), false, 'Keyboard returned to a session before reaching all 77');
      visited.add(current.selectedSessionId);
      pages.set(JSON.stringify(visible), visible);
      assert.deepEqual(current.nativePaneIds, nativeOrder, 'Selection changed native pane membership/order');
      if (index < initial.sessions.length - 1) {
        await page.locator('#app').focus(); await page.keyboard.press('ArrowRight');
      }
    }
    const pagedIds = [...pages.values()].flat();
    assert.equal(pages.size, 11); assertUniqueIds(pagedIds, 'all dense pages');
    assert.deepEqual([...pagedIds].sort(), initial.sessions.map(session => session.id).sort());
    assert.equal(visited.size, 77); receipt.densePages = [...pages.values()];
    await screenshot('dense-last-page-1440x1000');
  });
  await check('duplicate session titles select separate identities and matching details', async () => {
    await scenario('dense'); const sessions = (await state()).sessions;
    const first = sessions.find((session, index) => sessions.some((other, otherIndex) =>
      otherIndex > index && other.title === session.title));
    assert.ok(first, 'Dense fixture contains no duplicate title');
    const second = sessions.find(session => session.id !== first.id && session.title === first.title);
    for (const selected of [first, second]) {
      await reachSession(selected.id);
      assert.equal((await state()).selectedSessionId, selected.id); await assertDetail(selected);
    }
  });
  await check('dense native view opens the selected fixed group without pulling first-page peers', async () => {
    await scenario('dense'); const before = await state();
    await reachSession(before.sessions[0].id);
    for (let index = 0; index < 8; index++) {
      await page.locator('#app').focus(); await page.keyboard.press('ArrowRight');
    }
    const selected = await state();
    assert.equal(selected.selectedSessionId, before.sessions[8].id);
    const index = before.nativePaneIds.indexOf(selected.selectedSessionId);
    assert.ok(index >= 7, 'Dense test must select a later pane group');
    const expected = before.nativePaneIds.slice(Math.floor(index / 7) * 7, Math.floor(index / 7) * 7 + 7);
    await page.keyboard.press('Enter'); await waitForView('native');
    assert.deepEqual(await paneIds(), expected);
    assert.equal(await page.locator('.native-pane.focused').getAttribute('data-session-id'), selected.selectedSessionId);
    assert.deepEqual((await state()).nativePaneIds, before.nativePaneIds);
  });

  await check('empty fixture fabricates no session identity or selected detail', async () => {
    await scenario('empty'); const current = await state();
    assert.deepEqual(current.sessions, []); assert.deepEqual(current.visibleSessionIds, []);
    assert.deepEqual(current.nativePaneIds, []); assert.equal(current.selectedSessionId, null);
    assert.equal(await sessionButtons().count(), 0); assert.equal(await page.locator('.native-pane[data-session-id]:visible').count(), 0);
    assert.equal(await page.locator('#nativeButton').isDisabled(), true);
    assert.match(await page.locator('#app').innerText(), /세션[^\n]*(?:없|0)|비어\s*있|empty/i);
  });
  await check('empty fixture cannot open a fabricated native session with Enter', async () => {
    await scenario('empty'); await page.locator('#app').focus(); await page.keyboard.press('Enter');
    assert.equal((await state()).view, 'office'); assert.equal((await state()).selectedSessionId, null);
  });
  await check('empty fixture cannot open a fabricated session trace with Ctrl-G', async () => {
    await scenario('empty'); await page.locator('#app').focus(); await page.keyboard.press('Control+g');
    assert.equal((await state()).view, 'office'); assert.equal((await state()).selectedSessionId, null);
  });
  await check('empty fixture discloses unavailable usage instead of inventing quota percentages', async () => {
    await scenario('empty'); const quota = await page.locator('#quota').innerText();
    assert.match(quota, /조회\s*(?:안|되지|불가)|미조회|미확인|알\s*수\s*없|측정\s*(?:없|안)|데이터\s*없|unknown|unavailable/i);
    assert.doesNotMatch(quota, /\d+(?:\.\d+)?\s*%/);
    await screenshot('empty-1440x1000');
  });
  await check('Tab can leave the app instead of trapping keyboard focus', async () => {
    await scenario('normal'); await sessionButtons().last().focus();
    let escaped = false;
    for (let attempt = 0; attempt < 30; attempt++) {
      await page.keyboard.press('Tab');
      if (await page.locator('#app').evaluate(node => !node.contains(document.activeElement))) { escaped = true; break; }
    }
    assert.equal(escaped, true);
  });
  for (const view of ['native', 'trace']) {
    await check(`${view} view fits the 390px viewport`, async () => {
      await scenario('normal'); await page.setViewportSize({ width: 390, height: 1000 });
      await page.locator('#nativeButton').click(); await waitForView('native');
      if (view === 'trace') { await page.locator('#app').focus(); await page.keyboard.press('Control+g'); await waitForView('trace'); }
      await assertFits(view, false); await assertQuota(view);
      await screenshot(`${view}-390x1000`, true);
    });
  }
  for (const value of ['attention', 'completion', 'stale', 'dense', 'empty']) {
    await check(`${value} fixture fits the 390px viewport`, async () => {
      await scenario(value); await page.setViewportSize({ width: 390, height: 1000 }); await sceneReady();
      await assertFits(value); await assertQuota('office');
    });
  }
}

main().catch(async error => {
  await check('harness setup and browser execution', () => { throw error; });
}).finally(async () => {
  // Attempt each cleanup independently; a close error must not leave the server alive.
  if (browser) await check('browser closes after the run', async () => {
    await browser.close(); receipt.cleanup.browser = !browser.isConnected();
    assert.equal(receipt.cleanup.browser, true);
  });
  if (server) await check('localhost server closes after the run', async () => {
    if (server.listening) await new Promise((resolve, reject) => {
      server.close(error => error ? reject(error) : resolve());
      server.closeAllConnections();
    });
    receipt.cleanup.server = !server.listening; assert.equal(receipt.cleanup.server, true);
  });
  await check('frozen HTML and PNG hashes are unchanged after browser execution', () => {
    assert.equal(receipt.sources.length, 6, 'Could not freeze all six source files');
    const changed = [];
    for (const entry of receipt.sources) {
      try {
        entry.after = sha256(fs.readFileSync(entry.file));
        if (entry.after !== entry.before) changed.push(entry.file);
      } catch (error) { entry.error = error.message; changed.push(entry.file); }
    }
    assert.deepEqual(changed, [], `Source changed during the run: ${changed.join(', ')}`);
  });
  for (const field of ['consoleErrors', 'pageErrors', 'externalRequests', 'unexpectedRequests',
    'failedRequests', 'badResponses', 'serverErrors']) {
    await check(`no ${field} during synthetic interactions`, () => assert.deepEqual(receipt[field], []));
  }
  receipt.finishedAt = new Date().toISOString();
  const validation = path.join(artifacts, 'validation.json');
  fs.writeFileSync(validation, `${JSON.stringify(receipt, null, 2)}\n`);
  console.log(JSON.stringify({ passed: receipt.passed, failed: receipt.failed, artifacts, validation,
    sourceHash: receipt.sourceHash, scope: receipt.scope, cleanup: receipt.cleanup,
    failures: receipt.cases.filter(test => test.result === 'fail') }, null, 2));
  process.exitCode = receipt.failed ? 1 : 0;
});
