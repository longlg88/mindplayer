// Policy inputs are owned by the router. Prompt/history strings are only data.
// No I/O, implicit clock, catalog ordering, model tiers, or price defaults.
const TASKS = ['bounded_routine', 'multistep_coordination', 'open_problem'];
const GROUPS = ['ko', 'en', 'short-followup', 'ordinary', 'high-consequence'];
const nonempty = value => typeof value === 'string' && value.trim().length > 0;
const probability = value => Number.isFinite(value) && value >= 0 && value <= 1;
const count = value => Number.isSafeInteger(value) && value >= 0;
const strings = value => Array.isArray(value) && value.every(nonempty);
const includesAll = (supported, required) => strings(supported) && required.every(value => supported.includes(value));

export class PolicyError extends Error {
  constructor(code) { super(code); this.name = 'PolicyError'; this.code = code; }
}

function instant(value) {
  if (typeof value === 'number') return Number.isFinite(value) ? value : NaN;
  return typeof value === 'string' && /^\d{4}-\d{2}-\d{2}T/.test(value) ? Date.parse(value) : NaN;
}
function clock(now) {
  const time = instant(now);
  if (!Number.isFinite(time)) throw new PolicyError('INVALID_POLICY_TIME');
  return time;
}
function fresh(value, field, now) {
  const measured = instant(value?.[field]), expires = instant(value?.expiresAt);
  return measured <= now && expires > now && measured < expires;
}
const configuration = value => nonempty(value?.model) && (value.effort === null || nonempty(value.effort));
const same = (a, b) => a?.model === b?.model && a?.effort === b?.effort
  && (b?.provider === undefined || a?.provider === b.provider)
  && (b?.accountFingerprint === undefined || a?.accountFingerprint === b.accountFingerprint);
const identity = value => JSON.stringify([value.provider, value.accountFingerprint, value.model, value.effort]);
function available(profile, now) {
  return profile?.schemaVersion === 1 && nonempty(profile.version) && profile.allowed === true
    && configuration(profile) && nonempty(profile.provider) && nonempty(profile.accountFingerprint)
    && profile.availability?.status === 'available' && nonempty(profile.availability.source)
    && fresh(profile.availability, 'verifiedAt', now);
}

function textOnly(value) {
  if (typeof value === 'string') return { text: value, attachments: false };
  if (!Array.isArray(value)) return { text: '', attachments: value != null };
  const parts = []; let attachments = false;
  for (const item of value) {
    if (item?.type === 'text' && typeof item.text === 'string') parts.push(item.text);
    else attachments = true;
  }
  return { text: parts.join('\n'), attachments };
}

/** Caller supplies history from this thread only; no metadata/paths are copied.
 * Assistant history needs phase: 'final_answer'/'final' or final: true.
 * maxBytes bounds the whole JSON state, including escaping and metadata.
 */
export function buildSnapshot({ input, history = [], baseline, contextKnown = false, maxBytes = 16384, maxPairs = 4 } = {}) {
  if (!configuration(baseline)) throw new PolicyError('INVALID_BASELINE');
  if (!count(maxBytes) || !count(maxPairs) || !Array.isArray(history)) throw new PolicyError('INVALID_SNAPSHOT_LIMIT');
  const current = textOnly(input), pairs = [];
  let attachments = current.attachments, truncated = false;
  for (const item of history) {
    if (item?.role !== 'user' && !(item?.role === 'assistant'
      && (item.phase === 'final_answer' || item.phase === 'final' || item.final === true))) continue;
    const content = textOnly(item.content);
    const text = typeof item.text === 'string' ? item.text : content.text;
    attachments ||= content.attachments || item.attachments === true || (Array.isArray(item.attachments) && item.attachments.length > 0);
    if (!text) continue;
    const message = { role: item.role, text };
    if (item.role === 'user') pairs.push([message]);
    else if (pairs.at(-1)?.length === 1) pairs.at(-1).push(message);
    else truncated = true;
  }
  if (pairs.some(pair => pair.length !== 2)) truncated = true;
  if (pairs.length > maxPairs) { pairs.splice(0, pairs.length - maxPairs); truncated = true; }
  const snapshot = {
    schemaVersion: 1, currentInput: current.text, recent: pairs.flat(),
    baseline: { model: baseline.model, effort: baseline.effort },
    contextKnown: contextKnown === true && !truncated && !attachments, truncated, attachments,
  };
  const bytes = () => Buffer.byteLength(JSON.stringify(snapshot), 'utf8');
  const markTruncated = () => { snapshot.truncated = true; snapshot.contextKnown = false; };
  while (bytes() > maxBytes && pairs.length) {
    pairs.shift(); snapshot.recent = pairs.flat(); markTruncated();
  }
  if (bytes() > maxBytes) {
    markTruncated(); snapshot.currentInput = '';
    if (bytes() > maxBytes) throw new PolicyError('SNAPSHOT_LIMIT_TOO_SMALL');
    let low = 0, high = current.text.length;
    const prefix = length => {
      const part = current.text.slice(0, length);
      return /[\uD800-\uDBFF]$/.test(part) ? part.slice(0, -1) : part;
    };
    while (low < high) {
      const middle = Math.ceil((low + high) / 2);
      snapshot.currentInput = prefix(middle);
      if (bytes() <= maxBytes) low = middle;
      else high = middle - 1;
    }
    snapshot.currentInput = prefix(low);
  }
  return snapshot;
}

