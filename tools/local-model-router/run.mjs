#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { mkdtempSync, openSync, writeSync, closeSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { probeModels } from './app-server.mjs';
import { chooseModels } from './router.mjs';
import { createLocalProxy } from './proxy.mjs';
import { buildLaunchPlan } from './launch.mjs';
import { JevService, childEnvironment, loadJevCredentials } from './service.mjs';
import { createStatusSink } from './status.mjs';
import { privateUiFailureReason, resolveUiBinary } from './private-ui.mjs';
import { CURRENT_CODEX_FAMILY } from './current-family.mjs';

const usage = `로컬 Codex 라우팅 실험 (standalone A/B 규칙; --mindplayer는 세션별 Jev ON/OFF와 원본 권한 유지)
  node tools/local-model-router/run.mjs [--cwd PATH] [--resume THREAD_ID]
  node tools/local-model-router/run.mjs --list-models
  선택 옵션: --simple-model ID --complex-model ID --codex-home PATH
기존 Codex 로그인 사용. 통합 모드는 현재 카탈로그로 모델·effort를 선택하며 명시적 평가 정책이 있으면 우선합니다.
Jev 키·판단 실패는 새 턴을 전송하지 않습니다. 측정된 비용 절감은 보장하지 않습니다.
서비스는 127.0.0.1 전용이며 Codex 자체의 모델 통신은 외부로 나갑니다.`;

let failureStage = '인수·작업 폴더 확인';

// Do not print arbitrary exception payloads: CLI args can contain prompts and
// backend errors can contain credentials. Stage + a bounded reason is useful
// without copying that data into MindPlayer's stderr record.
export function safeFailureReason(error) {
  const privateUiReason = privateUiFailureReason(error);
  if (privateUiReason) return privateUiReason;
  const codes = new Set(['ENOENT', 'EACCES', 'EADDRINUSE', 'ECONNREFUSED', 'ENOTDIR']);
  if (codes.has(error?.code)) return error.code;
  const messages = new Map([
    ['Codex probe closed/timed out', '모델 목록 연결 종료 또는 시간 초과'],
    ['Unexpected model/list response', 'Codex 모델 목록 응답 형식 불일치'],
    ['실제 터미널에서 실행하세요. 테스트는 node --test로 분리되어 있습니다.', '실제 터미널이 필요합니다'],
    ['서로 다른 텍스트 모델 두 개가 필요합니다. --list-models로 후보를 확인하세요.', '사용 가능한 서로 다른 모델 후보가 두 개 필요합니다'],
  ]);
  return messages.get(error?.message) ?? '실행 설정 또는 Codex 연결 오류';
}

// A session starts with Jev on only when a key loaded; without one every Jev turn would be refused before sending.
export function jevDefaultEnvironment(credential) {
  return credential ? { MINDPLAYER_JEV_DEFAULT: 'jev' } : {};
}

