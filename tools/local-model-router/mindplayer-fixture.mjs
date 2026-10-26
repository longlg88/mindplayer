#!/usr/bin/env node
// Synthetic offline Codex server/UI: current-looking IDs are compatibility
// fixtures, not proof of genuine model availability, inference, or effort.
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createInterface } from 'node:readline';
import { pathToFileURL } from 'node:url';

const args = process.argv.slice(2);
const markerDir = process.env.MINDPLAYER_ROUTER_MARKER_DIR;

// This is a synthetic composer for Rust/embedded-router smoke coverage. The
// private Codex composer's real editing/queue protocol is verified separately.
export function createPromptComposer({ send, render, params, idPrefix = `fixture-${process.pid}` }) {
  let draft = '', mode = process.env.MINDPLAYER_JEV_DEFAULT === 'jev' ? 'jev' : 'native';
  let buffered = '', pasted = false, nextId = 100, submission = 1;
  const identity = () => `${idPrefix}-${submission}`;
  const payload = () => ({ ...params(), input: [{ type: 'text', text: draft }], clientUserMessageId: identity() });
  const state = () => ({ draft, mode });
  const paint = () => render(state());
  const edit = text => { draft += text; };
  paint();
  return {
    state,
    input(chunk) {
      buffered += chunk;
      const keys = ['\x1b[200~', '\x1b[201~', '\x1b[17~', '\x1b[18~'];
      while (buffered) {
        if (pasted) {
          const end = buffered.indexOf('\x1b[201~');
          if (end < 0) return;
          edit(buffered.slice(0, end));
          buffered = buffered.slice(end + 6); pasted = false;
          continue;
        }
        const key = keys.find(value => buffered.startsWith(value));
        if (key) {
          buffered = buffered.slice(key.length);
          if (key === '\x1b[200~') pasted = true;
          else if (key === '\x1b[17~') mode = mode === 'jev' ? 'native' : 'jev';
          // Unsupported F7 is consumed without changing the draft or mode.
          paint(); continue;
        }
        if (keys.some(value => value.startsWith(buffered))) return;
        const character = String.fromCodePoint(buffered.codePointAt(0));
        buffered = buffered.slice(character.length);
        if (character === '\r' || character === '\n') {
          if (draft) {
            const choice = { mode, includeRecent: false };
            send({ id: nextId++, method: 'turn/start', params: { ...payload(), mindplayerRoutingChoice: choice } });
            draft = ''; submission++;
          }
        } else if (character === '\x15') {
          draft = ''; submission++;
        } else if (character === '\x7f' || character === '\b') {
          draft = Array.from(draft).slice(0, -1).join('');
        } else if (character >= ' ') edit(character);
      }
      paint();
    },
  };
}

const models = [
  // Legacy entries deliberately precede the current family. Catalog position
  // must not let them enter the integrated Jev configuration menu.
  { model: 'gpt-5.4', id: 'gpt-5.4', inputModalities: ['text'], defaultReasoningEffort: 'low',
    description: 'Synthetic legacy routine configuration.', supportedReasoningEfforts: [{ reasoningEffort: 'low', description: 'Light reasoning' }] },
  { model: 'gpt-5.3-codex', id: 'gpt-5.3-codex', inputModalities: ['text'], defaultReasoningEffort: 'high',
    description: 'Synthetic legacy complex configuration.', supportedReasoningEfforts: [{ reasoningEffort: 'high', description: 'Deep reasoning' }] },
  { model: 'gpt-6-luna', id: 'gpt-6-luna', isDefault: true, inputModalities: ['text'], defaultReasoningEffort: 'low',
    description: 'Synthetic fast configuration for bounded routine tasks.', supportedReasoningEfforts: [{ reasoningEffort: 'low', description: 'Light reasoning' }] },
  { model: 'gpt-6.1-sol', id: 'gpt-6.1-sol', inputModalities: ['text'], defaultReasoningEffort: 'medium',
    description: 'Synthetic standard configuration for connected tasks.', supportedReasoningEfforts: [{ reasoningEffort: 'medium', description: 'Standard reasoning' }] },
  { model: 'gpt-6-astra', id: 'gpt-6-astra', inputModalities: ['text'], defaultReasoningEffort: 'high',
    description: 'Synthetic configuration for open problems and design tradeoffs.', supportedReasoningEfforts: [{ reasoningEffort: 'high', description: 'Deep reasoning' }] },
];

