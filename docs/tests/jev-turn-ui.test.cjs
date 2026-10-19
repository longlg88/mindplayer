'use strict';

// UI 문서만 검증. 기존 Playwright/Chrome 사용, 실제 Jev/Codex 호출 없음.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const http = require('node:http');
const vm = require('node:vm');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
const { chromium } = require(process.env.MINDPLAYER_PLAYWRIGHT_MODULE || 'playwright');
const root = path.resolve(__dirname, '../..');
const artifact = path.join(root, 'docs/plans/2026-10-15-jev-turn-ui.ko.html');
const html = fs.readFileSync(artifact, 'utf8');

async function main() {
  const scripts = [...html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)];
  assert.equal(scripts.length, 1);
  new vm.Script(scripts[0][1]);
  assert.match(html, /<html lang="ko">/);
  assert(!/<(?:script|img|iframe)\b[^>]*src=/i.test(html));
  assert(!/<link\b[^>]*href="(?!data:)/i.test(html));
  assert(!/\b(?:fetch|XMLHttpRequest|WebSocket|localStorage|sessionStorage)\s*[.(]/.test(scripts[0][1]));
  assert(!/\bsk-(?:proj-)?[A-Za-z0-9_-]{16,}|\bAKIA[A-Z0-9]{16}|-----BEGIN (?:RSA |OPENSSH )?PRIVATE KEY-----/.test(html));
  const ids = [...html.matchAll(/\bid="([^"]+)"/g)].map(m => m[1]);
  assert.equal(new Set(ids).size, ids.length);
  for (const phrase of ['실제 앱 변경 아님', '모델 A·B는 가상 후보', '실행 모델 미확인', '입력 보존', '같은 세션', '같은 계정', '같은 작업 폴더', '세 대안']) assert(html.includes(phrase), phrase);
  console.log('PASS static: Korean, valid script/IDs, no remote dependency/transport/storage/credential patterns');

  const screenshots = fs.mkdtempSync(path.join(os.tmpdir(), 'mindplayer-jev-turn-ui-'));
  const server = http.createServer((req, res) => {
    if (new URL(req.url, 'http://localhost').pathname === '/flow') { res.writeHead(200, {'Content-Type':'text/html; charset=utf-8'}); res.end(html); }
    else { res.writeHead(404); res.end('Not found'); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = 'http://127.0.0.1:' + server.address().port;
  const errors = [], externalRequests = [];
  let browser;
  try {
    browser = await chromium.launch({channel:'chrome', headless:true});
    const page = await browser.newPage({viewport:{width:1440,height:1000},reducedMotion:'reduce'});
    page.on('pageerror', error => errors.push(String(error)));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    page.on('request', request => { if (!request.url().startsWith(origin + '/')) externalRequests.push(request.url()); });
    await page.goto(origin + '/flow');
    assert.equal(await page.title(), 'MindPlayer · 세션부터 매 입력까지');
    assert.equal(await page.locator('[data-story]').count(), 10);
    assert.equal(await page.locator('[data-step]').count(), 10);
    assert.equal(await page.locator('[data-case]').count(), 4);
    assert.equal(await page.locator('details[open]').count(), 0);
    assert.equal(await page.locator('.mp-shell button,.mp-shell input,.mp-shell select,.mp-shell textarea').count(), 0, '제품 컨트롤 추가 없음');
    const firstViewport = await page.evaluate(() => ({comparison:document.querySelector('.comparison').getBoundingClientRect().bottom,navigator:document.querySelector('.navigator').getBoundingClientRect().bottom,height:innerHeight,overflow:document.documentElement.scrollWidth>innerWidth}));
    assert(firstViewport.comparison < firstViewport.height && firstViewport.navigator < firstViewport.height && !firstViewport.overflow);
    assert.equal(await page.locator('#current-view .focus-pane').evaluate(el => el.getBoundingClientRect().height), await page.locator('#proposed-view .focus-pane').evaluate(el => el.getBoundingClientRect().height), '추가 pane row 없음');
    await page.screenshot({path:path.join(screenshots,'desktop-top.png')});
    await page.locator('#storyboard').screenshot({path:path.join(screenshots,'desktop-storyboard.png')});

    const expected = {
      list:null,
      entry:'모델 A · medium · 직전 턴',
      typing:'모델 A · medium · 직전 턴',
      judging:'Jev 판단 중 · T1 · 실행 대기',
      requested:'모델 B · high · T1 요청 · 수락 대기',
      running:'모델 B · high · T1 실행 중 · 실행 확인',
      completed:'모델 B · high · T1 응답 완료',
      'followup-judge':'Jev 판단 중 · T2 · 실행 대기',
      'followup-run':'모델 A · low · T2 실행 중 · 실행 확인',
      'next-run':'모델 B · high · T3 실행 중 · 실행 확인',
    };
    for (const [id, model] of Object.entries(expected)) {
      const button = page.locator(`[data-step="${id}"]`);
      await button.focus(); await page.keyboard.press('Enter');
      assert.equal(await page.locator('[data-step][aria-pressed="true"]').count(), 1);
      const identity = await page.locator('#proposed-view .mp-shell').evaluate(el => ({session:el.dataset.session,account:el.dataset.account,cwd:el.dataset.cwd}));
      assert.deepEqual(identity,{session:'01a13399',account:'personal-account',cwd:'.'});
      if (model === null) { assert.equal(await page.locator('#proposed-view .flow-list').count(),1); }
      else { assert.equal(await page.locator('#proposed-view .model-line').textContent(),model); assert.equal(await page.locator('#proposed-view .composer').count(),1); }
      const footer = await page.locator('#proposed-view').evaluate(el => {const shell=el.querySelector('.mp-shell').getBoundingClientRect(),q=el.querySelector('.quota-footer').getBoundingClientRect(); return {inside:q.left>=shell.left&&q.right<=shell.right,widthDelta:shell.width-q.width};});
      assert(footer.inside && footer.widthDelta<=16, '全幅 footer 유지');
    }
    await page.locator('[data-step="typing"]').click();
    assert((await page.locator('#proposed-view .composer').textContent()).includes('동시 실행에서'));
    assert(!(await page.locator('#proposed-view .model-line').textContent()).includes('판단 중'));
    await page.locator('[data-step="followup-judge"]').click();
    assert((await page.locator('#proposed-view .conversation').textContent()).includes('원인 후보와 수정 대안'));
    assert((await page.locator('#proposed-view .conversation').textContent()).includes('방금 결론'));
    await page.keyboard.press('ArrowLeft');
    assert.equal(await page.locator('[data-step="completed"]').getAttribute('aria-pressed'),'true');
    await page.keyboard.press('Home'); assert.equal(await page.locator('[data-step="list"]').getAttribute('aria-pressed'),'true');
    await page.keyboard.press('End'); assert.equal(await page.locator('[data-step="next-run"]').getAttribute('aria-pressed'),'true');
    console.log('PASS journey: 10 stages, compose-before-submit, per-turn B→A→B, stable session/account/CWD, keyboard');

    for (const id of ['steer','failure','unverified','interrupted']) {
      await page.locator(`[data-case="${id}"]`).focus(); await page.keyboard.press('Enter');
      assert.equal(await page.locator('[data-case][aria-pressed="true"]').count(),1);
      const text=await page.locator('#edge-view .model-line').textContent();
      if(id==='steer') assert(text.includes('T3 계속 실행'));
      if(id==='failure') {assert(text.includes('실행 보류'));assert((await page.locator('#edge-view .composer').textContent()).includes('그 수정안'));}
      if(id==='unverified') {assert(text.includes('미확인'));assert(!text.includes('실행 확인'));}
      if(id==='interrupted') {assert(text.includes('중단됨'));assert(!text.includes('완료'));}
    }
    for(const id of ['decisions','truth','evidence']){const summary=page.locator(`#${id}>summary`);await summary.focus();await page.keyboard.press('Enter');assert(await page.locator(`#${id}`).evaluate(el=>el.open));await page.keyboard.press('Space');assert.equal(await page.locator(`#${id}`).evaluate(el=>el.open),false);}
    assert.equal(await page.locator('#grid-view .grid-pane').count(),7);
    const rows=await page.locator('#grid-view .grid-pane').evaluateAll(panes=>{const bands=new Map();for(const pane of panes){const y=pane.getBoundingClientRect().top;bands.set(y,(bands.get(y)||0)+1);}return [...bands.values()];});
    assert.deepEqual(rows,[4,3]);
    assert.equal(await page.locator('#grid-view .model-line.proposed').count(),1);
    const gridFooter=await page.locator('#grid-view').evaluate(el=>el.querySelector('.quota-footer').getBoundingClientRect().top>=el.querySelector('.pane-grid').getBoundingClientRect().bottom);
    assert(gridFooter);
    console.log('PASS states/layout: 4 exception states, 4+3 panes, one model-line change, full-width quota');

    await page.locator('[data-step="running"]').click();
    for(const width of [969,390]){
      await page.setViewportSize({width,height:width===969?822:844});await page.evaluate(()=>scrollTo(0,0));
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,`no document overflow at ${width}`);
      await page.screenshot({path:path.join(screenshots,`${width}-top.png`)});
      const region=page.locator('#proposed-view');await region.focus();await page.keyboard.press('ArrowRight');
      await page.waitForFunction(()=>document.querySelector('#proposed-view').scrollLeft>0);
    }
    assert.deepEqual(externalRequests,[]);assert.deepEqual(errors,[]);
    console.log('PASS browser: 1440/969/390px, horizontal scrolling, reduced motion, external requests=0, errors=0');
    console.log(JSON.stringify({verifiedAt:new Date().toISOString(),artifact:path.relative(root,artifact),sha256:crypto.createHash('sha256').update(html).digest('hex'),sourceRevision:execFileSync('git',['rev-parse','HEAD'],{cwd:root,encoding:'utf8'}).trim(),browser:await browser.version(),screenshots,viewports:[1440,969,390],normalStates:10,exceptionStates:4,externalRequests:0,errors:0,prototypeOnly:true,actualJevCalls:0},null,2));
  } finally {if(browser)await browser.close();await new Promise(resolve=>server.close(resolve));}
}
main().catch(error=>{console.error(error);process.exitCode=1;});
