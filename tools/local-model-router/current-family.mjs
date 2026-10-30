import { createHash } from 'node:crypto';

const fail = () => { throw Object.assign(new Error('registry-review-required'), { code: 'registry-review-required' }); };
const nonempty = value => typeof value === 'string' && value.trim().length > 0;
const modelId = value => typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$/.test(value);
const timestamp = value => typeof value === 'string' && /^\d{4}-\d{2}-\d{2}T.*Z$/.test(value) ? Date.parse(value) : NaN;
function freeze(value) {
  if (value && typeof value === 'object') { Object.values(value).forEach(freeze); Object.freeze(value); }
  return value;
}
function validate(registry) {
  const roles = ['frontier', 'standard', 'light'], baselines = registry?.baselineOnly === undefined ? [] : registry.baselineOnly;
  if (registry?.schemaVersion !== 1 || registry.provider !== 'codex' || !nonempty(registry.version) || registry.version.length > 128
    || !Number.isFinite(timestamp(registry.reviewedAt)) || !(timestamp(registry.reviewDeadline) > timestamp(registry.reviewedAt))
    || !registry.current || Object.keys(registry.current).length !== roles.length || !roles.every(role => modelId(registry.current[role]))
    || !Array.isArray(registry.legacy) || registry.legacy.length > 64 || !registry.legacy.every(modelId)
    || !Array.isArray(baselines) || baselines.length > 64 || !baselines.every(modelId)
    || new Set([...Object.values(registry.current), ...registry.legacy, ...baselines]).size !== roles.length + registry.legacy.length + baselines.length
    || !['source', 'cliVersion', 'observedOn', 'basis', 'limitation'].every(key => nonempty(registry.provenance?.[key]))) fail();
}

/** A reviewed native runtime set, not a public release or entitlement claim.
 * Maintenance is deliberately bounded: manually review before reviewDeadline;
 * absent generation metadata cannot establish perpetual latestness. */
export const CURRENT_CODEX_FAMILY = freeze({
  schemaVersion: 1,
  version: 'codex-native-0.162.1-2026-10-28',
  provider: 'codex',
  reviewedAt: '2026-10-27T21:46:06Z',
  reviewDeadline: '2026-11-18T00:00:00Z',
  provenance: {
    source: 'native-private-cli-catalog', cliVersion: '0.162.1', observedOn: '2026-10-28',
    basis: 'Runtime model/list descriptions and cached native catalog; no explicit generation metadata or Terra capability ranking.',
    limitation: 'Runtime evidence, not a public release guarantee. Manual review required by reviewDeadline.',
  },
  current: { frontier: 'gpt-6-astra', standard: 'gpt-6.1-sol', light: 'gpt-6-luna' },
  baselineOnly: ['gpt-6.1-terra'],
  legacy: ['gpt-6-sol', 'gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna'],
});

export function snapshotFamilyRegistry(registry) {
  try {
    const snapshot = structuredClone(registry);
    validate(snapshot);
    return freeze(snapshot);
  } catch { return fail(); }
}

/** Metadata guard only. The caller must still intersect native eligibility;
 * neither catalog ordering nor numeric model names establish membership. */
export function reviewFamilyRegistry(registry, { catalog, now } = {}) {
  validate(registry);
  if (!Number.isFinite(now) || now < timestamp(registry.reviewedAt) || now >= timestamp(registry.reviewDeadline)
    || !Array.isArray(catalog)) fail();
  const currentModels = Object.values(registry.current), baselineOnlyModels = registry.baselineOnly ?? [];
  const known = [...currentModels, ...baselineOnlyModels, ...registry.legacy];
  for (const row of catalog) {
    if (row?.isDefault === true && !known.includes(row.model)) fail();
    // Native Model.upgrade and Model.upgradeInfo.model are optional; an absent
    // isDefault/upgrade field is valid and supplies no freshness evidence.
    if ([row?.upgrade, row?.upgradeInfo?.model].some(target => target != null && !currentModels.includes(target))) fail();
  }
  return freeze({ version: registry.version, digest: createHash('sha256').update(JSON.stringify(registry)).digest('hex'),
    currentModels, baselineOnlyModels: [...baselineOnlyModels], frontier: registry.current.frontier, legacyModels: [...registry.legacy] });
}
