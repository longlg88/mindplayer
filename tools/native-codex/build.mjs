#!/usr/bin/env node
// Reproduce the private UI from a pinned upstream checkout. Never replaces the
// user's normal Codex and never activates an untested build automatically.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const revision = '092d3acd6bec3e3a14bdc7e7a2810ab628ab759d';
const contract = 'mindplayer-native-routing-v4';
const { values } = parseArgs({ options: {
  source: { type: 'string' }, output: { type: 'string' }, cargo: { type: 'string', default: 'cargo' },
  'code-mode-host': { type: 'string' },
} });
if (!values.source || !values.output) throw new Error('Required: --source <pinned checkout> --output <private staging directory> [--cargo <cargo>]');
const source = realpathSync(values.source);
const output = resolve(values.output);
const patch = readFileSync(new URL('./queue.patch', import.meta.url));
const patchSha256 = createHash('sha256').update(patch).digest('hex');
const buildEnv = { ...process.env,
  PATH: values.cargo.includes('/') ? `${dirname(resolve(values.cargo))}:${process.env.PATH}` : process.env.PATH,
  CARGO_TARGET_DIR: join(source, 'codex-rs', 'target'),
  CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? '4' };
for (const key of ['RUSTC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS',
  'AI_GATEWAY_API_KEY', 'TYPESAFE_API_KEY', 'VERCEL_OIDC_TOKEN']) delete buildEnv[key];
