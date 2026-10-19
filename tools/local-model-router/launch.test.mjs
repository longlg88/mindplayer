import test from 'node:test';
import assert from 'node:assert/strict';
import { buildLaunchPlan } from './launch.mjs';

const base = { cwd: '/tmp/project', proxyUrl: 'ws://127.0.0.1:4123', initialModel: 'catalog-a' };

test('new launch preserves prompt argv, including dash-leading and Unicode text', () => {
  const prompt = '-한글 prompt 🐈';
  const plan = buildLaunchPlan({ ...base, originalArgs: ['--ask-for-approval', 'never', '--sandbox', 'danger-full-access', prompt] });
  assert.deepEqual(plan.uiArgs, [
    '--remote', base.proxyUrl, '--remote-auth-token-env', 'MINDPLAYER_LOCAL_ROUTER_TOKEN',
    '--model', 'catalog-a', '--cd', base.cwd,
    '--ask-for-approval', 'never', '--sandbox', 'danger-full-access', prompt,
  ]);
  assert.deepEqual(plan.backendArgs, [
    '-c', 'approval_policy="never"', '-c', 'sandbox_mode="danger-full-access"',
  ]);
});

test('resume removes UI permission flags but moves them into backend config', () => {
  const plan = buildLaunchPlan({ ...base, originalArgs: [
    'resume', 'thread-1', '--ask-for-approval=on-request', '--sandbox', 'workspace-write',
  ] });
  assert.deepEqual(plan.uiArgs.slice(8), ['resume', 'thread-1']);
  assert.deepEqual(plan.backendArgs, [
    '-c', 'approval_policy="on-request"', '-c', 'sandbox_mode="workspace-write"',
  ]);
});

test('remote resume without explicit permissions preserves account configuration defaults', () => {
  const resume = buildLaunchPlan({ ...base, originalArgs: ['resume', 'thread-1'] });
  assert.deepEqual(resume.backendArgs, []);
  const fresh = buildLaunchPlan({ ...base, originalArgs: [] });
  assert.deepEqual(fresh.backendArgs, []);
  assert.equal(fresh.uiArgs.includes('read-only'), false);
});

test('restrictive resume profile is not overridden by implicit backend permissions', () => {
  const plan = buildLaunchPlan({ ...base, originalArgs: ['resume', 'thread-1', '--profile', 'read-only-profile'] });
  assert.deepEqual(plan.backendArgs, ['--profile', 'read-only-profile']);
  assert.ok(!JSON.stringify(plan).includes('danger-full-access'));
  assert.ok(!JSON.stringify(plan).includes('approval_policy'));
});

test('same config flags and profile are sent to the backend', () => {
  const plan = buildLaunchPlan({ ...base, originalArgs: [
    '--profile', 'trusted', '-c', 'trust_level="trusted"', '--config=ui_theme="dark"',
  ] });
  assert.deepEqual(plan.backendArgs, [
    '--profile', 'trusted', '-c', 'trust_level="trusted"', '-c', 'ui_theme="dark"',
  ]);
  assert.deepEqual(plan.uiArgs.slice(8), [
    '--profile', 'trusted', '-c', 'trust_level="trusted"', '--config=ui_theme="dark"',
  ]);
});

test('permission config conflict is rejected instead of silently choosing one', () => {
  for (const originalArgs of [
    ['resume', 'thread-1', '--sandbox', 'workspace-write', '-c', 'sandbox_mode="workspace-write"'],
    ['resume', 'thread-1', '-c', 'sandbox_mode="workspace-write"', '--sandbox', 'workspace-write'],
  ]) assert.throws(() => buildLaunchPlan({ ...base, originalArgs }), /cannot combine sandbox_mode/);
});

test('end-of-options makes the remaining original argv opaque', () => {
  const plan = buildLaunchPlan({ ...base, originalArgs: ['--', '--sandbox', 'never', '-dash prompt 🐈'] });
  assert.deepEqual(plan.uiArgs.slice(8), ['--', '--sandbox', 'never', '-dash prompt 🐈']);
  assert.deepEqual(plan.backendArgs, []);
});

test('remote flags in original args are rejected', () => {
  for (const args of [['--remote', 'ws://evil'], ['--remote-auth-token-env', 'EVIL'], ['--remote-auth-token=secret']]) {
    assert.throws(() => buildLaunchPlan({ ...base, originalArgs: args }), /does not accept/);
  }
});

test('original argv is not mutated and required values are validated', () => {
  const originalArgs = ['resume', 'thread-1', '--ask-for-approval', 'never'];
  const snapshot = [...originalArgs];
  buildLaunchPlan({ ...base, originalArgs });
  assert.deepEqual(originalArgs, snapshot);
  assert.throws(() => buildLaunchPlan({ ...base, originalArgs: ['--sandbox'] }), /requires a value/);
  assert.throws(() => buildLaunchPlan({ ...base, originalArgs: ['--profile', '-x'] }), /requires a value/);
});
test('integrated Jev service preserves the native model/config instead of injecting a catalog guess', () => {
  const originalArgs = ['--model', 'selected-by-user', '-c', 'model_reasoning_effort="max"'];
  const plan = buildLaunchPlan({ originalArgs, cwd: '/same', proxyUrl: 'ws://127.0.0.1:1', initialModel: 'catalog-default', preserveNativeModel: true });
  assert.equal(plan.uiArgs.filter(arg => arg === '--model').length, 1);
  assert.ok(!plan.uiArgs.includes('catalog-default'));
  assert.ok(plan.uiArgs.includes('selected-by-user'));
});