/** Version 1 profiles are one explicitly allowed provider/account/model/effort.
 * availability: {status:'available',source,verifiedAt,expiresAt}
 * capabilities: {inputModalities:string[],tools:string[],contextWindow:number}
 * requirements.contextTokens is the verified full thread requirement, not just
 * the current input. Omit it when unknown: baseline can remain, switches cannot.
 */
export function filterConfigurations({ catalog, profiles, accountFingerprint, now, requirements = {} } = {}) {
  const time = clock(now);
  if (!Array.isArray(catalog) || !Array.isArray(profiles) || !nonempty(accountFingerprint)) throw new PolicyError('INVALID_POLICY_INPUT');
  if (!requirements || typeof requirements !== 'object' || Array.isArray(requirements)) throw new PolicyError('INVALID_REQUIREMENTS');
  if (requirements.ordinaryUsageAllowed === false || requirements.usageBlocked === true) throw new PolicyError('USAGE_BLOCKED');
  const modalities = requirements.inputModalities ?? ['text'], tools = requirements.tools ?? [];
  if (!strings(modalities) || !modalities.length || !strings(tools)
    || (requirements.contextTokens !== undefined && !count(requirements.contextTokens))) throw new PolicyError('INVALID_REQUIREMENTS');
  const requirementsKnown = strings(requirements.inputModalities) && strings(requirements.tools) && count(requirements.contextTokens);
  const unique = new Map();
  for (const profile of profiles) {
    if (!profile || typeof profile !== 'object') continue;
    const key = identity(profile);
    unique.set(key, unique.has(key) ? null : profile);
  }
  return [...unique.values()].filter(profile => {
    if (!available(profile, time) || profile.accountFingerprint !== accountFingerprint
      || (requirements.provider !== undefined && profile.provider !== requirements.provider)) return false;
    const cap = profile.capabilities;
    if (!includesAll(cap?.inputModalities, modalities) || !includesAll(cap?.tools, tools)
      || !count(cap?.contextWindow) || cap.contextWindow === 0 || cap.contextWindow < (requirements.contextTokens ?? 0)) return false;
    const models = catalog.filter(model => model?.model === profile.model
      && (model.provider === undefined || model.provider === profile.provider));
    return models.length > 0 && models.every(model => {
      if (model.hidden === true || model.accessDenied === true || model.available === false
        || ['denied', 'access_denied', 'blocked'].includes(model.availability?.status ?? model.availability)
        || (model.accountFingerprint !== undefined && model.accountFingerprint !== accountFingerprint)
        || !includesAll(model.inputModalities, modalities)) return false;
      const efforts = model.supportedReasoningEfforts;
      if (!Array.isArray(efforts)) return false;
      if (profile.effort === null ? !(efforts.length === 0 && model.defaultReasoningEffort === null)
        : !efforts.some(effort => (typeof effort === 'string' ? effort : effort?.reasoningEffort) === profile.effort)) return false;
      const advertisedTools = model.supportedTools ?? model.tools;
      return (advertisedTools === undefined || includesAll(advertisedTools, tools))
        && (model.contextWindow === undefined || (count(model.contextWindow) && model.contextWindow >= (requirements.contextTokens ?? 0)));
    });
  }).map(profile => ({ ...structuredClone(profile), requirementsKnown }));
}

