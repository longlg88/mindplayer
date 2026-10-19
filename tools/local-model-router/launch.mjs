const REMOTE_OPTIONS = new Set(['--remote', '--remote-auth-token-env', '--remote-auth-token']);
const PERMISSION_OPTIONS = new Map([
  ['--ask-for-approval', 'approval_policy'],
  ['--sandbox', 'sandbox_mode'],
]);

function optionValue(args, index, name, value) {
  if (value !== undefined) return { value, nextIndex: index };
  if (index + 1 >= args.length || args[index + 1].startsWith('-')) throw new Error(`${name} requires a value`);
  return { value: args[index + 1], nextIndex: index + 1 };
}

function configEntry(value) {
  const equals = value.indexOf('=');
  if (equals <= 0) return null;
  return { key: value.slice(0, equals), value: value.slice(equals + 1) };
}

function addConfig(args, key, value) {
  args.push('-c', `${key}=${JSON.stringify(value)}`);
}

/**
 * Build the native Codex UI argv and the separate app-server argv.
 * The original argv is never mutated; prompts remain opaque positional values.
 */
export function buildLaunchPlan({ originalArgs = [], cwd, proxyUrl, tokenEnv = 'MINDPLAYER_LOCAL_ROUTER_TOKEN', initialModel, preserveNativeModel = false }) {
  if (!Array.isArray(originalArgs)) throw new TypeError('originalArgs must be an array');
  if (!cwd || !proxyUrl || !initialModel) throw new Error('cwd, proxyUrl, and initialModel are required');

  const forwardedArgs = [];
  const backendArgs = [];
  const permissionValues = new Map();
  const configValues = new Map();
  const isResume = originalArgs[0] === 'resume';

  for (let index = 0; index < originalArgs.length; index++) {
    const arg = originalArgs[index];
    const [name, inlineValue] = arg.includes('=') ? arg.split(/=(.*)/s, 2) : [arg, undefined];

    if (arg === '--') {
      forwardedArgs.push(...originalArgs.slice(index));
      break;
    }
    if (REMOTE_OPTIONS.has(name)) throw new Error(`--mindplayer does not accept ${name} in original Codex args`);

    const permissionKey = PERMISSION_OPTIONS.get(name);
    if (permissionKey) {
      const resolved = optionValue(originalArgs, index, name, inlineValue);
      index = resolved.nextIndex;
      if (permissionValues.has(permissionKey)) throw new Error(`duplicate ${name} in original Codex args`);
      permissionValues.set(permissionKey, resolved.value);
      if (!isResume) forwardedArgs.push(arg, ...(inlineValue === undefined ? [resolved.value] : []));
      continue;
    }

    if (name === '-c' || name === '--config') {
      const resolved = optionValue(originalArgs, index, name, inlineValue);
      index = resolved.nextIndex;
      const entry = configEntry(resolved.value);
      if (entry && (entry.key === 'approval_policy' || entry.key === 'sandbox_mode')) {
        if (permissionValues.has(entry.key)) throw new Error(`cannot combine ${entry.key} flag and configuration`);
        configValues.set(entry.key, entry.value);
      }
      forwardedArgs.push(arg, ...(inlineValue === undefined ? [resolved.value] : []));
      backendArgs.push('-c', resolved.value);
      continue;
    }

    if (name === '--profile') {
      const resolved = optionValue(originalArgs, index, name, inlineValue);
      index = resolved.nextIndex;
      forwardedArgs.push(arg, ...(inlineValue === undefined ? [resolved.value] : []));
      backendArgs.push(arg, ...(inlineValue === undefined ? [resolved.value] : []));
      continue;
    }

    forwardedArgs.push(arg);
  }

  for (const [key, value] of permissionValues) {
    if (configValues.has(key)) throw new Error(`cannot combine ${key} flag and configuration`);
    addConfig(backendArgs, key, value);
  }

  return {
    cwd,
    originalArgs: [...originalArgs],
    backendArgs,
    uiArgs: [
      '--remote', proxyUrl,
      '--remote-auth-token-env', tokenEnv,
      ...(preserveNativeModel ? [] : ['--model', initialModel]),
      '--cd', cwd,
      ...forwardedArgs,
    ],
  };
}
