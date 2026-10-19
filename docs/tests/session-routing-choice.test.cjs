'use strict';

// 브라우저 시안만 검증한다. Rust 앱 / CLI / Jev 실행 검증이 아니다.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const http = require('node:http');
const vm = require('node:vm');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');

const repo = path.resolve(__dirname, '../..');
const relativeHtml = 'docs/plans/2026-10-16-session-routing-choice.ko.html';
const htmlPath = path.join(repo, relativeHtml);
const source = fs.readFileSync(htmlPath, 'utf8');
const sha256 = text => crypto.createHash('sha256').update(text).digest('hex');
const results = [];
function pass(name) { results.push(name); console.log(`PASS ${name}`); }

async function main() {
  assert.match(source, /<html lang="ko">/);
  const scripts = [...source.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)];
  assert.equal(scripts.length, 1);
  new vm.Script(scripts[0][1], { filename: relativeHtml });
  const ids = [...source.matchAll(/\bid="([^"]+)"/g)].map(m => m[1]);
  assert.equal(ids.length, new Set(ids).size, 'unique IDs');
  assert.ok(source.indexOf('id="assessment"') < source.indexOf('id="comparison"'));
  assert.match(source, /앱 미구현/);
  assert.match(source, /기본값 미사용.*미승인/);
  assert.match(source, /미사용.*기존 CLI 그대로/);
  assert.match(source, /실행 중인 창/);
  assert.doesNotMatch(source, /<(?:script|link|img|iframe)[^>]+(?:src|href)=["']https?:/i);
  assert.doesNotMatch(scripts[0][1], /\b(?:fetch|XMLHttpRequest|WebSocket|localStorage|sessionStorage|sendBeacon)\b/);
  assert.doesNotMatch(source, /(?:sk-[A-Za-z0-9_-]{18,}|Bearer\s+[A-Za-z0-9._-]{20,}|vck_[A-Za-z0-9_-]{15,})/);
  assert.doesNotMatch(source, /[\u3040-\u30ff]/, 'Korean copy must not contain stray Japanese');
  pass('static: Korean, assessment first, draft scope, unique IDs, inline syntax, no credentials/network/storage');

  const playwright = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');
  const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-routing-choice-'));
  const screenshots = [];
  const server = http.createServer((req,res) => {
    if (req.url === '/flow') { res.writeHead(200, { 'Content-Type':'text/html; charset=utf-8' }); res.end(source); }
    else if (req.url === '/favicon.ico') { res.writeHead(204); res.end(); }
    else { res.writeHead(404); res.end(); }
  });
  await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  let browser;
  const errors = [], externalRequests = [];
  try {
    browser = await playwright.chromium.launch({ channel:'chrome', headless:true });
    const page = await browser.newPage({ viewport:{ width:1440,height:1000 }, reducedMotion:'reduce' });
    page.on('pageerror',e => errors.push(String(e)));
    page.on('console',m => { if (m.type() === 'error') errors.push(m.text()); });
    page.on('request',r => { if (!r.url().startsWith(base)) externalRequests.push(r.url()); });
    await page.goto(`${base}/flow`);
    const visible = id => page.locator(`#${id}`).isVisible();
    const content = id => page.locator(`#${id}`).textContent();
    const disabled = id => page.locator(`#${id}`).isDisabled();
    async function reset(kind='existing',ready='ready') {
      await page.selectOption('#entry-kind',kind);
      await page.selectOption('#ready-kind',ready);
      await page.click('#reset');
    }
    async function useJev() {
      await page.check('input[name="mode"][value="jev"]');
      await page.check('#consent');
    }
    async function stage(value) {
      await page.selectOption('#flow-stage',value);
      await page.click('#show-stage');
    }
    async function screenshot(name) {
      const output = path.join(artifacts,`${name}.png`);
      await page.screenshot({ path:output, fullPage:false });
      screenshots.push(output);
    }
    async function noOuterOverflow() {
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),true,'no page horizontal overflow');
    }

    for (const id of ['assessment','current-screen','open-choice']) {
      const box = await page.locator(`#${id}`).boundingBox();
      assert.ok(box && box.y >= 0 && box.y + box.height <= 900,`${id} fits first 900px viewport`);
    }
    const left = await page.locator('#current-screen').boundingBox();
    const right = await page.locator('#proposed-screen').boundingBox();
    assert.ok(left.x + left.width < right.x,'side-by-side actual TUI comparison');
    await noOuterOverflow();
    await screenshot('desktop-first-viewport');
    pass('desktop: direction assessment and current/proposed TUI choice in first viewport');

    for (const kind of ['existing','new']) {
      await reset(kind,'missing');
      assert.ok(await visible('open-choice'));
      assert.equal(await visible('focus-pane'),false);
      assert.equal(await disabled('open-button'),false);
      assert.match(await content('readiness'),/Jev 키 없이/);
      assert.equal(await page.isChecked('input[value="native"]'),true);
      await page.click('#cancel-button');
      assert.ok(await visible('cancel-note'));
      assert.equal(await visible('focus-pane'),false);
      assert.match(await content('notice'),/생성 \/ 실행 \/ 선택 기록 없음/);
      await reset(kind,'missing');
      await page.click('#open-button');
      assert.ok(await visible('focus-pane'));
      assert.equal(await page.locator('#focus-pane').evaluate(el => getComputedStyle(el).backgroundColor),'rgb(40, 44, 52)','opaque pane hides the underlying list');
      assert.doesNotMatch(await content('target-label'),/시작 전/);
      assert.match(await content('pane-title'),/Jev 미사용/);
      if (kind === 'existing') assert.match(await content('proposed-caption'),/같은 세션 \/ 계정 유지/);
      else assert.match(await content('proposed-caption'),/실제 생성 후 선택 기록 제안/);
      for (const step of ['entry','judging','requested','running','completed','followup','queued','failure']) {
        await stage(step);
        assert.match(await content('pane-status'),/Jev 미사용/);
        assert.doesNotMatch(await content('pane-content'),/Jev 판단 중/);
      }
      assert.match(await content('pane-content'),/CLI 자체 오류/);
    }
    pass('existing + new: native default, no Jev key required, cancellation, unchanged identity, all native stages');

    for (const kind of ['existing','new']) {
      await reset(kind,'ready');
      await page.check('input[value="jev"]');
      assert.ok(await visible('consent-row'));
      assert.ok(await disabled('open-button'),'key does not imply consent');
      assert.match(await content('consent-row'),/TypeSafe Jev/);
      await page.check('#consent');
      assert.equal(await disabled('open-button'),false);
      assert.match(await content('readiness'),/실제 인증/);
      await page.click('#open-button');
      assert.match(await content('pane-title'),/Jev 사용/);
      await stage('judging');
      assert.match(await content('pane-content'),/아직 Codex 실행에 보내지/);
      await stage('requested');
      assert.match(await content('pane-content'),/model \+ effort/);
      assert.match(await content('pane-status'),/medium/);
      await stage('running');
      assert.match(await content('pane-status'),/턴 시작 확인/);
      assert.match(await content('pane-content'),/도구 실행 중/);
      assert.match(source,/서버 내부 serving.*검증한 것은 아니다/);
      await stage('completed');
      assert.match(await content('pane-status'),/완료/);
      await stage('followup');
      assert.match(await content('pane-content'),/다시 Jev 판단/);
      assert.match(await content('pane-content'),/유지도 정상/);
      await stage('queued');
      assert.match(await content('pane-content'),/개별 네이티브 큐/);
      assert.match(await content('pane-content'),/중단 \/ 실패 뒤 자동 실행하지/);
      await stage('failure');
      assert.match(await content('pane-status'),/판단 오류 · 실행 미전송/);
      assert.match(await content('pane-content'),/자동 미사용 전환 \/ 자동 재전송 없음/);
    }
    await stage('running');
    await page.locator('#assessment').scrollIntoViewIfNeeded();
    await screenshot('desktop-jev-running');
    pass('existing + new: separate consent, model + effort, request/start distinction, follow-up, queue and fail-closed');

    for (const ready of ['missing','denied','unsupported']) {
      await reset('existing',ready);
      await useJev();
      assert.ok(await disabled('open-button'));
      assert.equal(await visible('focus-pane'),false);
      const expected = {missing:/키 없음/,denied:/정책이 외부 전송 금지/,unsupported:/미지원/};
      assert.match(await content('readiness'),expected[ready]);
      await page.click('#use-native');
      assert.equal(await disabled('open-button'),false);
      assert.equal(await page.isChecked('input[value="native"]'),true);
      await page.click('#open-button');
      assert.match(await content('pane-title'),/Jev 미사용/);
    }
    pass('missing key + transfer-denied policy + unsupported CLI: explicit native change only, no silent fallback');

    await reset('live');
    assert.ok(await visible('live-note'));
    assert.equal(await visible('open-choice'),false);
    assert.equal(await visible('focus-pane'),false);
    await stage('judging');
    assert.match(await content('notice'),/복귀로 바뀌지/);
    assert.match(await content('live-note'),/대기 입력을 중단하거나 재전송하지/);
    pass('live reattach: preserves active process and effective mode; no hot switch or duplicate spawn');

    await reset();
    await page.focus('input[value="native"]');
    await page.keyboard.press('ArrowDown');
    assert.equal(await page.isChecked('input[value="jev"]'),true);
    await page.keyboard.press('ArrowUp');
    assert.equal(await page.isChecked('input[value="native"]'),true);
    await page.keyboard.press('Escape');
    assert.ok(await visible('cancel-note'));
    await reset();
    await page.focus('input[value="native"]');
    await page.keyboard.press('Enter');
    assert.ok(await visible('focus-pane'),'Enter opens native choice');
    await page.locator('#alternatives > summary').focus();
    await page.keyboard.press('Enter');
    assert.equal(await page.locator('#alternatives').getAttribute('open'),'');
    assert.equal(await page.locator('.alternative').count(),3);
    pass('keyboard: radio arrows, Escape, Enter submit and expandable three alternatives');

    await page.locator('#layout-detail > summary').click();
    assert.equal(await page.locator('.pane-row.top .tiny-pane').count(),4);
    assert.equal(await page.locator('.pane-row.bottom .tiny-pane').count(),3);
    const panes = await page.locator('.tiny-pane').first().boundingBox();
    const layout = await page.locator('#layout-preview').boundingBox();
    const footer = await page.locator('.layout-footer').boundingBox();
    assert.ok(panes && layout && footer && footer.width > panes.width * 3.5);
    await page.locator('#layout-preview').scrollIntoViewIfNeeded();
    await screenshot('desktop-four-plus-three');
    pass('layout: seven panes in 4+3 rows and one full-width usage footer');

    await reset();
    for (const width of [969,390]) {
      await page.setViewportSize({width,height:1000});
      await page.evaluate(() => scrollTo(0,0));
      await noOuterOverflow();
      if (width === 390) {
        assert.equal(await page.locator('.comparison').evaluate(el => getComputedStyle(el).gridTemplateColumns.split(' ').length),1);
        assert.ok(await page.locator('.terminal-scroll').first().evaluate(el => el.scrollWidth > el.clientWidth));
      }
      await screenshot(`viewport-${width}`);
    }
    assert.equal(await page.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches),true);
    assert.deepEqual(errors,[]);
    assert.deepEqual(externalRequests,[]);
    pass('responsive: 969/390px, internal TUI scroll only, reduced motion, zero browser errors/external requests');

    const receipt = {
      schemaVersion:1,
      verifiedAt:new Date().toISOString(),
      scope:'standalone UI prototype only; not Rust launch/resume, Jev inference or installation',
      source:{ html:relativeHtml,sha256:sha256(source),head:execFileSync('git',['rev-parse','HEAD'],{cwd:repo,encoding:'utf8'}).trim(),worktree:'pre-existing dirty changes preserved' },
      browser:{name:'Google Chrome via Playwright',version:browser.version(),headless:true},
      result:'passed',groups:results,externalRequests:externalRequests.length,browserErrors:errors.length,
      actualJevCalls:0,actualModelCalls:0,appCodeChanged:false,appInstalled:false,
      viewports:[{width:1440,height:1000,coreFitsHeight:900},{width:969,height:1000},{width:390,height:1000}],
      screenshots,
      notVerified:['Rust integration','real CLI mode persistence and queue behavior','service authentication','actual model/effort switching','cost savings / quality / cache performance']
    };
    // 테스트가 생성한 검증 출력이다. 제품 소스를 수정하지 않는다.
    const receiptPath = process.env.MINDPLAYER_ROUTING_CHOICE_RECEIPT;
    if (receiptPath) fs.writeFileSync(receiptPath,`${JSON.stringify(receipt,null,2)}\n`);
    console.log(JSON.stringify(receipt,null,2));
  } finally {
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
  }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