function thresholdsValid(thresholds, now) {
  return thresholds?.schemaVersion === 1 && nonempty(thresholds.version) && nonempty(thresholds.evidence)
    && nonempty(thresholds.qualityVersion) && fresh(thresholds, 'evaluatedAt', now)
    && count(thresholds.minSamples) && thresholds.minSamples > 0 && count(thresholds.sampleSize)
    && thresholds.sampleSize >= thresholds.minSamples && includesAll(thresholds.groups, GROUPS)
    && ['capabilityConfidence', 'contextSufficient', 'highConsequence', 'qualityFloor', 'highRiskQualityFloor', 'nonInferiorityMargin']
      .every(key => probability(thresholds[key]))
    && thresholds.capabilityConfidence > 0 && thresholds.contextSufficient > 0
    && thresholds.qualityFloor > 0 && thresholds.highRiskQualityFloor >= thresholds.qualityFloor;
}
function quality(profile, task, thresholds, now, highRisk = false) {
  const evaluation = profile.quality, result = evaluation?.tasks?.[task];
  if (evaluation?.version !== thresholds.qualityVersion || !nonempty(evaluation.evidence)
    || !fresh(evaluation, 'evaluatedAt', now) || !count(evaluation.sampleSize) || evaluation.sampleSize < thresholds.minSamples
    || !count(result?.sampleSize) || result.sampleSize < thresholds.minSamples || !probability(result.lowerBound)) return null;
  if (!highRisk) return result.lowerBound;
  return count(result.highRiskSampleSize) && result.highRiskSampleSize >= thresholds.minSamples
    && probability(result.highRiskLowerBound) ? result.highRiskLowerBound : null;
}
function cost(profile, task, now) {
  const measurement = profile.cost, billing = profile.billing;
  if (!measurement || !billing || measurement.regime !== billing.regime || measurement.accountFingerprint !== profile.accountFingerprint
    || !nonempty(measurement.source) || !nonempty(measurement.comparisonKey) || !fresh(measurement, 'measuredAt', now)
    || measurement.scope !== 'all-in-turn' || !Number.isFinite(measurement.totals?.[task]) || measurement.totals[task] < 0) return null;
  if (billing.regime === 'subscription') {
    if (measurement.unit !== 'relative-quota' || !nonempty(billing.pool) || measurement.pool !== billing.pool) return null;
  } else if (billing.regime === 'api') {
    // Execution API approval belongs to deployment policy, never inferred from a price.
    if (billing.authorized !== true || !nonempty(billing.currency) || measurement.unit !== billing.currency) return null;
  } else return null;
  return measurement.totals[task];
}
function comparable(a, b, task, now) {
  return a.provider === b.provider && a.accountFingerprint === b.accountFingerprint
    && cost(a, task, now) !== null && cost(b, task, now) !== null
    && a.billing.regime === b.billing.regime && a.cost.unit === b.cost.unit
    && a.cost.pool === b.cost.pool && a.cost.comparisonKey === b.cost.comparisonKey;
}
function eligible(candidates, baseline, now) {
  const live = Array.isArray(candidates) ? candidates.filter(candidate => available(candidate, now)) : [];
  if (!live.length) throw new PolicyError('NO_ELIGIBLE_CONFIGURATION');
  const matches = configuration(baseline) ? live.filter(candidate => same(candidate, baseline)) : [];
  if (matches.length !== 1) throw new PolicyError('BASELINE_INELIGIBLE');
  const base = matches[0];
  return { base, live: live.filter(candidate => candidate.provider === base.provider && candidate.accountFingerprint === base.accountFingerprint) };
}

/** Comparison readiness does not need a Jev judgment. It gates applying a
 * model change, not the separate every-user-turn Jev classification.
 * Threshold v1 fields (none have policy defaults):
 * {schemaVersion:1,version,evidence,evaluatedAt,expiresAt,sampleSize,groups,
 *  minSamples,qualityVersion,capabilityConfidence,contextSufficient,
 *  highConsequence,qualityFloor,highRiskQualityFloor,nonInferiorityMargin}
 * groups must cover ko/en/short-followup/ordinary/high-consequence. Quality:
 * {version,evidence,evaluatedAt,expiresAt,sampleSize,tasks:{[capability]:result}}
 */
export function canCompareConfigurations(candidates, baseline, { thresholds, now } = {}) {
  const time = clock(now), { base, live } = eligible(candidates, baseline, time);
  if (live.length === 1) return { canCompare: false, reason: 'single-candidate' };
  if (!thresholdsValid(thresholds, time)) return { canCompare: false, reason: 'thresholds-unverified' };
  const canCompare = base.requirementsKnown === true && live.some(candidate => !same(candidate, base) && candidate.requirementsKnown === true
    && TASKS.some(task => {
      const baseQuality = quality(base, task, thresholds, time), candidateQuality = quality(candidate, task, thresholds, time);
      return baseQuality !== null && candidateQuality !== null
        && candidateQuality >= Math.max(thresholds.qualityFloor, baseQuality - thresholds.nonInferiorityMargin)
        && comparable(candidate, base, task, time);
    }));
  return { canCompare, reason: canCompare ? 'comparable-alternative' : 'comparison-unverified' };
}