function reply(message, result) {
  process.stdout.write(`${JSON.stringify({ id: message.id, result })}\n`);
}

function appServer() {
  const capacityFailure = existsSync(`${markerDir}/synthetic-capacity-failure`);
  let capacityFailed = false;
  writeFileSync(`${markerDir}/app-server.pid`, `${process.pid}\n`);
  writeFileSync(`${markerDir}/backend-env.json`, JSON.stringify({ gatewayKeyPresent: !!process.env.AI_GATEWAY_API_KEY, typesafeKeyPresent: !!process.env.TYPESAFE_API_KEY }));
  let email = 'fixture@example.com';
  let percent = 7;
  let cap = 0;
  if (process.env.MINDPLAYER_ROUTER_QUOTA === '1') {
    const isolated = !process.env.CODEX_HOME?.endsWith('/.codex');
    email = isolated ? 'two@example.com' : 'one@example.com';
    percent = isolated ? 65 : 10;
    cap = isolated ? 500 : 0;
    try { percent = Number(readFileSync(`${process.env.CODEX_HOME}/usage-percent`, 'utf8').trim()); } catch {}
    try { email = readFileSync(`${process.env.CODEX_HOME}/usage-email`, 'utf8').trim(); } catch {}
  }
  const input = createInterface({ input: process.stdin });
  input.on('line', line => {
    let message;
    try { message = JSON.parse(line); } catch { process.exitCode = 2; return; }
    if (message.method === 'initialize') reply(message, { serverInfo: { name: 'mindplayer-fixture' } });
    else if (message.method === 'model/list') {
      writeFileSync(`${markerDir}/model-catalog.json`, JSON.stringify(models));
      reply(message, { data: models, nextCursor: null });
    }
    else if (message.method === 'account/read') reply(message, { account: { type: 'chatgpt', email, planType: 'business' } });
    else if (message.method === 'thread/start' || message.method === 'thread/resume') reply(message, {
      thread: { id: message.params.threadId ?? 'fixture-thread', source: 'cli', ephemeral: message.params.ephemeral ?? false },
      model: 'gpt-6-luna', reasoningEffort: 'low',
    });
    else if (message.method === 'account/rateLimits/read') reply(message, {
      accountId: 'fixture-account', ordinaryUsageAllowed: true,
      rateLimits: { primary: { usedPercent: percent, windowDurationMins: 10080 }, individualLimit: { limit: String(cap), used: String(cap), remainingPercent: 0 }, spendControlReached: cap > 0 },
    });
    else if (message.method === 'turn/start') {
      const text = message.params?.input?.map(item => item.text ?? '').join('\n') ?? '';
      const label = text === 'A' ? 'A' : text === 'analyze B' ? 'B' : text === 'C' ? 'C' : 'bootstrap';
      appendFileSync(`${markerDir}/routes.jsonl`, `${JSON.stringify({ label, model: message.params.model, effort: message.params.effort,
        requestId: message.id, params: message.params,
        threadId: message.params.threadId, cwd: message.params.cwd ?? null,
        clientUserMessageId: message.params.clientUserMessageId ?? null,
        routingChoicePresent: Object.hasOwn(message.params, 'mindplayerRoutingChoice') })}\n`);
      reply(message, { turn: { id: `fixture-turn-${label}` } });
      const overloaded = capacityFailure && !capacityFailed && label === 'A' && message.params.model === 'gpt-6-luna';
      if (overloaded) {
        capacityFailed = true;
        process.stdout.write(`${JSON.stringify({ method: 'error', params: { threadId: message.params.threadId,
          turnId: `fixture-turn-${label}`, willRetry: false,
          error: { message: 'Synthetic server capacity failure', codexErrorInfo: 'serverOverloaded' } } })}\n`);
      }
      process.stdout.write(`${JSON.stringify({ method: 'turn/completed', params: { threadId: message.params.threadId, turn: { id: `fixture-turn-${label}`, status: overloaded ? 'failed' : 'completed' } } })}\n`);
    } else if (message.id !== undefined) reply(message, {});
  });
}

