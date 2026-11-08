#!/usr/bin/env bash
# Offline active/archive gates. No installation, provider requests, or real HOME.
# Full: bash tools/verify-active-archive.sh
# Exact executable: MINDPLAYER_TEST_BINARY=/absolute/path/mindplayer \
#   bash tools/verify-active-archive.sh --binary-only
# Run only after source integration/freeze. Four sequential >=130s workloads
# exercise two separate 60s catalog adoptions; never wrap this in timeout(1).
# Inspect retained metrics: bash tools/verify-active-archive.sh --check-report LOG
set -euo pipefail

# These explicit protocol stand-ins let account_isolation.rs exercise every
# provider branch without silently skipping cases or opening real credentials.
# They prove environment selection, not compatibility with an installed CLI.
offline_cli() {
  local program=${0##*/} selected
  case "$program" in
    codex)
      case "$*" in
        *trust_level*123*) printf 'invalid trust_level: expected a string\n' >&2; return 1 ;;
        'login status')
          if [[ ${CODEX_HOME:-"$HOME/.codex"} == "$MINDPLAYER_EVALUATOR_HOME/.codex" ]]; then
            printf 'Logged in using ChatGPT (offline fixture)\n'
          else printf 'Not logged in\n'; return 1; fi ;;
        *) return 1 ;;
      esac ;;
    claude)
      [[ "$*" == 'auth status' ]] || return 1
      selected=${CLAUDE_CONFIG_DIR:-"$HOME/.claude"}
      if [[ "$selected" == "$MINDPLAYER_EVALUATOR_HOME/.claude" ]]; then
        printf '{"loggedIn": true, "email": "fixture@example.test"}\n'
      else printf '{"loggedIn": false}\n'; return 1; fi ;;
    kiro-cli|agent)
      if [[ "$HOME" == "$MINDPLAYER_EVALUATOR_HOME" ]]; then
        printf 'Logged in (offline fixture)\n'
      else printf 'Not logged in\n'; return 1; fi ;;
    curl) return 7 ;;
    *) return 1 ;;
  esac
}
case "${0##*/}" in
  codex|claude|kiro-cli|kiro|agent|omp|pi|vercel|curl|security)
    offline_cli "$@"; exit $? ;;
esac

