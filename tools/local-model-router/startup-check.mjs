#!/usr/bin/env node
import { pathToFileURL } from 'node:url';
import { loadJevCredentials, loadPolicy } from './service.mjs';
import { resolveUiBinary } from './private-ui.mjs';

/** Local readiness is not authentication success. Use the runtime's private
 * loaders and UI contract without starting a service or contacting a provider. */
export function checkStartup() {
  try {
    const loaded = loadPolicy();
    // This is the same explicit veto checked before JevService's key/consent.
    if (loaded?.policy?.consent?.externalPromptTransfer === false) {
      return { ok: false, reason: 'external-transfer-policy-unset' };
    }
    if (!loadJevCredentials()) return { ok: false, reason: 'jev-key-unavailable' };
    resolveUiBinary({ integrated: true });
    return { ok: true, reason: 'ready' };
  } catch (error) {
    const reasons = new Map([
      ['policy-invalid', 'policy-invalid'],
      ['ERR_PRIVATE_UI_MISSING', 'private-ui-missing'],
      ['ERR_PRIVATE_UI_MANIFEST', 'private-ui-invalid'],
      ['ERR_PRIVATE_UI_PATH', 'private-ui-invalid'],
      ['ERR_PRIVATE_UI_HASH', 'private-ui-invalid'],
      ['ERR_PRIVATE_UI_CONTRACT', 'private-ui-invalid'],
    ]);
    return { ok: false, reason: reasons.get(error?.code) ?? 'startup-check-failed' };
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const result = checkStartup();
  process.stdout.write(`${JSON.stringify(result)}\n`);
  process.exitCode = result.ok ? 0 : 1;
}