function judgmentValid(judgment) {
  return judgment && [...TASKS, 'unknown'].includes(judgment.capability) && probability(judgment.confidence)
    && probability(judgment.contextSufficient) && probability(judgment.highConsequence)
    && judgment.probabilities && Object.keys(judgment.probabilities).length === 4
    && [...TASKS, 'unknown'].every(task => Object.hasOwn(judgment.probabilities, task) && probability(judgment.probabilities[task]))
    && Math.abs(Object.values(judgment.probabilities).reduce((sum, value) => sum + value, 0) - 1) <= 1e-6
    && judgment.probabilities[judgment.capability] === Math.max(...Object.values(judgment.probabilities));
}
function latency(profile, now) {
  const measurement = profile.latency;
  return measurement && nonempty(measurement.source) && nonempty(measurement.comparisonKey)
    && fresh(measurement, 'measuredAt', now) && count(measurement.sampleSize) && measurement.sampleSize > 0
    && Number.isFinite(measurement.p50Ms) && measurement.p50Ms >= 0 ? measurement : null;
}

/** Thresholds and quality lower bounds are evaluated evidence, not Jev success
 * probabilities. Unknown/partial context and failed judgments keep exact base.
 * quality task results: {lowerBound,sampleSize,highRiskLowerBound?,highRiskSampleSize?}
 * cost: {regime,unit,accountFingerprint,pool?,comparisonKey,source,measuredAt,
 *        expiresAt,scope:'all-in-turn',totals:{[capability]:number}}
 */
export function chooseConfiguration({ candidates, baseline, judgment, thresholds, context, now } = {}) {
  const time = clock(now), { base, live } = eligible(candidates, baseline, time);
  const result = (selected, reason) => ({ model: selected.model, effort: selected.effort, reason, changed: !same(selected, base) });
  const keep = reason => result(base, reason);
  if (context?.ordinaryUsageAllowed === false || context?.usageBlocked === true) throw new PolicyError('USAGE_BLOCKED');
  if (live.length === 1) return keep('single-candidate');
  if (!thresholdsValid(thresholds, time)) return keep('thresholds-unverified');
  if (!judgmentValid(judgment)) return keep('judgment-unverified');
  if (judgment.capability === 'unknown') return keep('capability-unknown');
  if (judgment.confidence < thresholds.capabilityConfidence) return keep('judgment-uncertain');
  if (context?.contextKnown !== true || context.truncated !== false || context.attachments !== false || base.requirementsKnown !== true) return keep('context-incomplete');
  if (judgment.contextSufficient < thresholds.contextSufficient) return keep('context-insufficient');
  const task = judgment.capability, highRisk = judgment.highConsequence >= thresholds.highConsequence;
  const baseQuality = quality(base, task, thresholds, time, highRisk);
  if (baseQuality === null) return keep('quality-unverified');
  const floor = Math.max(highRisk ? thresholds.highRiskQualityFloor : thresholds.qualityFloor,
    baseQuality - (highRisk ? 0 : thresholds.nonInferiorityMargin));
  const qualified = live.filter(candidate => candidate.requirementsKnown === true
    && quality(candidate, task, thresholds, time, highRisk) !== null
    && quality(candidate, task, thresholds, time, highRisk) >= floor);
  const baseQualified = qualified.includes(base);
  const measured = qualified.filter(candidate => comparable(candidate, base, task, time));
  if (!measured.length) {
    if (!baseQualified) throw new PolicyError('QUALITY_FLOOR_UNMET');
    return keep('cost-unverified');
  }
  if (measured.length === 1 && measured[0] === base) {
    return keep(qualified.some(candidate => candidate !== base) ? 'cost-unverified' : 'no-qualified-alternative');
  }
  const minimum = Math.min(...measured.map(candidate => cost(candidate, task, time)));
  const tied = measured.filter(candidate => cost(candidate, task, time) === minimum);
  if (tied.length === 1) return result(tied[0], same(tied[0], base) ? 'baseline-optimal' : 'minimum-comparable-cost');
  const latencies = tied.map(candidate => latency(candidate, time));
  if (latencies.every(value => value && value.comparisonKey === latencies[0].comparisonKey)) {
    const fastest = Math.min(...latencies.map(value => value.p50Ms));
    const winners = tied.filter((_, index) => latencies[index].p50Ms === fastest);
    if (winners.length === 1) return result(winners[0], same(winners[0], base) ? 'baseline-optimal' : 'measured-latency-tie');
  }
  if (!baseQualified) throw new PolicyError('AMBIGUOUS_QUALITY_CONFIGURATION');
  return keep('unresolved-cost-tie');
}