# Validate captured evidence without building or launching anything. Keep this
# before fixture setup so regression tests need neither credentials nor Cargo.
if [[ ${1:-} == --check-report ]]; then
  [[ $# == 2 ]] || { printf 'usage: %s --check-report LOG\n' "$0" >&2; exit 2; }
  env -u NODE_OPTIONS node --input-type=module --eval '
import { readFileSync } from "node:fs";
const fail = message => { throw new Error(message); };
const check = (condition, message) => { if (!condition) fail(message); };
const finite = value => typeof value === "number" && Number.isFinite(value) && value >= 0;
const reports = readFileSync(process.argv[1], "utf8").split("\n")
  .filter(line => line.includes("ACTIVE_ARCHIVE_EVALUATION="))
  .map(line => JSON.parse(line.slice(line.indexOf("ACTIVE_ARCHIVE_EVALUATION=") + 26)));
check(reports.length > 0, "no executable performance reports");
for (const r of reports) {
  check([7, 10].includes(r.children) && [5000, 50000].includes(r.archive_sources), "unsupported workload");
  check(finite(r.measured_seconds) && r.measured_seconds >= 130, "duration must be at least 130s");
  check(finite(r.p99_ms) && r.p99_ms <= 100, "p99 exceeds 100ms");
  check(finite(r.max_ms) && r.max_ms <= 500, "max exceeds 500ms");
  check(Array.isArray(r.refreshes) && r.refreshes.length >= 2, "two catalog adoptions required");
  const points = r.latency_timeline;
  check(Array.isArray(points) && points.length >= 1000, "typing timeline requires at least 1000 samples");
  for (const [i, e] of r.refreshes.entries()) {
    check(e.cycle === i + 1 && finite(e.created_seconds) && finite(e.observed_seconds)
      && e.observed_seconds >= 60 * (i + 1) && e.created_seconds < e.observed_seconds
      && e.row_rendered === true && e.restored === true && e.native_children_after === r.children,
      "two catalog adoptions must render and restore distinct new sources");
    if (i) check(e.created_seconds > r.refreshes[i - 1].observed_seconds
      && e.observed_seconds - r.refreshes[i - 1].observed_seconds >= 60,
      "catalog adoptions must span separate 60s cycles");
    check(points.some(p => Math.abs(p.at_seconds - e.observed_seconds) <= 2 && p.phase === "archive_search")
      && points.some(p => p.at_seconds <= e.observed_seconds && p.at_seconds >= e.observed_seconds - 2
        && ["split", "zoom"].includes(p.phase)), "typing must overlap each adoption");
  }
  const resources = r.resource_timeline;
  check(Array.isArray(resources) && resources.length >= 100
    && resources[0].at_seconds <= 2 && resources.at(-1).at_seconds >= r.measured_seconds - 2
    && resources.every((p, i) => finite(p.at_seconds) && finite(p.cpu_seconds)
      && finite(p.rss_kib) && p.rss_kib > 0 && p.processes > 0
      && (!i || (p.at_seconds >= resources[i - 1].at_seconds
        && p.at_seconds - resources[i - 1].at_seconds <= 10)))
    && r.refreshes.every(e => resources.some(p => Math.abs(p.at_seconds - e.observed_seconds) <= 2)),
    "CPU/RSS timeline must cover the full run and each adoption");
  check(r.growth?.ticks >= 100 && r.growth.files >= r.children
    && r.growth.initial_bytes >= r.growth.files * 1_000_000
    && r.growth.appended_bytes >= r.growth.files * 65536, "transcripts must grow throughout the run");
  check(points.every((p, i) => finite(p.ms) && finite(p.at_seconds)
    && p.at_seconds <= r.measured_seconds && (!i || p.at_seconds >= points[i - 1].at_seconds))
    && points[0].at_seconds <= 2 && points.at(-1).at_seconds >= r.measured_seconds - 2,
    "invalid typing timeline");
  const latencies = points.map(p => p.ms).sort((a, b) => a - b);
  for (const q of [50, 95, 99]) {
    const observed = latencies[Math.ceil(latencies.length * q / 100) - 1];
    check(finite(r[`p${q}_ms`]) && Math.abs(observed - r[`p${q}_ms`]) < 0.000001,
      `p${q} must be computed from recorded latency samples`);
  }
  check(r.samples === points.length && r.max_ms === latencies.at(-1), "sample count/max mismatch");
  check(["split", "zoom", "archive_search"].every(phase => points.some(p => p.phase === phase)),
    "split, zoom, and Archive typing must all be measured");
  check(r.lost_inputs === 0 && r.duplicate_inputs === 0 && r.deadlocks === 0,
    "lost input, duplicate input, or deadlock detected");
  check(finite(r.cpu_seconds) && r.cpu_seconds >= resources.at(-1).cpu_seconds
    && r.peak_sampled_tree_rss_kib >= Math.max(...resources.map(p => p.rss_kib)), "resource totals mismatch");
  check(r.source_bytes_verified === true && r.native_cleanup === true && r.provider_requests === 0,
    "source purity, native cleanup, and offline evidence required");
}
console.log(`PERFORMANCE_REPORTS_VALIDATED=${reports.length}`);
' "$2"
  exit $?
fi

mode=full
case "${1:-}" in
  '') ;;
  --binary-only) mode=binary ;;
  *) printf 'usage: %s [--binary-only | --check-report LOG]\n' "$0" >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || { printf 'unexpected arguments\n' >&2; exit 2; }

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$repo"
for program in cargo rustc node ps shasum rg; do
  command -v "$program" >/dev/null || { printf 'missing required tool: %s\n' "$program" >&2; exit 2; }
