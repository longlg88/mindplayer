import test from 'node:test';
import assert from 'node:assert/strict';

const moduleUnderTest = () => import('./current-family.mjs');
const NOW = Date.parse('2026-10-28T00:00:00Z');


test('registry snapshots clone and freeze the caller policy and bind its exact version and digest', async () => {
  const { CURRENT_CODEX_FAMILY, snapshotFamilyRegistry, reviewFamilyRegistry } = await moduleUnderTest();
  const supplied = structuredClone(CURRENT_CODEX_FAMILY), saved = snapshotFamilyRegistry(supplied);
  const review = reviewFamilyRegistry(saved, { catalog: [], now: NOW });
  supplied.current.light = 'gpt-99-unreviewed'; supplied.provenance.basis = 'changed';
  assert.equal(saved.current.light, 'gpt-6-luna'); assert.notEqual(saved.provenance.basis, 'changed');
  assert.equal(review.version, saved.version); assert.match(review.digest, /^[a-f0-9]{64}$/);
  assert.deepEqual(review, reviewFamilyRegistry(saved, { catalog: [], now: NOW }));
  const revised = structuredClone(saved); revised.provenance.basis += ' reviewed';
  assert.notEqual(reviewFamilyRegistry(snapshotFamilyRegistry(revised), { catalog: [], now: NOW }).digest, review.digest);
});

test('registry expiry and malformed review metadata require review instead of guessing newer versions', async () => {
  const { CURRENT_CODEX_FAMILY: family, snapshotFamilyRegistry, reviewFamilyRegistry } = await moduleUnderTest();
  for (const now of [Date.parse(family.reviewedAt) - 1, Date.parse(family.reviewDeadline), NaN]) {
    assert.throws(() => reviewFamilyRegistry(family, { catalog: [], now }), { code: 'registry-review-required' });
  }
  for (const patch of [{ version: '' }, { provider: 'other' }, { reviewDeadline: 'never' }, { provenance: null },
    { current: { ...family.current, light: family.current.frontier } }, { legacy: [...family.legacy, family.current.light] },
    { baselineOnly: null }, { baselineOnly: [family.current.light] }, { baselineOnly: [...family.baselineOnly, family.baselineOnly[0]] }]) {
    assert.throws(() => snapshotFamilyRegistry({ ...family, ...patch }), { code: 'registry-review-required' });
  }
});

test('absent defaults are valid; unknown native defaults and non-current upgrade destinations require review', async () => {
  const { CURRENT_CODEX_FAMILY: family, reviewFamilyRegistry } = await moduleUnderTest();
  for (const catalog of [[], [{ model: 'unknown' }], [{ model: 'gpt-5.6-sol', isDefault: true }],
    [{ model: 'gpt-6.1-terra', isDefault: true }],
    [{ model: 'gpt-5.6-sol', upgrade: 'gpt-6-astra', upgradeInfo: { model: 'gpt-6.1-sol' } }]]) {
    assert.doesNotThrow(() => reviewFamilyRegistry(family, { catalog, now: NOW }));
  }
  for (const catalog of [[{ model: 'unknown', isDefault: true }], [{ model: 'gpt-6-astra', upgrade: 'gpt-99-unreviewed' }],
    [{ model: 'gpt-5.6-sol', upgradeInfo: { model: 'gpt-6-sol' } }]]) {
    assert.throws(() => reviewFamilyRegistry(family, { catalog, now: NOW }), { code: 'registry-review-required' });
  }
});
