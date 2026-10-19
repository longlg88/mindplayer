'use strict';

// HTML 시안만 검증한다. 실제 앱 / PTY / CLI / 계정 / provider 검증이 아니다.
// HTML 작성자가 freeze를 알린 뒤에만 브라우저 검증을 실행한다.
// MINDPLAYER_PLAYWRIGHT_MODULE=/existing/playwright node docs/tests/session-tasks-category-gallery.test.cjs
// node docs/tests/session-tasks-category-gallery.test.cjs --syntax-only
// 선택 사항: MINDPLAYER_GALLERY_FROZEN_SHA256=<작성자가 고정한 HTML 해시>
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');

const relativeTest = 'docs/tests/session-tasks-category-gallery.test.cjs';
const relativeHtml = 'docs/plans/2026-10-18-session-tasks-category-gallery.ko.html';
const htmlPath = path.resolve(__dirname, '../..', relativeHtml);
const ids = Array.from({ length: 10 }, (_, index) => index + 1);
const groups = ['a', 'b'];
const categoryIds = ['ops', 'dev', 'docs', 'none'];
const viewports = [1440, 1280, 768, 390, 320];
const sha256 = value => crypto.createHash('sha256').update(value).digest('hex');
const copy = value => JSON.parse(JSON.stringify(value));
const records = state => ({ sessions: state.sessions, tasks: state.tasks });
const graph = state => state.tasks.map(task => ({ id: task.id, links: [...task.links].sort() }));
const identity = state => state.sessions.map(({ id, title, agent, native }) => ({ id, title, agent, native }));
const initialPhase = (group, id) => group === 'b' ? ({ 5: 'pick', 7: 'confirm', 8: 'moved' }[id] || 'browse') : 'browse';

// Receipts contain test names, counts, measurements and hashes, never DOM text,
// model snapshots, console messages, request URLs, credentials or source snippets.
function failureMetadata(error, test) {
  const failure = { test, name: error.name || 'Error', code: error.code || null };
  if (error.operator) failure.operator = error.operator;
  for (const field of ['actual', 'expected']) {
    if (typeof error[field] === 'number' || typeof error[field] === 'boolean') failure[field] = error[field];
  }
  const location = String(error.stack || '').match(/session-tasks-category-gallery\.test\.cjs:(\d+):(\d+)/);
  if (location) failure.location = { line: Number(location[1]), column: Number(location[2]) };
  return failure;
}

function assertState(state, group, id) {
  assert.equal(state.idea, id);
  assert.ok(Array.isArray(state.sessions) && state.sessions.length >= 2);
  assert.ok(Array.isArray(state.tasks) && state.tasks.length >= 2);
  assert.equal(new Set(state.sessions.map(session => session.id)).size, state.sessions.length);
  assert.equal(new Set(state.tasks.map(task => task.id)).size, state.tasks.length);
  for (const session of state.sessions) {
    for (const field of ['id', 'title', 'agent', 'category', 'native']) assert.ok(Object.hasOwn(session, field));
    assert.ok(String(session.id).length > 0 && session.title.length > 0);
    assert.ok(categoryIds.includes(session.category));
  }
  const sessionIds = new Set(state.sessions.map(session => session.id));
  for (const task of state.tasks) {
    for (const field of ['id', 'title', 'priority', 'due', 'status', 'links']) assert.ok(Object.hasOwn(task, field));
    assert.ok(String(task.id).length > 0 && task.title.length > 0);
    assert.ok(Array.isArray(task.links));
    assert.equal(new Set(task.links).size, task.links.length);
    assert.ok(task.links.every(id => sessionIds.has(id)));
  }
  assert.ok(Number.isInteger(state.selectedSession) && state.selectedSession >= 0 && state.selectedSession < state.sessions.length);
  assert.ok(Number.isInteger(state.selectedTask) && state.selectedTask >= 0 && state.selectedTask < state.tasks.length);
  assert.ok(['sessions', 'tasks'].includes(state.focus));
  assert.equal(state.phase, initialPhase(group, id));
}