done
cargo_binary=$(command -v cargo)
original_path=$PATH
cargo_home=${CARGO_HOME:-"$HOME/.cargo"}
rustup_home=${RUSTUP_HOME:-"$HOME/.rustup"}
run_dir=$(mktemp -d "${TMPDIR:-/tmp}/mindplayer-active-archive.XXXXXXXX")
run_dir=$(cd "$run_dir" && pwd -P)
mkdir -p "$run_dir/home" "$run_dir/tmp" "$run_dir/bin" "$run_dir/fixtures" "$run_dir/logs"
for program in codex claude kiro-cli kiro agent omp pi vercel curl security; do
  ln -s "$repo/tools/verify-active-archive.sh" "$run_dir/bin/$program"
done
# The script is invoked through bash by humans, and directly through these
# fixture symlinks. The source file must therefore retain executable mode.
[[ -x "$repo/tools/verify-active-archive.sh" ]] || {
  printf 'evaluator must be executable (chmod +x tools/verify-active-archive.sh)\n' >&2; exit 2;
}

binary=${MINDPLAYER_TEST_BINARY:-}
if [[ "$mode" == binary && -z "$binary" ]]; then
  printf '%s\n' '--binary-only requires an absolute MINDPLAYER_TEST_BINARY' >&2; exit 2
