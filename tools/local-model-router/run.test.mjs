import { test } from 'node:test';
import assert from 'node:assert/strict';
import { safeFailureReason } from './run.mjs';

test('startup diagnostics identify bounded causes without printing exception payloads', () => {
  assert.equal(safeFailureReason({ code: 'ENOENT', message: 'secret-value' }), 'ENOENT');
  assert.equal(safeFailureReason(new Error('Codex probe closed/timed out')), '모델 목록 연결 종료 또는 시간 초과');
  assert.equal(safeFailureReason({ code: 'secret-value', message: 'private prompt or auth token' }), '실행 설정 또는 Codex 연결 오류');
});