async function main() {
  const started = Date.now();
  const syntaxOnly = process.argv.includes('--syntax-only');
  const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-session-tasks-category-gallery-'));
  const receiptPath = path.join(artifacts, 'validation.json');
  const receipt = {
    schemaVersion: 1, result: 'failed', mode: syntaxOnly ? 'syntax-only' : 'browser',
    verifiedAt: new Date().toISOString(), artifacts, receiptPath,
    scope: 'standalone HTML proposal; synthetic browser interactions only',
    harness: { file: relativeTest, sha256: sha256(fs.readFileSync(__filename)) },
    source: { file: relativeHtml }, cases: [], compositions: [], screenshots: [], geometry: [],
    ideaCounts: { a: 10, b: 10 }, viewports, keyboardEvents: 0,
    network: { allowedRequests: 0, blockedRequests: [], failedRequests: [], badResponses: [], webSockets: 0 },
    console: { messages: 0, errors: 0, warnings: 0, pageErrors: 0 },
    cleanup: { browser: 'not-launched', server: 'not-started' },
    actualProviderCalls: 0, productionCodeChanged: false, installed: false,
    notVerified: ['real terminal rendering and IME', 'Rust/CLI/provider integration',
      'production persistence and auto-sync', 'screen-reader behavior', 'subjective visual quality',
      'clearing a nonempty folded fixture: no public setter or fold shortcut'],
  };
  let currentCase = 'setup';
  let source;
  let browser;
  let server;
  let successful = false;
  const emit = value => console.log(JSON.stringify(value));
  async function test(name, run) {
    currentCase = name;
    const caseStarted = Date.now();
    try {
      await run();
      receipt.cases.push({ name, result: 'passed', elapsedMs: Date.now() - caseStarted });
      emit({ test: name, result: 'passed' });
    } catch (error) {
      receipt.cases.push({ name, result: 'failed', elapsedMs: Date.now() - caseStarted });
      throw error;
    }
  }

  try {
    assert.ok(process.argv.slice(2).every(argument => argument === '--syntax-only'), 'supported arguments only');
    await test('harness JavaScript parses without starting a browser or server', () => {
      new vm.Script(fs.readFileSync(__filename, 'utf8'), { filename: relativeTest });
    });
    if (syntaxOnly) {
      receipt.notVerified.unshift('all HTML static and browser assertions: waiting for author freeze');
      successful = true;
      return;
    }

    currentCase = 'load frozen HTML';
    source = fs.readFileSync(htmlPath, 'utf8');
    receipt.source.sha256 = sha256(source);
    const expectedHash = process.env.MINDPLAYER_GALLERY_FROZEN_SHA256;
    if (expectedHash) {
      assert.match(expectedHash, /^[a-f\d]{64}$/i);
      assert.equal(receipt.source.sha256, expectedHash.toLowerCase(), 'author freeze hash');
      receipt.source.authorFreezeMatched = true;
    }
    const scripts = [...source.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/gi)];
    await test('HTML declares Korean document language', () => {
      assert.match(source, /<html\b[^>]*\blang=["']ko["']/i);
    });
    await test('Korean source contains no stray Japanese copy', () => {
      assert.doesNotMatch(source, /[\u3040-\u30ff]/u);
    });
    await test('all interaction scripts are inline and parse', () => {
      assert.ok(scripts.length > 0);
      scripts.forEach(([whole, attributes, script], index) => {
        assert.doesNotMatch(attributes, /\bsrc\s*=/i);
        new vm.Script(script, { filename: `${relativeHtml}:${index}` });
      });
    });
    await test('HTML loads no external assets or CSS', () => {
      assert.doesNotMatch(source, /<(?:script|link|img|iframe|source|video|audio|object|embed)\b[^>]+(?:src|href|data)\s*=\s*["']\s*(?:https?:)?\/\//i);
      assert.doesNotMatch(source, /@import\b|url\(\s*["']?\s*(?:https?:)?\/\//i);
    });
    await test('interaction scripts use no transport or persistent storage APIs', () => {
      assert.doesNotMatch(scripts.map(script => script[2]).join('\n'),
        /\b(?:fetch|XMLHttpRequest|WebSocket|WebTransport|EventSource|Worker|SharedWorker|serviceWorker|sendBeacon|localStorage|sessionStorage|indexedDB|CacheStorage|caches)\b|document\s*\.\s*cookie/);
    });
    await test('HTML contains no credential or private-key patterns', () => {
      assert.doesNotMatch(source,
        /(?:sk-[A-Za-z\d_-]{18,}|vck_[A-Za-z\d_-]{15,}|Bearer\s+[A-Za-z\d._-]{20,}|AKIA[A-Z\d]{16}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----)/);
    });

    currentCase = 'launch installed Playwright with Chrome';
    const modulePath = process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright';
    const { chromium } = require(modulePath);
    receipt.playwright = { module: modulePath };
    server = http.createServer((request, response) => {
      if (request.method === 'GET' && request.url === '/gallery') {
        response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
        response.end(source);
      } else if (request.method === 'GET' && request.url === '/favicon.ico') {
        response.writeHead(204); response.end();
      } else {
        response.writeHead(404); response.end();
      }
    });
    await new Promise((resolve, reject) => {
      const failed = error => reject(error);
      server.once('error', failed);
      server.listen(0, '127.0.0.1', () => { server.removeListener('error', failed); resolve(); });
    });
    receipt.cleanup.server = 'listening';
    const base = `http://127.0.0.1:${server.address().port}`;
    const allowed = new Set([`${base}/gallery`, `${base}/favicon.ico`]);
    browser = await chromium.launch({ channel: 'chrome', headless: true });
    receipt.cleanup.browser = 'open';
    receipt.browser = { channel: 'chrome', version: browser.version(), headless: true };
    const context = await browser.newContext({
      viewport: { width: 1440, height: 1050 }, reducedMotion: 'reduce', serviceWorkers: 'block',
    });
    const requestMetadata = request => ({
      urlSha256: sha256(request.url()), method: request.method(), resourceType: request.resourceType(),
    });
    await context.route('**/*', route => {
      const request = route.request();
      if (request.method() === 'GET' && allowed.has(request.url())) {
        receipt.network.allowedRequests++;
        return route.continue();
      }
      receipt.network.blockedRequests.push(requestMetadata(request));
      return route.abort('blockedbyclient');
    });
    // Installed Playwright's WebSocket routing does not connect upstream unless
    // connectToServer() is called. Never call it in this HTML-only harness.
    await context.routeWebSocket('**/*', socket => {
      receipt.network.webSockets++;
      return socket.close({ code: 1008, reason: 'HTML-only gallery' });
    });
    context.on('requestfailed', request => receipt.network.failedRequests.push(requestMetadata(request)));
    context.on('response', response => {
      if (response.status() >= 400) receipt.network.badResponses.push({ status: response.status(), urlSha256: sha256(response.url()) });
    });
    const page = await context.newPage();
    page.setDefaultTimeout(5000);
    page.on('pageerror', () => { receipt.console.pageErrors++; });
    page.on('console', message => {
      receipt.console.messages++;
      if (message.type() === 'error') receipt.console.errors++;
      if (message.type() === 'warning') receipt.console.warnings++;
    });
    await page.goto(`${base}/gallery`, { waitUntil: 'load' });
    await page.waitForFunction(() => {
      const api = window.MindPlayerDesignGallery;
      return api && ['select', 'reset', 'snapshot'].every(name => typeof api[name] === 'function');
    });
    const section = group => page.locator(`#section-${group}`);
    const terminal = group => page.locator(`#terminal-${group}`);
    const buttons = group => section(group).locator(`button[data-group="${group}"][data-idea]`);
    const button = (group, id) => section(group).locator(`button[data-group="${group}"][data-idea="${id}"]`);
    const snapshot = group => page.evaluate(group => window.MindPlayerDesignGallery.snapshot(group), group);
    const text = group => terminal(group).locator('.line').evaluateAll(lines => lines.map(line => line.textContent).join('\n'));
    const key = async (group, value) => { receipt.keyboardEvents++; await terminal(group).press(value); };
    const press = async value => { receipt.keyboardEvents++; await page.keyboard.press(value); };
    async function prepare(group, id) {
      await page.evaluate(({ group, id }) => {
        window.MindPlayerDesignGallery.select(group, id);
        window.MindPlayerDesignGallery.reset(group);
      }, { group, id });
      assert.equal((await snapshot(group)).idea, id);
      await terminal(group).focus();
      return snapshot(group);
    }
    async function focusPane(group, target) {
      if ((await snapshot(group)).focus !== target) await key(group, 'F6');
      assert.equal((await snapshot(group)).focus, target);
    }
    async function selectIndex(group, pane, target) {
      await focusPane(group, pane);
      const field = pane === 'sessions' ? 'selectedSession' : 'selectedTask';
      const limit = (await snapshot(group))[pane].length;
      for (let steps = 0; steps < limit && (await snapshot(group))[field] !== target; steps++) await key(group, 'ArrowDown');
      assert.equal((await snapshot(group))[field], target);
    }
    async function setLinked(sessionIndex, taskIndex) {
      await selectIndex('a', 'sessions', sessionIndex);
      await selectIndex('a', 'tasks', taskIndex);
      const state = await snapshot('a');
      if (!state.tasks[taskIndex].links.includes(state.sessions[sessionIndex].id)) await key('a', 'F7');
    }
    async function picker(id) {
      const initial = await prepare('b', id);
      await key('b', 't');
      const state = await snapshot('b');
      assert.equal(state.phase, 'pick');
      assert.deepEqual(records(state), records(initial));
      assert.ok(Number.isInteger(state.target) && state.target >= 0 && state.target < categoryIds.length);
      assert.notEqual(categoryIds[state.target], state.sessions[state.selectedSession].category);
      return state;
    }
    async function destination(id) {
      const before = await picker(id);
      const from = before.sessions[before.selectedSession].category;
      const to = categoryIds[(categoryIds.indexOf(from) + 1 + id % 3) % categoryIds.length];
      const target = categoryIds.indexOf(to);
      for (let steps = 0; steps < categoryIds.length && (await snapshot('b')).target !== target; steps++) await key('b', 'ArrowDown');
      assert.equal((await snapshot('b')).target, target);
      return { before, to };
    }
    async function moved(id) {
      const { before, to } = await destination(id);
      await key('b', 'Enter');
      assert.equal((await snapshot('b')).phase, 'confirm');
      assert.deepEqual(records(await snapshot('b')), records(before));
      await key('b', 'Enter');
      const after = await snapshot('b');
      assert.equal(after.phase, 'moved');
      const selected = before.sessions[before.selectedSession];
      const expected = copy(records(before));
      expected.sessions[before.selectedSession].category = to;
      assert.deepEqual(records(after), expected);
      assert.deepEqual(after.lastMove, { id: selected.id, from: selected.category, to });
      assert.deepEqual(identity(after), identity(before));
      return { before, after, to };
    }
    async function screenshot(name, target, metadata = {}) {
      if (target !== page) await target.scrollIntoViewIfNeeded();
      await page.evaluate(async () => {
        await document.fonts.ready;
        await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      });
      const output = path.join(artifacts, `${name}.png`);
      assert.equal(fs.existsSync(output), false, 'fresh screenshot path');
      const png = await target.screenshot({ path: output, animations: 'disabled' });
      assert.ok(png.length > 100);
      assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
      const width = png.readUInt32BE(16), height = png.readUInt32BE(20);
      assert.ok(width > 0 && height > 0);
      const result = { name, file: path.basename(output), bytes: png.length, width, height, sha256: sha256(png), ...metadata };
      receipt.screenshots.push(result);
      return result;
    }
    async function assertGrid(group, id, record = false) {
      const grid = await terminal(group).evaluate(node => {
        const lines = [...node.querySelectorAll('.line')];
        return {
          role: node.getAttribute('role'), tabindex: node.getAttribute('tabindex'),
          cellWidth: Number.parseFloat(getComputedStyle(node).getPropertyValue('--cw')),
          widgets: node.querySelectorAll('input,button,select,textarea,a,[contenteditable="true"],[role="button"]').length,
          characterOnly: [...node.children].every(line => line.matches('.line') && [...line.children].every(run =>
            run.tagName === 'SPAN' && run.matches('.run') && !run.children.length)),
          rows: lines.map(line => ({
            text: line.textContent,
            runs: [...line.children].map(run => {
              const match = run.style.width.match(/^calc\(var\(--cw\)\s*\*\s*(\d+)\)$/);
              const bounds = run.getBoundingClientRect();
              return { cells: match ? Number(match[1]) : null,
                dataCells: run.hasAttribute('data-cells') ? Number(run.dataset.cells) : null,
                x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height };
            }),
          })),
        };
      });
      assert.equal(grid.role, 'application');
      assert.equal(grid.tabindex, '0');
      assert.equal(grid.widgets, 0);
      assert.equal(grid.characterOnly, true);
      assert.equal(grid.rows.length, 34);
      const rows = grid.rows.map(row => row.runs.reduce((total, run) => total + run.cells, 0));
      const runWidthErrors = grid.rows.flatMap(row => row.runs.map(run => Math.abs(run.width - grid.cellWidth * run.cells)));
      const gaps = grid.rows.flatMap(row => row.runs.slice(1).map((run, index) => Math.abs(run.x - row.runs[index].x - row.runs[index].width)));
      receipt.geometry.push({ group, id, columns: [...new Set(rows)], rows: grid.rows.length,
        cellWidth: grid.cellWidth, maxRunWidthError: Math.max(...runWidthErrors), maxGap: Math.max(0, ...gaps) });
      assert.ok(Number.isFinite(grid.cellWidth) && grid.cellWidth > 0);
      for (const row of grid.rows) {
        assert.ok(row.runs.length > 0);
        assert.doesNotMatch(row.text, /[\r\n\t]/);
        assert.equal(row.runs.reduce((total, run) => total + run.cells, 0), 112);
        row.runs.forEach((run, index) => {
          assert.ok(Number.isInteger(run.cells) && run.cells > 0);
          if (run.dataCells !== null) assert.equal(run.dataCells, run.cells);
          assert.ok(run.width > 0 && run.height > 0);
          assert.ok(Math.abs(run.width - grid.cellWidth * run.cells) <= 0.5);
          if (index) {
            const previous = row.runs[index - 1];
            assert.ok(Math.abs(run.x - previous.x - previous.width) <= 0.5);
            assert.ok(Math.abs(run.y - previous.y) <= 0.5);
          }
        });
      }
      if (record) receipt.compositions.push({ group, id, sha256: sha256(grid.rows.map(row => row.text).join('\n')) });
    }

    await test('gallery API advertises 112 columns and 34 rows', async () => {
      assert.deepEqual(await page.evaluate(() => window.MindPlayerDesignGallery.geometry), { columns: 112, rows: 34 });
    });
    await test('gallery has exactly ten numbered choice buttons in each section', async () => {
      for (const group of groups) {
        assert.equal(await section(group).count(), 1);
        assert.equal(await buttons(group).count(), 10);
        assert.deepEqual(await buttons(group).evaluateAll(nodes => nodes.map(node => Number(node.dataset.idea))), ids);
      }
      assert.equal(await page.locator('button[data-group][data-idea]').count(), 20);
    });
    await test('all twenty choice buttons name different presentation patterns', async () => {
      const patterns = await page.locator('button[data-group][data-idea]').evaluateAll(nodes => nodes.map(node => node.dataset.pattern));
      assert.ok(patterns.every(pattern => typeof pattern === 'string' && pattern.trim().length > 0));
      assert.equal(new Set(patterns).size, 20);
    });
    await test('visible heading and scope identify a Korean HTML-only proposal', async () => {
      assert.match(await page.title(), /[가-힣]/);
      assert.match(await page.locator('h1').innerText(), /[가-힣]/);
      assert.match(await page.locator('.intro-note').innerText(), /HTML.*시안/);
      for (const group of groups) assert.match(await section(group).locator('h2').innerText(), /[가-힣]/);
    });

    for (const group of groups) {
      for (const id of ids) {
        await test(`${group.toUpperCase()}${id} exposes its declared initial state`, async () => {
          assertState(await prepare(group, id), group, id);
        });
        await test(`${group.toUpperCase()}${id} renders a focusable 112 by 34 character grid`, async () => {
          await prepare(group, id); await assertGrid(group, id, true);
        });
        await test(`${group.toUpperCase()}${id} can be selected by its document button`, async () => {
          await button(group, id).click();
          assert.equal((await snapshot(group)).idea, id);
          assert.equal(await button(group, id).getAttribute('aria-selected'), 'true');
          assert.equal(await section(group).locator('button[data-idea][aria-selected="true"]').count(), 1);
        });
        await test(`${group.toUpperCase()}${id} releases keyboard focus with Tab`, async () => {
          await prepare(group, id); await press('Tab');
          const focus = await terminal(group).evaluate(node => ({
            trapped: node.contains(document.activeElement),
            documentControl: document.activeElement !== document.body && document.activeElement.tabIndex >= 0,
          }));
          assert.equal(focus.trapped, false);
          assert.equal(focus.documentControl, true);
        });
      }
      await test(`${group.toUpperCase()} snapshot returns a detached nested clone`, async () => {
        const before = await prepare(group, 1);
        await page.evaluate(group => {
          const state = window.MindPlayerDesignGallery.snapshot(group);
          state.sessions[0].title = '__clone_probe__';
          state.sessions[0].category = '__clone_probe__';
          state.tasks[0].links.push('__clone_probe__');
          state.tasks[0].status = '__clone_probe__';
          state.folded.push('__clone_probe__');
        }, group);
        assert.deepEqual(await snapshot(group), before);
      });
      await test(`${group.toUpperCase()} choice tabs support all four arrow keys`, async () => {
        await prepare(group, 1); await button(group, 1).focus();
        for (const [value, expected] of [['ArrowDown', 2], ['ArrowUp', 1], ['ArrowRight', 2], ['ArrowLeft', 1]]) {
          await press(value);
          assert.equal((await snapshot(group)).idea, expected);
          assert.equal(await button(group, expected).evaluate(node => node === document.activeElement), true);
        }
      });
      await test(`${group.toUpperCase()} choice tabs support Home and End`, async () => {
        await button(group, 1).focus(); await press('End');
        assert.equal((await snapshot(group)).idea, 10);
        await press('Home'); assert.equal((await snapshot(group)).idea, 1);
      });
      await test(`${group.toUpperCase()} choice buttons activate with Enter and Space`, async () => {
        await prepare(group, 1);
        await button(group, 3).focus(); await press('Enter');
        assert.equal((await snapshot(group)).idea, 3);
        await button(group, 4).focus(); await press('Space');
        assert.equal((await snapshot(group)).idea, 4);
      });
    }
    await test('all twenty initial terminal compositions differ', () => {
      assert.equal(receipt.compositions.length, 20);
      assert.equal(new Set(receipt.compositions.map(frame => frame.sha256)).size, 20);
    });
    await test('B8 initially records the selected session move from ops to docs', async () => {
      const state = await prepare('b', 8);
      assert.deepEqual(state.lastMove, { id: state.sessions[state.selectedSession].id, from: 'ops', to: 'docs' });
      assert.equal(state.sessions[state.selectedSession].category, 'docs');
    });

    for (const id of ids) {
      await test(`A${id} F6 switches list focus in both directions`, async () => {
        const before = await prepare('a', id);
        await key('a', 'F6'); assert.equal((await snapshot('a')).focus, 'tasks');
        await key('a', 'F6'); assert.equal((await snapshot('a')).focus, 'sessions');
        assert.deepEqual(records(await snapshot('a')), records(before));
      });
      for (const pane of ['sessions', 'tasks']) {
        await test(`A${id} arrows select only the focused ${pane} list`, async () => {
          await prepare('a', id); await focusPane('a', pane);
          const field = pane === 'sessions' ? 'selectedSession' : 'selectedTask';
          const other = pane === 'sessions' ? 'selectedTask' : 'selectedSession';
          const before = await snapshot('a');
          await key('a', 'ArrowDown');
          assert.equal((await snapshot('a'))[field], 1);
          assert.equal((await snapshot('a'))[other], before[other]);
          await key('a', 'ArrowUp'); assert.equal((await snapshot('a'))[field], 0);
          assert.deepEqual(records(await snapshot('a')), records(before));
        });
        await test(`A${id} F7 toggles exactly the selected link from ${pane}`, async () => {
          await prepare('a', id); await focusPane('a', pane);
          const before = await snapshot('a'), expected = copy(records(before));
          const sessionId = before.sessions[before.selectedSession].id;
          const task = expected.tasks[before.selectedTask];
          task.links = task.links.includes(sessionId) ? task.links.filter(id => id !== sessionId) : [...task.links, sessionId];
          await key('a', 'F7');
          assert.deepEqual(records(await snapshot('a')), expected);
        });
        await test(`A${id} a second F7 restores the link graph from ${pane}`, async () => {
          await prepare('a', id); await focusPane('a', pane);
          const before = await snapshot('a');
          await key('a', 'F7'); await key('a', 'F7');
          assert.deepEqual(graph(await snapshot('a')), graph(before));
        });
      }
      await test(`A${id} supports several sessions per task and several tasks per session`, async () => {
        const before = await prepare('a', id);
        await setLinked(0, 0); await setLinked(1, 0); await setLinked(0, 1);
        const after = await snapshot('a');
        assert.ok(after.tasks[0].links.includes(after.sessions[0].id));
        assert.ok(after.tasks[0].links.includes(after.sessions[1].id));
        assert.ok(after.tasks[1].links.includes(after.sessions[0].id));
        for (const task of before.tasks) {
          const current = after.tasks.find(candidate => candidate.id === task.id);
          assert.ok(task.links.every(id => current.links.includes(id)), 'unrelated links are retained');
        }
        assert.deepEqual(after.sessions, before.sessions);
      });
      await test(`A${id} Space completes only the selected manual task`, async () => {
        await prepare('a', id); await focusPane('a', 'tasks');
        const before = await snapshot('a'), expected = copy(records(before));
        expected.tasks[before.selectedTask].status = '완료';
        await key('a', 'Space');
        assert.deepEqual(records(await snapshot('a')), expected);
      });
      await test(`A${id} completed manual tasks can be reopened`, async () => {
        await prepare('a', id); await focusPane('a', 'tasks');
        const before = await snapshot('a');
        await key('a', 'Space'); await key('a', 'Space');
        assert.equal((await snapshot('a')).tasks[before.selectedTask].status, before.tasks[before.selectedTask].status);
      });
      await test(`A${id} Space on the session list leaves task and native states unchanged`, async () => {
        const before = await prepare('a', id);
        await key('a', 'Space');
        assert.deepEqual(records(await snapshot('a')), records(before));
      });
      await test(`A${id} a native response ending does not complete its linked manual task`, async () => {
        const state = await prepare('a', id);
        const ended = state.sessions.filter(session => session.native === '응답 종료');
        assert.ok(ended.length > 0);
        const linked = state.tasks.filter(task => task.links.some(id => ended.some(session => session.id === id)));
        assert.ok(linked.length > 0 && linked.some(task => task.status !== '완료'));
      });
      await test(`A${id} Enter shows a mock notice without changing task or session records`, async () => {
        const before = await prepare('a', id);
        await key('a', 'Enter');
        assert.match(await page.locator('#notice-a').innerText(), /실제 CLI 실행 없음/);
        assert.deepEqual(records(await snapshot('a')), records(before));
      });
    }

    for (const id of ids) {
      await test(`B${id} T opens a picker without moving a session`, async () => { await picker(id); });
      await test(`B${id} picker arrows change only the destination`, async () => {
        const before = await picker(id);
        await key('b', 'ArrowDown');
        assert.equal((await snapshot('b')).target, (before.target + 1) % categoryIds.length);
        await key('b', 'ArrowUp');
        const after = await snapshot('b');
        assert.equal(after.target, before.target);
        assert.equal(after.selectedSession, before.selectedSession);
        assert.deepEqual(records(after), records(before));
      });
      await test(`B${id} first Enter previews the move without applying it`, async () => {
        const { before } = await destination(id);
        await key('b', 'Enter');
        assert.equal((await snapshot('b')).phase, 'confirm');
        assert.deepEqual(records(await snapshot('b')), records(before));
      });
      await test(`B${id} second Enter moves exactly one stable session and records its route`, async () => { await moved(id); });
      for (const phase of ['pick', 'confirm']) {
        await test(`B${id} Escape cancels ${phase} without changing any records`, async () => {
          const { before } = await destination(id);
          if (phase === 'confirm') await key('b', 'Enter');
          assert.equal((await snapshot('b')).phase, phase);
          await key('b', 'Escape');
          const after = await snapshot('b');
          assert.equal(after.phase, 'browse');
          assert.deepEqual(records(after), records(before));
          assert.deepEqual(after.lastMove, before.lastMove);
        });
      }
      await test(`B${id} F8 restores the moved session without changing its identity or tasks`, async () => {
        const { before } = await moved(id);
        await key('b', 'F8');
        const after = await snapshot('b');
        assert.deepEqual(records(after), records(before));
        assert.equal(after.lastMove, null);
        assert.equal(after.phase, 'browse');
      });
      await test(`B${id} G finds the moved session and clears filter search and folds`, async () => {
        const { after: movedState } = await moved(id);
        const selected = movedState.sessions[movedState.selectedSession];
        await key('b', 'g');
        const found = await snapshot('b');
        assert.deepEqual(records(found), records(movedState));
        assert.equal(found.sessions[found.selectedSession].id, selected.id);
        assert.equal(found.filter, 'all');
        assert.equal(found.query, '');
        assert.deepEqual(found.folded, []);
        assert.equal(found.phase, 'browse');
        const rendered = await text('b');
        assert.ok(rendered.includes(selected.id) && rendered.includes(selected.title));
      });
      await test(`B${id} browse arrows select a session without changing category or title`, async () => {
        await prepare('b', id); await key('b', 'Escape');
        const before = await snapshot('b');
        await key('b', 'ArrowDown'); assert.equal((await snapshot('b')).selectedSession, 1);
        await key('b', 'ArrowUp'); assert.equal((await snapshot('b')).selectedSession, 0);
        assert.deepEqual(records(await snapshot('b')), records(before));
      });
    }
    await test('B1 G clears a nonempty category filter', async () => {
      const before = await prepare('b', 1);
      assert.notEqual(before.filter, 'all');
      await key('b', 'g'); assert.equal((await snapshot('b')).filter, 'all');
      assert.deepEqual(records(await snapshot('b')), records(before));
    });
    await test('B9 G clears a nonempty search query', async () => {
      const before = await prepare('b', 9);
      assert.ok(before.query.length > 0);
      await key('b', 'g'); assert.equal((await snapshot('b')).query, '');
      assert.deepEqual(records(await snapshot('b')), records(before));
    });

    for (const group of groups) {
      const other = group === 'a' ? 'b' : 'a';
      await test(`${group.toUpperCase()} selection leaves the other section unchanged`, async () => {
        const before = await snapshot(other);
        await button(group, 10).click();
        assert.equal((await snapshot(group)).idea, 10);
        assert.deepEqual(await snapshot(other), before);
      });
      await test(`${group.toUpperCase()} reset restores only its own current idea`, async () => {
        const baseline = await prepare(group, 3);
        if (group === 'a') await key(group, 'F7');
        else await key(group, 't');
        assert.notDeepEqual(await snapshot(group), baseline);
        const otherBefore = await snapshot(other);
        await page.evaluate(group => window.MindPlayerDesignGallery.reset(group), group);
        assert.deepEqual(await snapshot(group), baseline);
        assert.deepEqual(await snapshot(other), otherBefore);
      });
    }
    await test('A link demo control uses the same link behavior as F7', async () => {
      const before = await prepare('a', 1);
      await section('a').locator('button[data-action="link"]').click();
      assert.notDeepEqual(graph(await snapshot('a')), graph(before));
      await key('a', 'F7'); assert.deepEqual(graph(await snapshot('a')), graph(before));
    });
    await test('B move demo control opens the picker', async () => {
      const before = await prepare('b', 1);
      await section('b').locator('button[data-action="move"]').click();
      assert.equal((await snapshot('b')).phase, 'pick');
      assert.deepEqual(records(await snapshot('b')), records(before));
    });
    await test('B undo demo control restores the last move', async () => {
      const { before } = await moved(1);
      await section('b').locator('button[data-action="undo"]').click();
      assert.deepEqual(records(await snapshot('b')), records(before));
      assert.equal((await snapshot('b')).lastMove, null);
    });
    await test('B find demo control reveals the selected session', async () => {
      const before = await prepare('b', 9);
      await section('b').locator('button[data-action="find"]').click();
      const after = await snapshot('b');
      assert.equal(after.filter, 'all'); assert.equal(after.query, '');
      assert.deepEqual(records(after), records(before));
    });
    await test('preserved execution preview keeps four top and three bottom panes', async () => {
      const layout = page.locator('[data-layout="4+3"]');
      assert.equal(await layout.count(), 1);
      assert.equal(await layout.locator('.panes.top > .pane').count(), 4);
      assert.equal(await layout.locator('.panes.bottom > .pane').count(), 3);
      const pane = await layout.locator('.pane').first().boundingBox();
      const quota = await layout.locator('.quota-preview').boundingBox();
      assert.ok(pane && quota && quota.width > pane.width * 3.5);
    });

    await page.setViewportSize({ width: 1440, height: 1050 });
    for (const group of groups) for (const id of ids) {
      await test(`${group.toUpperCase()}${id} has a fresh desktop terminal screenshot`, async () => {
        await prepare(group, id); await assertGrid(group, id);
        const image = await screenshot(`${group}-${String(id).padStart(2, '0')}-desktop-1440`, terminal(group), { group, id, viewport: 1440 });
        assert.ok(image.width >= 750 && image.height >= 500);
      });
    }
    await test('all twenty desktop terminal screenshots differ', () => {
      const images = receipt.screenshots.filter(image => image.viewport === 1440 && image.id);
      assert.equal(images.length, 20);
      assert.equal(new Set(images.map(image => image.sha256)).size, 20);
    });
    for (const width of viewports) {
      await test(`${width}px viewport has no document horizontal overflow`, async () => {
        await page.setViewportSize({ width, height: 1050 });
        await prepare('a', 1); await prepare('b', 1);
        await page.evaluate(() => {
          for (const node of document.querySelectorAll('.terminal-scroll')) node.scrollLeft = 0;
          window.scrollTo(0, 0);
        });
        const measured = await page.evaluate(() => ({
          viewport: innerWidth, document: document.documentElement.scrollWidth, body: document.body.scrollWidth,
          terminals: [...document.querySelectorAll('.terminal-scroll')].map(node => {
            const bounds = node.getBoundingClientRect();
            return { left: bounds.left, right: bounds.right, clientWidth: node.clientWidth,
              scrollWidth: node.scrollWidth, overflowX: getComputedStyle(node).overflowX };
          }),
        }));
        receipt.geometry.push({ viewport: width, ...measured });
        assert.ok(measured.document <= measured.viewport + 1);
        assert.ok(measured.body <= measured.viewport + 1);
        assert.equal(measured.terminals.length, 2);
        for (const bounds of measured.terminals) {
          assert.ok(bounds.left >= -1 && bounds.right <= measured.viewport + 1);
          assert.ok(bounds.clientWidth > 0);
          if (bounds.scrollWidth > bounds.clientWidth + 1) assert.ok(['auto', 'scroll'].includes(bounds.overflowX));
        }
        for (const group of groups) await assertGrid(group, 1);
        await screenshot(`top-${width}`, page, { viewport: width });
      });
    }
    await page.setViewportSize({ width: 390, height: 1050 });
    for (const group of groups) await test(`${group.toUpperCase()} section has a fresh mobile screenshot`, async () => {
      await prepare(group, 1);
      await screenshot(`${group}-mobile-390`, section(group), { group, viewport: 390 });
    });

    await test('prototype leaves browser storage empty', async () => {
      assert.deepEqual(await page.evaluate(() => ({ local: localStorage.length, session: sessionStorage.length, cookie: document.cookie.length })),
        { local: 0, session: 0, cookie: 0 });
      assert.equal((await context.cookies()).length, 0);
    });
    await test('browser emits no console messages or page errors', () => {
      assert.deepEqual(receipt.console, { messages: 0, errors: 0, warnings: 0, pageErrors: 0 });
    });
    await test('browser makes no external API asset or WebSocket requests', () => {
      assert.deepEqual(receipt.network.blockedRequests, []);
      assert.deepEqual(receipt.network.failedRequests, []);
      assert.deepEqual(receipt.network.badResponses, []);
      assert.equal(receipt.network.webSockets, 0);
      assert.ok(receipt.network.allowedRequests >= 1);
    });
    successful = true;
  } catch (error) {
    receipt.failure = failureMetadata(error, currentCase);
    process.exitCode = 1;
  } finally {
    try {
      if (browser) {
        await browser.close();
        assert.equal(browser.isConnected(), false);
        receipt.cleanup.browser = 'closed';
      }
    } catch (error) {
      successful = false; process.exitCode = 1;
      receipt.cleanup.browser = 'failed';
      receipt.cleanup.browserFailure = failureMetadata(error, 'await browser.close');
    } finally {
      try {
        if (server && server.listening) {
          await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
          assert.equal(server.listening, false);
          receipt.cleanup.server = 'closed';
        }
      } catch (error) {
        successful = false; process.exitCode = 1;
        receipt.cleanup.server = 'failed';
        receipt.cleanup.serverFailure = failureMetadata(error, 'await server.close');
      } finally {
        if (source !== undefined) {
          try {
            receipt.source.afterSha256 = sha256(fs.readFileSync(htmlPath, 'utf8'));
            receipt.source.unchanged = receipt.source.afterSha256 === receipt.source.sha256;
            if (!receipt.source.unchanged) { successful = false; process.exitCode = 1; }
          } catch (error) {
            successful = false; process.exitCode = 1;
            receipt.source.failure = failureMetadata(error, 'source freeze check');
          }
        }
        receipt.result = successful ? 'passed' : 'failed';
        receipt.elapsedMs = Date.now() - started;
        receipt.counts = { passed: receipt.cases.filter(test => test.result === 'passed').length,
          failed: receipt.cases.filter(test => test.result === 'failed').length, screenshots: receipt.screenshots.length };
        fs.writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`, { flag: 'wx' });
        emit({ result: receipt.result, mode: receipt.mode, ...receipt.counts, receiptPath,
          ...(receipt.failure ? { failure: receipt.failure } : {}), cleanup: receipt.cleanup });
      }
    }
  }
}

main().catch(error => {
  console.error(JSON.stringify({ result: 'failed', failure: failureMetadata(error, 'harness output') }));
  process.exitCode = 1;
});
