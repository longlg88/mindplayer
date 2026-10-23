import { test } from 'node:test';
import assert from 'node:assert/strict';
import { jevDefaultEnvironment, safeFailureReason } from './run.mjs';
import { childEnvironment } from './service.mjs';

test('startup diagnostics identify bounded causes without printing exception payloads', () => {
  assert.equal(safeFailureReason({ code: 'ENOENT', message: 'secret-value' }), 'ENOENT');
  assert.equal(safeFailureReason(new Error('Codex probe closed/timed out')), '모델 목록 연결 종료 또는 시간 초과');
  assert.equal(safeFailureReason({ code: 'secret-value', message: 'private prompt or auth token' }), '실행 설정 또는 Codex 연결 오류');
});

test('a new session starts with Jev on only when a key loaded', () => {
  assert.deepEqual(jevDefaultEnvironment({ provider: 'gateway' }), { MINDPLAYER_JEV_DEFAULT: 'jev' });
  assert.deepEqual(jevDefaultEnvironment({ provider: 'typesafe' }), { MINDPLAYER_JEV_DEFAULT: 'jev' });
  assert.deepEqual(jevDefaultEnvironment(null), {});
});

test('an inherited Jev default never reaches a child; only the router sets it', () => {
  const clean = childEnvironment({ MINDPLAYER_JEV_DEFAULT: 'jev', PATH: '/bin' });
  assert.equal(clean.MINDPLAYER_JEV_DEFAULT, undefined);
  assert.equal(clean.PATH, '/bin');
});
