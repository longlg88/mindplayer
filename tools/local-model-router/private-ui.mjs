import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { closeSync, constants, fstatSync, lstatSync, openSync, readSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';

const CONTRACT = 'mindplayer-native-routing-v4';
const SOURCE_REVISION = '092d3acd6bec3e3a14bdc7e7a2810ab628ab759d';
const VERSION = '0.162.1';
const MANIFEST_MAX_BYTES = 16 * 1024;
const BINARY_MAX_BYTES = 512 * 1024 * 1024;
const REASONS = new Map([
  ['ERR_PRIVATE_UI_MISSING', 'MindPlayer 전용 Codex 설치 파일이 없습니다'],
  ['ERR_PRIVATE_UI_MANIFEST', '전용 Codex 설치 정보·버전이 올바르지 않습니다'],
  ['ERR_PRIVATE_UI_PATH', '전용 Codex 경로·파일 권한이 안전하지 않습니다'],
  ['ERR_PRIVATE_UI_HASH', '전용 Codex 파일 무결성 확인에 실패했습니다'],
  ['ERR_PRIVATE_UI_CONTRACT', '전용 Codex UI 계약이 일치하지 않거나 응답하지 않습니다'],
]);

function fail(code) {
  return Object.assign(new Error(REASONS.get(code)), { code });
}

export function privateUiFailureReason(error) {
  return REASONS.get(error?.code);
}

function checkDirectory(path) {
  const stat = lstatSync(path);
  if (!stat.isDirectory() || stat.uid !== process.getuid() || (stat.mode & 0o022)) throw fail('ERR_PRIVATE_UI_PATH');
}

// Read only regular, owned files, without following the final symlink or
// blocking on a FIFO. Check the byte bound again while reading a growing file.
function readCheckedFile(path, { maxBytes, privateOnly = false, executable = false }, consume) {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.uid !== process.getuid() || (stat.mode & 0o7022)
      || (privateOnly && (stat.mode & 0o077)) || (executable && !(stat.mode & 0o100))
      || stat.size < 1 || stat.size > maxBytes) throw fail('ERR_PRIVATE_UI_PATH');
    const buffer = Buffer.alloc(Math.min(maxBytes + 1, 64 * 1024));
    let total = 0;
    let count;
    while ((count = readSync(fd, buffer, 0, buffer.length, null)) !== 0) {
      total += count;
      if (total > maxBytes) throw fail('ERR_PRIVATE_UI_PATH');
      consume(buffer.subarray(0, count));
    }
    const after = fstatSync(fd);
    if (total !== stat.size || after.size !== stat.size || after.mode !== stat.mode
      || after.mtimeMs !== stat.mtimeMs || after.ctimeMs !== stat.ctimeMs) throw fail('ERR_PRIVATE_UI_PATH');
  } finally { closeSync(fd); }
}

/** Fixed schemaVersion: 1 manifest under HOME; no executable/config overrides.
 * Integrated UI, probe and app-server share this build; standalone uses PATH. */
export function resolveUiBinary({ integrated = false } = {}) {
  if (!integrated) return 'codex';
  let stage = 'ERR_PRIVATE_UI_PATH';
  try {
    const home = realpathSync(homedir());
    const data = join(home, '.mindplayer');
    const root = join(data, 'native-codex');
    checkDirectory(data);
    checkDirectory(root);
    const parts = [];
    readCheckedFile(join(root, 'active.json'), { maxBytes: MANIFEST_MAX_BYTES, privateOnly: true }, chunk => parts.push(Buffer.from(chunk)));
    stage = 'ERR_PRIVATE_UI_MANIFEST';
    const manifest = JSON.parse(Buffer.concat(parts).toString('utf8'));
    if (!manifest || Array.isArray(manifest) || Object.keys(manifest).length !== 8
      || manifest.schemaVersion !== 1 || manifest.sourceRevision !== SOURCE_REVISION
      || manifest.contract !== CONTRACT || manifest.version !== VERSION
      || typeof manifest.binarySha256 !== 'string' || !/^[a-f0-9]{64}$/.test(manifest.binarySha256)
      || manifest.binary !== `builds/${manifest.binarySha256}/codex`
      || typeof manifest.codeModeHostSha256 !== 'string' || !/^[a-f0-9]{64}$/.test(manifest.codeModeHostSha256)
      || manifest.codeModeHost !== `builds/${manifest.binarySha256}/codex-code-mode-host`) throw fail(stage);

    stage = 'ERR_PRIVATE_UI_PATH';
    const binary = resolve(root, manifest.binary);
    checkDirectory(join(root, 'builds'));
    checkDirectory(join(root, 'builds', manifest.binarySha256));
    // Upstream locates the code-mode host beside the CLI. Verify both before
    // executing either; a working CLI contract alone cannot attest tool support.
    for (const [name, expectedHash] of [[manifest.binary, manifest.binarySha256], [manifest.codeModeHost, manifest.codeModeHostSha256]]) {
      const path = resolve(root, name);
      const within = relative(root, path);
      if (isAbsolute(within) || within === '..' || within.startsWith(`..${sep}`) || realpathSync(path) !== path) throw fail(stage);
      const hash = createHash('sha256');
      readCheckedFile(path, { maxBytes: BINARY_MAX_BYTES, executable: true }, chunk => hash.update(chunk));
      if (hash.digest('hex') !== expectedHash) throw fail('ERR_PRIVATE_UI_HASH');
    }

    stage = 'ERR_PRIVATE_UI_CONTRACT';
    const result = spawnSync(binary, ['--mindplayer-ui-contract'], {
      env: { HOME: home, PATH: process.env.PATH ?? '/usr/bin:/bin' },
      encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
      // A newly copied native binary can take more than five seconds to start.
      timeout: 15_000, killSignal: 'SIGKILL', maxBuffer: 1024,
    });
    if (result.error || result.status !== 0 || result.stderr !== ''
      || (result.stdout !== CONTRACT && result.stdout !== `${CONTRACT}\n`)) throw fail(stage);
    return binary;
  } catch (error) {
    // Never retain filesystem paths, binary stdout/stderr, or upstream errors.
    if (privateUiFailureReason(error)) throw error;
    throw fail(error?.code === 'ENOENT' ? 'ERR_PRIVATE_UI_MISSING' : stage);
  }
}