async function main() {
  const rawArgs = process.argv.slice(2);
  const separator = rawArgs.indexOf('--');
  const integrated = rawArgs.includes('--mindplayer');
  const perPromptChoice = integrated && process.env.MINDPLAYER_PER_PROMPT_CHOICE === '1';
  const originalArgs = integrated && separator >= 0 ? rawArgs.slice(separator + 1) : [];
  const parserArgs = integrated && separator >= 0 ? rawArgs.slice(0, separator) : rawArgs;
  const { values } = parseArgs({ args: parserArgs, options: {
    mindplayer: { type: 'boolean' },
    cwd: { type: 'string', default: process.cwd() }, resume: { type: 'string' },
    'simple-model': { type: 'string' }, 'complex-model': { type: 'string' },
    'codex-home': { type: 'string' }, 'list-models': { type: 'boolean' }, help: { type: 'boolean', short: 'h' },
  } });
  if (values.help) { console.log(usage); return; }
  const cwd = realpathSync(values.cwd);
  failureStage = '전용 Codex UI 확인';
  let uiBinary = resolveUiBinary({ integrated });
  const credential = integrated ? loadJevCredentials() : null;
  const status = integrated ? createStatusSink(process.env.MINDPLAYER_ROUTER_STATUS) : () => {};
  const env = childEnvironment(process.env);
  let availability = null;
  try { availability = JSON.parse(process.env.MINDPLAYER_ROUTER_AVAILABILITY ?? 'null'); } catch { /* unknown, never a quota estimate */ }
  let proxy;
  let ui;
  let stopping = false;
  let killTimer;
  const probeAbort = new AbortController();
  const signals = ['SIGINT', 'SIGTERM', 'SIGHUP'];
  const stop = signal => {
    if (stopping) return;
    stopping = true;
    process.exitCode = 128 + ({ SIGINT: 2, SIGHUP: 1, SIGTERM: 15 }[signal] ?? 1);
    probeAbort.abort();
    ui?.kill('SIGTERM');
    void proxy?.close();
    killTimer = setTimeout(() => { if (ui?.exitCode === null) ui.kill('SIGKILL'); }, 2000);
    killTimer.unref();
  };
  const signalHandlers = new Map(signals.map(signal => [signal, () => stop(signal)]));
  for (const [signal, handler] of signalHandlers) process.on(signal, handler);
  const removeSignalHandlers = () => signalHandlers.forEach((handler, signal) => process.removeListener(signal, handler));
  if (values['codex-home']) env.CODEX_HOME = realpathSync(values['codex-home']);
  console.log('Codex 모델 후보 확인 중… (목록은 실제 사용 권한 검증이 아닙니다)');
  failureStage = '실행 인수 구성';
  const probePlan = integrated
    ? buildLaunchPlan({ originalArgs, cwd, proxyUrl: 'ws://127.0.0.1:0', initialModel: 'probe', preserveNativeModel: true })
    : null;
  let catalog;
  let account = null;
  try {
    failureStage = 'Codex 모델 목록 확인';
    const result = await probeModels({ binary: uiBinary, cwd, env, signal: probeAbort.signal, includeAccount: integrated, ...(probePlan ? { args: probePlan.backendArgs } : {}) });
    catalog = integrated ? result.catalog : result;
    account = integrated ? result.account : null;
  } catch (error) {
    removeSignalHandlers();
    if (stopping) return;
    throw error;
  }
  if (stopping) { removeSignalHandlers(); return; }
  if (values['list-models']) {
    for (const model of catalog.filter(m => !m.hidden)) console.log(`${model.model}${model.isDefault ? ' (catalog default)' : ''}`);
    removeSignalHandlers();
    return;
  }
  failureStage = '터미널·모델 후보 확인';
  if (!process.stdin.isTTY || !process.stdout.isTTY) throw new Error('실제 터미널에서 실행하세요. 테스트는 node --test로 분리되어 있습니다.');
  const visible = catalog.filter(m => !m.hidden && m.inputModalities?.includes('text'));
  const initial = visible.find(m => m.isDefault) ?? visible[0];
  if (!initial) throw new Error('No text model available');
  // The standalone A/B harness stays explicit. MindPlayer no longer assumes
  // two models or treats catalog position as a cost/quality ordering.
  const pair = integrated ? null : chooseModels(catalog, { simpleModel: values['simple-model'], complexModel: values['complex-model'] });
  const logDir = mkdtempSync(join(tmpdir(), 'mindplayer-local-router-'));
  const logPath = join(logDir, 'routing.jsonl');
  const log = openSync(logPath, 'wx', 0o600);
  const audit = event => {
    // Status remains useful even when the audit disk is full; neither failure
    // is allowed to replay a native turn or expose an upstream error body.
    try { status(event); } catch { /* pane may already have closed */ }
    writeSync(log, JSON.stringify({ time: new Date().toISOString(), ...event }) + '\n');
  };
  try {
    const plan = integrated
      ? buildLaunchPlan({ originalArgs, cwd, proxyUrl: 'ws://127.0.0.1:0', initialModel: initial.model, preserveNativeModel: true })
      : null;
    failureStage = '로컬 중계 연결';
    if (resolveUiBinary({ integrated }) !== uiBinary) throw new Error('Private native install changed during startup');
    proxy = await createLocalProxy({ binary: uiBinary, catalog, pair, cwd, env, audit, ...(integrated ? {
      requireFreshTurn: true,
      decisionTimeoutMs: 31_000,
      serviceFactory: record => new JevService({ catalog, latestFamily: CURRENT_CODEX_FAMILY, accountHome: env.CODEX_HOME, account, availability, ...credential, audit: record, requirePromptChoice: perPromptChoice,
        expectedThreadId: originalArgs[0] === 'resume' ? originalArgs[1] : values.resume ?? null }),
    } : {}), ...(plan ? { backendArgs: plan.backendArgs } : {}) });
    if (stopping) return;
    console.log(perPromptChoice ? (credential ? '세션별 Jev · 새 세션 ON · F6 ON/OFF · Enter 전송' : '세션별 Jev · 키 없음 · 새 세션 OFF · F6 ON/OFF · Enter 전송')
      : integrated
      ? credential ? `Jev 매 새 턴 모델 선택 · ${credential.provider === 'typesafe' ? 'TypeSafe 직접 API' : 'Vercel Gateway'}`
        : 'Jev API 키 없음 · 새 턴은 실행하지 않습니다'
      : `규칙 A: ${pair.simple.model} / 규칙 B: ${pair.complex.model}`);
    console.log(`로컬 연결: ${proxy.url} · 메타데이터 로그: ${logPath}`);
    console.log('모델줄은 선택·요청 수락·실행 중·턴 완료를 구분합니다. 서버 모델 변경 보고는 별도 정보입니다.');
    const args = integrated
      ? buildLaunchPlan({ originalArgs, cwd, proxyUrl: proxy.url, initialModel: initial.model, preserveNativeModel: true }).uiArgs
      : ['--remote', proxy.url, '--remote-auth-token-env', 'MINDPLAYER_LOCAL_ROUTER_TOKEN', '--model', pair.simple.model, '--cd', cwd];
    if (!integrated) {
      if (values.resume) args.push('resume', values.resume);
      else args.push('--sandbox', 'read-only', '--ask-for-approval', 'on-request');
    }
    // Recheck after startup probes: a changed private install must pass the
    // same hash/contract checks immediately before the UI is spawned.
    failureStage = '전용 Codex UI 확인';
    if (resolveUiBinary({ integrated }) !== uiBinary) throw new Error('Private native install changed during startup');
    failureStage = 'Codex UI 실행';
    ui = spawn(uiBinary, args, { cwd, env: { ...env, MINDPLAYER_LOCAL_ROUTER_TOKEN: proxy.token,
      ...(perPromptChoice ? { MINDPLAYER_PER_PROMPT_CHOICE: '1', ...jevDefaultEnvironment(credential) } : {}) }, stdio: 'inherit' });
    const exitCode = await new Promise((resolve, reject) => {
      ui.once('error', reject);
      ui.once('exit', code => resolve(code ?? 1));
    });
    if (!stopping) process.exitCode = exitCode;
  } finally {
    await proxy?.close();
    clearTimeout(killTimer);
    removeSignalHandlers();
    closeSync(log);
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch(error => {
    const hint = privateUiFailureReason(error) ? 'MindPlayer 전용 Codex 설치를 확인하세요.' : 'Codex 로그인·모델 후보·터미널 상태를 확인하세요.';
    console.error(`로컬 라우터 ${failureStage} 실패: ${safeFailureReason(error)}. ${hint}`);
    process.exitCode ||= 1;
  });
}

export { main };