const run = (binary, args, cwd = source, capture = false) => {
  const result = spawnSync(binary, args, { cwd, encoding: 'utf8', stdio: capture ? ['ignore', 'pipe', 'pipe'] : 'inherit',
    env: buildEnv });
  if (result.error || result.status !== 0) throw new Error(`${binary} ${args[0]} failed (${result.status ?? 'spawn'}); no activation`);
  return result.stdout?.trim();
};
if (run('git', ['rev-parse', 'HEAD'], source, true) !== revision) throw new Error('Wrong upstream revision');
if (run('git', ['ls-files', '--others', '--exclude-standard'], source, true)) throw new Error('Untracked upstream files; use a dedicated checkout');
// The release tag bumps Cargo.toml but leaves 161 local workspace lock entries
// at 0.0.0. Normalize only those exact version lines, not dependency resolution.
const lockPath = join(source, 'codex-rs', 'Cargo.lock');
const upstreamLock = run('git', ['show', `${revision}:codex-rs/Cargo.lock`], source, true) + '\n';
const normalizedLock = upstreamLock.replaceAll(/^version = "0\.0\.0"$/gm, 'version = "0.162.1"');
const currentLock = readFileSync(lockPath, 'utf8');
if (currentLock !== upstreamLock && currentLock !== normalizedLock) throw new Error('Unreviewed upstream dependency lock changes');
const dirty = run('git', ['diff', '--binary', 'HEAD', '--', '.', ':(exclude)codex-rs/Cargo.lock'], source, true);
if (!dirty) {
  const changes = run('git', ['status', '--porcelain'], source, true);
  if (changes && changes !== 'M codex-rs/Cargo.lock') throw new Error('Untracked upstream files; use a dedicated checkout');
  run('git', ['apply', '--check', fileURLToPath(new URL('./queue.patch', import.meta.url))]);
  // Keep reviewed new source files in the reproducible HEAD diff without
  // staging their contents; the strict unrelated-untracked-file guard remains.
  run('git', ['apply', '--intent-to-add', fileURLToPath(new URL('./queue.patch', import.meta.url))]);
} else if (dirty !== patch.toString('utf8').trim()) {
  throw new Error('Upstream checkout has changes other than the reviewed queue patch');
}
if (currentLock !== normalizedLock) writeFileSync(lockPath, normalizedLock);
const rust = join(source, 'codex-rs');
const rustc = run('rustc', ['--version'], rust, true);
if (!rustc.startsWith('rustc 1.95.0 ')) throw new Error('Pinned Rust 1.95.0 required; use --cargo /absolute/toolchain/bin/cargo');
run(values.cargo, ['test', '--locked', '--profile', 'dev-small', '-p', 'codex-tui', '--lib', 'mindplayer_'], rust);
run(values.cargo, ['test', '--locked', '--profile', 'dev-small', '-p', 'codex-app-server-protocol', '--lib', 'mindplayer_'], rust);
run(values.cargo, ['test', '--locked', '--profile', 'dev-small', '-p', 'codex-app-server', '--test', 'all', 'mindplayer_require_new_turn'], rust);
run(values.cargo, ['build', '--locked', '--profile', 'dev-small', '-p', 'codex-cli', '--bin', 'codex'], rust);
// Current catalog models require Code Mode. The upstream standalone layout
// resolves this separate host beside codex; a CLI-only build cannot run tools.
let suppliedHost;
if (values['code-mode-host']) {
  suppliedHost = realpathSync(values['code-mode-host']);
  // Explicit existing official package input, not a runtime PATH fallback.
  // Its metadata establishes the declared release, not source-build attestation.
  const packageMetadata = JSON.parse(readFileSync(join(dirname(dirname(suppliedHost)), 'codex-package.json'), 'utf8'));
  if (packageMetadata.version !== '0.162.1' || packageMetadata.layoutVersion !== 1
    || packageMetadata.entrypoint !== 'bin/codex' || !suppliedHost.endsWith('/bin/codex-code-mode-host')) {
    throw new Error('Supplied host must be from an explicit official 0.162.1 package layout');
  }
} else {
  run(values.cargo, ['build', '--locked', '--profile', 'dev-small', '-p', 'codex-code-mode-host', '--bin', 'codex-code-mode-host'], rust);
  run(values.cargo, ['test', '--locked', '--profile', 'dev-small', '-p', 'codex-code-mode-host', '--test', 'stdio'], rust);
}
if (run('git', ['rev-parse', 'HEAD'], source, true) !== revision ||
    run('git', ['diff', '--binary', 'HEAD', '--', '.', ':(exclude)codex-rs/Cargo.lock'], source, true) !== patch.toString('utf8').trim() ||
    run('git', ['ls-files', '--others', '--exclude-standard'], source, true) || readFileSync(lockPath, 'utf8') !== normalizedLock) {
  throw new Error('Source changed during build; no candidate or activation');
}
const built = join(rust, 'target', 'dev-small', 'codex');
if (run(built, ['--mindplayer-ui-contract'], rust, true) !== contract) throw new Error('Private native UI contract missing');
if (run(built, ['--version'], rust, true) !== 'codex-cli 0.162.1') throw new Error('Private native UI version mismatch');
const binarySha256 = createHash('sha256').update(readFileSync(built)).digest('hex');
const builtHost = suppliedHost ?? join(rust, 'target', 'dev-small', 'codex-code-mode-host');
const codeModeHostSha256 = createHash('sha256').update(readFileSync(builtHost)).digest('hex');
const binary = `builds/${binarySha256}/codex`;
const codeModeHost = `builds/${binarySha256}/codex-code-mode-host`;
const destination = join(output, binary);
mkdirSync(dirname(destination), { recursive: true, mode: 0o700 });
if (existsSync(destination)) {
  if (createHash('sha256').update(readFileSync(destination)).digest('hex') !== binarySha256) throw new Error('Existing content-addressed build differs');
} else { copyFileSync(built, destination); chmodSync(destination, 0o700); }
const destinationHost = join(output, codeModeHost);
if (existsSync(destinationHost)) {
  if (createHash('sha256').update(readFileSync(destinationHost)).digest('hex') !== codeModeHostSha256) throw new Error('Existing content-addressed host differs');
} else { copyFileSync(builtHost, destinationHost); chmodSync(destinationHost, 0o700); }
const manifest = { schemaVersion: 1, sourceRevision: revision, contract, version: '0.162.1', patchSha256, binarySha256, binary, codeModeHostSha256, codeModeHost,
  profile: 'dev-small', sourceDirectory: source, rustc,
  codeModeHostOrigin: suppliedHost ? { kind: 'explicit-official-prebuilt', declaredPackageVersion: '0.162.1', path: suppliedHost,
    sourceBuildAttested: false, compatibilityRequiresActualNativeToolTest: true } : { kind: 'same-source-build', sourceRevision: revision },
  nativeRegressionTests: [
    'cargo test --locked --profile dev-small -p codex-tui --lib mindplayer_',
    'cargo test --locked --profile dev-small -p codex-app-server-protocol --lib mindplayer_',
    'cargo test --locked --profile dev-small -p codex-app-server --test all mindplayer_require_new_turn',
    ...(suppliedHost ? [] : ['cargo test --locked --profile dev-small -p codex-code-mode-host --test stdio']),
  ] };
const candidate = join(output, `candidate-${binarySha256}.json`);
writeFileSync(candidate, JSON.stringify(manifest, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify({ candidate, binary: destination, binarySha256, codeModeHost: destinationHost, codeModeHostSha256, activated: false }));