fi
if [[ -n "$binary" ]]; then
  [[ "$binary" == /* && -f "$binary" && -x "$binary" ]] || {
    printf 'MINDPLAYER_TEST_BINARY must name an absolute executable file\n' >&2; exit 2;
  }
fi

safe_run() {
  local extra=()
  [[ -z "$binary" ]] || extra+=("MINDPLAYER_TEST_BINARY=$binary")
  [[ -z ${CARGO_TARGET_DIR:-} ]] || extra+=("CARGO_TARGET_DIR=$CARGO_TARGET_DIR")
  env -i \
    HOME="$run_dir/home" PATH="$run_dir/bin:$original_path" \
    CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" \
    CARGO_BUILD_JOBS=2 \
    TMPDIR="$run_dir/tmp" TMP="$run_dir/tmp" TEMP="$run_dir/tmp" \
    XDG_CONFIG_HOME="$run_dir/home/.config" XDG_DATA_HOME="$run_dir/home/.local/share" \
    XDG_CACHE_HOME="$run_dir/home/.cache" XDG_STATE_HOME="$run_dir/home/.local/state" \
    TERM=xterm-256color LANG=en_US.UTF-8 RUSTDOCFLAGS='-D warnings' \
    MINDPLAYER_STATE="$run_dir/state.json" MINDPLAYER_AUDIT="$run_dir/audit.jsonl" \
    MINDPLAYER_CONVO_DIR="$run_dir/conversations" MINDPLAYER_PROMPTS_DIR="$run_dir/prompts" \
    MINDPLAYER_HANDOFF_DIR="$run_dir/handoffs" MINDPLAYER_OMP_DIR="$run_dir/omp" \
    MINDPLAYER_TEST_TMPDIR="$run_dir/fixtures" MINDPLAYER_EVALUATOR_HOME="$run_dir/home" \
    "${extra[@]}" "$@"
}

source_hash() {
  safe_run node --input-type=module --eval '
import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
const hash = createHash("sha256"); let files = 0;
function visit(path) {
  for (const entry of readdirSync(path, { withFileTypes: true }).sort((a,b) => a.name.localeCompare(b.name))) {
    if (["target", ".git", "node_modules"].includes(entry.name)) continue;
    const file = join(path, entry.name);
    if (entry.isDirectory()) visit(file);
    else if (entry.isFile()) add(file);
  }
}
function add(path) {
  const bytes = readFileSync(path);
  hash.update(JSON.stringify([path, bytes.length]) + "\n").update(bytes); files++;
}
for (const path of ["Cargo.toml", "Cargo.lock", "crates", "tools", ".github"]) {
  if (!existsSync(path)) continue;
  if (path.includes("." ) && ![".github"].includes(path)) add(path); else visit(path);
}
console.log(JSON.stringify({ files, sha256: hash.digest("hex") }));'
}

before_hash=$(source_hash)
binary_hash=
verify_sources() {
  local status=$? after_hash
  trap - EXIT
  after_hash=$(source_hash) || status=1
  if [[ "$before_hash" != "$after_hash" ]]; then
    printf 'FAIL: source bytes changed during evaluator\nbefore=%s\nafter=%s\n' "$before_hash" "$after_hash" >&2
    status=1
  else printf 'SOURCE_BYTES_UNCHANGED=%s\n' "$after_hash"; fi
  if ! safe_run node --input-type=module --eval '
import { readdirSync } from "node:fs";
if (readdirSync(process.argv[1]).length) throw new Error("task-owned fixture remnants remain; preserve them for diagnosis");
' "$run_dir/fixtures"; then
    status=1
  else printf 'FIXTURE_DIRECTORIES_REMOVED=true\n'; fi
  if [[ -n "$binary_hash" ]]; then
    local after_binary_hash
    after_binary_hash=$(shasum -a 256 "$binary") || status=1
    if [[ "$binary_hash" != "$after_binary_hash" ]]; then
      printf 'FAIL: selected executable changed during evaluator\nbefore=%s\nafter=%s\n' "$binary_hash" "$after_binary_hash" >&2
      status=1
    else printf 'BINARY_BYTES_UNCHANGED=%s\n' "$after_binary_hash"; fi
  fi
  printf 'EVALUATOR_LOG_DIR=%s\n' "$run_dir/logs"
  if [[ "$status" == 0 ]]; then printf 'PASS: %s active/archive evaluator\n' "$mode"; fi
  exit "$status"
}
trap verify_sources EXIT

phase() {
  local label=$1 log
  shift
  log="$run_dir/logs/$label.log"
  printf '\n[%s] ' "$label"
  printf '%q ' "$@"
  printf '\n'
  if safe_run "$@" >"$log" 2>&1; then
    tail -n 12 "$log" | sed -E 's/(ACTIVE_ARCHIVE_EVALUATION=).*/\1[raw metrics retained in phase log]/'
  else
    local status=$?
    tail -n 100 "$log" | sed -E 's/(ACTIVE_ARCHIVE_EVALUATION=).*/\1[raw metrics retained in phase log]/' >&2
    printf 'FAIL: %s (exit %s); full log: %s\n' "$label" "$status" "$log" >&2
    return "$status"
  fi
}

printf 'MODE=%s\nOFFLINE_HOME=%s\n' "$mode" "$run_dir/home"
printf 'Account-isolation tests use explicit offline CLI stand-ins; no live-auth compatibility claim.\n'
if [[ "$mode" == full ]]; then
  phase fmt "$cargo_binary" fmt --all --check
  phase clippy "$cargo_binary" clippy -j2 --workspace --all-targets --all-features -- -D warnings
  phase offline-account-isolation "$cargo_binary" test -p mindplayer-core --test account_isolation -- --nocapture --test-threads=1
  for evidence in 'verified isolation for 4 provider(s)' 'checked availability for 4 provider(s)'; do
    rg -Fq "$evidence" "$run_dir/logs/offline-account-isolation.log" || {
      printf 'offline account-isolation branches were not all exercised: %s\n' "$evidence" >&2; exit 1;
    }
  done
  phase workspace-tests "$cargo_binary" test --workspace --all-features -- --test-threads=1
  phase docs "$cargo_binary" doc --workspace --all-features --no-deps
  phase release-catalog "$cargo_binary" test --release -p mindplayer-core --test catalog -- --include-ignored --nocapture --test-threads=1
  phase release-sort "$cargo_binary" test --release -p mindplayer-tui --bin mindplayer working_set_root_sort_budget -- --ignored --nocapture --test-threads=1
  rg -q 'test result: ok\. 1 passed' "$run_dir/logs/release-sort.log" || {
    printf 'release sort gate did not run exactly one test\n' >&2; exit 1;
  }
  phase release-render "$cargo_binary" test --release -p mindplayer-tui --bin mindplayer list_actual_render -- --ignored --nocapture --test-threads=1
  rg -q 'test result: ok\. 2 passed; 0 failed; 0 ignored;' "$run_dir/logs/release-render.log" || {
    printf 'release render gate did not run exactly two tests\n' >&2; exit 1;
  }
  phase release-build "$cargo_binary" build --release -p mindplayer-tui
  if [[ -z "$binary" ]]; then
    metadata=$(safe_run "$cargo_binary" metadata --no-deps --format-version 1)
    target_dir=$(printf '%s' "$metadata" | safe_run node -e 'let s="";process.stdin.on("data",x=>s+=x).on("end",()=>process.stdout.write(JSON.parse(s).target_directory))')
    binary="$target_dir/release/mindplayer"
  fi
fi
[[ -x "$binary" ]] || { printf 'selected binary is not executable\n' >&2; exit 1; }
# Compile first, then run the harness directly. Long PTY evaluations must not
# hold Cargo's artifact lock and stall another agent's build. This also records
# the executable checksum only after Cargo has finished building its binaries.
fixture_log="$run_dir/logs/executable-fixture-build.jsonl"
printf '\n[executable-fixture-build] compile release smoke harness\n'
if safe_run "$cargo_binary" test --release -j2 -p mindplayer-tui --test smoke --no-run --message-format=json >"$fixture_log" 2>"$run_dir/logs/executable-fixture-build.stderr"; then
  tail -n 8 "$run_dir/logs/executable-fixture-build.stderr"
else
  tail -n 30 "$fixture_log" >&2
  tail -n 30 "$run_dir/logs/executable-fixture-build.stderr" >&2
  exit 1
fi
test_binary=$(safe_run node --input-type=module --eval '
import { readFileSync, realpathSync } from "node:fs";
const artifacts = readFileSync(process.argv[1], "utf8").trim().split("\n").map(line => JSON.parse(line))
  .filter(value => value.reason === "compiler-artifact" && value.target.name === "smoke" && value.profile.test && value.executable);
if (artifacts.length !== 1) throw new Error("expected exactly one release smoke executable");
process.stdout.write(realpathSync(artifacts[0].executable));' "$fixture_log")
printf 'TEST_HARNESS_BINARY=%s\n' "$test_binary"
printf 'EXACT_BINARY=%s\n' "$binary"
safe_run "$binary" --version
binary_hash=$(shasum -a 256 "$binary")
printf '%s\n' "$binary_hash"
phase executable-active-archive "$test_binary" active_archive_ --include-ignored --nocapture --test-threads=1
rg -q 'test result: ok\. 6 passed' "$run_dir/logs/executable-active-archive.log" || {
  printf 'executable active/archive gate did not run all six behavioral tests\n' >&2; exit 1;
}
[[ $(rg -c '^ACTIVE_ARCHIVE_EVALUATION=|ACTIVE_ARCHIVE_EVALUATION=' "$run_dir/logs/executable-active-archive.log") == 4 ]] || {
  printf 'expected four executable latency/resource reports\n' >&2; exit 1;
}
phase executable-evidence bash "$repo/tools/verify-active-archive.sh" --check-report "$run_dir/logs/executable-active-archive.log"
safe_run node --input-type=module --eval '
import { readFileSync } from "node:fs";
const reports = readFileSync(process.argv[1], "utf8").split("\n")
  .filter(line => line.includes("ACTIVE_ARCHIVE_EVALUATION="))
  .map(line => JSON.parse(line.slice(line.indexOf("ACTIVE_ARCHIVE_EVALUATION=") + 26)));
const workloads = reports.map(r => `${r.children}/${r.archive_sources}`).sort();
if (JSON.stringify(workloads) !== JSON.stringify(["10/5000", "10/50000", "7/5000", "7/50000"]))
  throw new Error("all four distinct 7/10 child x 5k/50k catalog workloads are required");
for (const r of reports) console.log(JSON.stringify({children: r.children, archive_sources: r.archive_sources,
  seconds: r.measured_seconds, samples: r.samples, p50_ms: r.p50_ms, p95_ms: r.p95_ms,
  p99_ms: r.p99_ms, max_ms: r.max_ms, cpu_seconds: r.cpu_seconds,
  peak_rss_kib: r.peak_sampled_tree_rss_kib, refreshes: r.refreshes}));
' "$run_dir/logs/executable-active-archive.log"