function remoteUi() {
  if (process.env.MINDPLAYER_PER_PROMPT_CHOICE !== '1') throw new Error('private v4 UI requires per-message routing metadata');
  const remote = args[args.indexOf('--remote') + 1];
  const tokenEnv = args[args.indexOf('--remote-auth-token-env') + 1];
  if (!remote || !tokenEnv || !process.env[tokenEnv]) throw new Error('remote URL/token missing');
  const cwd = args[args.indexOf('--cd') + 1];
  writeFileSync(`${markerDir}/ui.pid`, `${process.pid}\n`);
  writeFileSync(`${markerDir}/ui.json`, JSON.stringify({ cwd, codexHome: process.env.CODEX_HOME ?? null, args, gatewayKeyPresent: !!process.env.AI_GATEWAY_API_KEY, typesafeKeyPresent: !!process.env.TYPESAFE_API_KEY }));
  writeFileSync(`${markerDir}/proxy-url`, `${remote}\n`);

  const ws = new WebSocket(remote, { headers: { Authorization: `Bearer ${process.env[tokenEnv]}` } });
  let nextId = 1;
  let initialized = false;
  let composer;
  let revision = 0;
  let threadId = args.includes('resume') ? args[args.indexOf('resume') + 1] : 'fixture-thread';
  ws.addEventListener('open', () => {
    ws.send(JSON.stringify({ id: nextId++, method: 'initialize', params: { clientInfo: { name: 'mindplayer-fixture' } } }));
  });
  ws.addEventListener('message', event => {
    let message;
    try { message = JSON.parse(event.data); } catch { return; }
    if (message.id === 1 && message.result) {
      ws.send(JSON.stringify({ method: 'initialized', params: {} }));
      ws.send(JSON.stringify({ id: nextId++, method: args.includes('resume') ? 'thread/resume' : 'thread/start', params: { threadId, ephemeral: false, sessionStartSource: 'startup' } }));
    } else if (message.id === 2 && message.result?.thread) {
      threadId = message.result.thread.id;
      initialized = true;
      process.stdout.write('\x1b[?2004hPANE-READY\r\n');
      composer = createPromptComposer({
        params: () => ({ threadId, cwd, model: 'gpt-6-luna', effort: 'low' }),
        send(request) {
          appendFileSync(`${markerDir}/ui-requests.jsonl`, `${JSON.stringify(request)}\n`, { mode: 0o600 });
          ws.send(JSON.stringify(request));
        },
        render({ draft, mode }) {
          writeFileSync(`${markerDir}/composer-state.json`, JSON.stringify({ revision: ++revision, draft, mode }));
          const label = mode === 'jev' ? '[F6] Jev ON · 외부 전송 동의' : '[F6] Jev OFF · 기본 모델 유지';
          process.stdout.write(`\x1b[2J\x1b[HPANE-READY\r\n${label}\r\n> ${draft}`);
        },
      });
      composer.input(buffered); buffered = '';
    } else if (initialized) {
      if ((message.method === 'error' || message.params?.turn?.status === 'failed')
          && existsSync(`${markerDir}/synthetic-capacity-failure`)) {
        appendFileSync(`${markerDir}/capacity-events.jsonl`, `${JSON.stringify(message)}\n`, { mode: 0o600 });
      }
      if (message.error) {
        appendFileSync(`${markerDir}/ui-errors.jsonl`, `${JSON.stringify(message)}\n`, { mode: 0o600 });
        process.stdout.write(`\r\nPROMPT-ERROR ${message.error.message}\r\n`);
      }
    }
  });
  ws.addEventListener('error', () => process.exitCode = 3);
  ws.addEventListener('close', () => process.exit());

  let buffered = '';
  if (process.stdin.isTTY) process.stdin.setRawMode(true);
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', chunk => {
    if (initialized) composer.input(chunk);
    else buffered += chunk;
  });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  if (!markerDir) throw new Error('MINDPLAYER_ROUTER_MARKER_DIR is required');
  mkdirSync(markerDir, { recursive: true });
  if (args.includes('app-server')) appServer();
  else remoteUi();
}
