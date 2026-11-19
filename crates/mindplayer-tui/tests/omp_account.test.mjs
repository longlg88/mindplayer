import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, readdir, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const source = await readFile(new URL("../src/omp_account.js", import.meta.url), "utf8");
const { default: accountExtension } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);

function factory(file, launchId, rejectedEvent) {
  const oldFile = process.env.MINDPLAYER_OMP_ACCOUNT_FILE;
  const oldLaunch = process.env.MINDPLAYER_OMP_ACCOUNT_LAUNCH_ID;
  process.env.MINDPLAYER_OMP_ACCOUNT_FILE = file;
  process.env.MINDPLAYER_OMP_ACCOUNT_LAUNCH_ID = launchId;
  const handlers = new Map();
  try {
    accountExtension({ on(name, handler) {
      if (name === rejectedEvent) throw new Error("unsupported host event");
      handlers.set(name, handler);
    } });
  } finally {
    if (oldFile === undefined) delete process.env.MINDPLAYER_OMP_ACCOUNT_FILE;
    else process.env.MINDPLAYER_OMP_ACCOUNT_FILE = oldFile;
    if (oldLaunch === undefined) delete process.env.MINDPLAYER_OMP_ACCOUNT_LAUNCH_ID;
    else process.env.MINDPLAYER_OMP_ACCOUNT_LAUNCH_ID = oldLaunch;
  }
  return handlers;
}

async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), "mindplayer-account-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const file = join(dir, "account.json");
  const state = {
    sessionId: "native-session-a",
    model: { provider: "provider-a", id: "model-a" },
    rows: [
      { credentialId: 11, email: "first@example.test", active: true },
      { credentialId: 27, email: "actual@example.test", accountId: "account-27", orgName: "Actual workspace", active: false },
    ],
    sticky: undefined,
    queries: [],
    stickyQueries: [],
    timers: new Map(),
  };
  const ctx = {
    agent: { kind: "main", depth: 0 },
    sessionManager: { getSessionId: () => state.sessionId },
    get model() { return state.model; },
    modelRegistry: {
      authStorage: {
        oauth: {
          accounts(provider, sessionId) {
            state.queries.push([provider, sessionId]);
            if (state.rows instanceof Error) throw state.rows;
            return state.rows;
          },
          identity() { throw new Error("identity must not be used"); },
        },
        sessions: {
          sticky(provider, sessionId) {
            state.stickyQueries.push([provider, sessionId]);
            return state.sticky;
          },
        },
        get credentials() { throw new Error("raw credential storage must not be read"); },
        get keys() { throw new Error("credential resolution must not be invoked"); },
      },
    },
    setInterval(callback, ms) {
      assert.equal(ms, 500);
      const handle = Symbol("managed-timer");
      state.timers.set(handle, callback);
      return handle;
    },
    clearTimer(handle) { state.timers.delete(handle); },
  };
  const handlers = factory(file, "launch-a");
  const emit = (name, message, context = ctx) => handlers.get(name)(message ? { message } : {}, context);
  const assistant = (credentialId = 27, overrides = {}) => ({
    role: "assistant", provider: "provider-a", model: "model-a", credentialId, ...overrides,
  });
  const read = async () => JSON.parse(await readFile(file, "utf8"));
  const tick = () => { for (const callback of state.timers.values()) callback(); };
  return { dir, file, state, ctx, handlers, emit, assistant, read, tick };
}

test("actual message ID selects a non-first/non-active account and final rotation wins", async (t) => {
  const f = await fixture(t);
  f.emit("session_start");
  assert.equal((await f.read()).state, "unresolved");
  assert.equal(f.state.queries.length, 0);
  f.emit("message_update", f.assistant());
  assert.deepEqual(await f.read(), {
    schema: "mindplayer.omp-account.v1", launchId: "launch-a", sessionId: "native-session-a",
    state: "observed", provider: "provider-a", model: "model-a", credentialId: 27,
    kind: "oauth", email: "actual@example.test", accountId: "account-27", orgName: "Actual workspace",
  });
  assert.deepEqual(f.state.queries, [["provider-a", "native-session-a"]]);
  const before = await stat(f.file);
  for (let i = 0; i < 100; i++) f.emit("message_update", f.assistant());
  f.tick();
  assert.equal(f.state.queries.length, 1);
  assert.equal((await stat(f.file)).ino, before.ino);
  assert.equal((await stat(f.file)).mtimeMs, before.mtimeMs);
  f.emit("message_end", f.assistant(11));
  assert.equal((await f.read()).email, "first@example.test");
  assert.equal((await f.read()).credentialId, 11);
  assert.equal(f.state.queries.length, 2);
  assert.equal((await stat(f.file)).mode & 0o777, 0o600);
  assert.deepEqual(await readdir(f.dir), ["account.json"]);
});

test("session, turn, retry and idle model changes revoke observations without selecting auth", async (t) => {
  const f = await fixture(t);
  for (const name of ["session_start", "session_switch", "session_branch", "session_clear", "session_tree", "before_agent_start", "turn_start", "retry_fallback_applied", "auto_retry_start"]) {
    f.emit("message_end", f.assistant());
    const queries = f.state.queries.length;
    f.emit(name);
    assert.equal((await f.read()).state, "unresolved", name);
    assert.equal(f.state.queries.length, queries, name);
  }
  f.emit("message_end", f.assistant());
  f.state.model = { provider: "provider-b", id: "model-b" };
  f.tick();
  assert.equal((await f.read()).state, "unresolved");
  const queries = f.state.queries.length;
  f.emit("message_end", f.assistant());
  assert.equal((await f.read()).state, "unresolved");
  assert.equal(f.state.queries.length, queries);
  f.emit("message_end", f.assistant(27, { provider: "provider-b", model: "model-b" }));
  assert.equal((await f.read()).provider, "provider-b");
  assert.deepEqual(f.state.queries.at(-1), ["provider-b", "native-session-a"]);
  f.state.sessionId = "native-session-b";
  f.tick();
  assert.equal((await f.read()).sessionId, "native-session-b");
  assert.equal((await f.read()).state, "unresolved");
  assert.equal(f.state.timers.size, 1);
  f.emit("session_shutdown");
  assert.equal(f.state.timers.size, 0);
});

test("child factories, including depth-zero non-main agents, never touch the main sideband", async (t) => {
  const f = await fixture(t);
  f.emit("message_end", f.assistant());
  const before = await readFile(f.file, "utf8");
  const childHandlers = factory(f.file, "launch-a");
  const child = { ...f.ctx, agent: { kind: "subagent", depth: 0 } };
  for (const handler of childHandlers.values()) handler({ message: f.assistant(11) }, child);
  for (const handler of f.handlers.values()) handler({ message: f.assistant(11) }, child);
  f.tick();
  assert.equal(await readFile(f.file, "utf8"), before);
  assert.equal(f.state.queries.length, 1);
  assert.equal(f.state.timers.size, 1);
});

test("absent or invalid credential IDs never infer the sole roster account", async (t) => {
  const f = await fixture(t);
  f.state.rows = [f.state.rows[1]];
  for (const credentialId of [undefined, null, "27", 0, -1, NaN, 2.5, Number.MAX_SAFE_INTEGER + 1]) {
    f.emit("message_end", f.assistant(27, { credentialId }));
    assert.equal((await f.read()).state, "unresolved");
    assert.equal((await f.read()).email, undefined);
  }
  assert.equal(f.state.queries.length, 0);
  f.emit("message_end", f.assistant(27, { role: "user" }));
  assert.equal(f.state.queries.length, 0);
});

test("API-key type requires an exact pin and OAuth identity requires a numeric roster match", async (t) => {
  const f = await fixture(t);
  f.state.sticky = { credentialId: 900, type: "api_key", index: 0 };
  f.emit("message_end", f.assistant(900));
  assert.equal((await f.read()).kind, "api_key");
  assert.deepEqual(f.state.stickyQueries, [["provider-a", "native-session-a"]]);
  f.emit("message_end", f.assistant(901));
  assert.equal((await f.read()).state, "unresolved");
  f.state.sticky = { credentialId: 902, type: "oauth", index: 0 };
  f.emit("message_end", f.assistant(902));
  assert.equal((await f.read()).kind, "credential");
  f.state.rows = [{ credentialId: "903", email: "wrong@example.test" }];
  f.state.sticky = undefined;
  f.emit("message_end", f.assistant(903));
  assert.equal((await f.read()).kind, "credential");
  assert.equal((await f.read()).email, undefined);
});

test("malformed or missing capabilities fail closed and do not repeat roster queries per chunk", async (t) => {
  const f = await fixture(t);
  f.state.rows = new Error("SECRET must never be serialized");
  for (let i = 0; i < 10; i++) f.emit("message_update", f.assistant());
  assert.equal(f.state.queries.length, 1);
  assert.equal((await f.read()).state, "unsupported");
  assert.equal((await readFile(f.file, "utf8")).includes("SECRET"), false);
  f.state.rows = {};
  f.emit("message_end", f.assistant(28));
  assert.equal((await f.read()).state, "unsupported");
  delete f.ctx.modelRegistry.authStorage.oauth.accounts;
  f.emit("message_end", f.assistant(29));
  assert.equal((await f.read()).state, "unsupported");
  f.ctx.sessionManager.getSessionId = () => undefined;
  f.tick();
  await assert.rejects(readFile(f.file), { code: "ENOENT" });
});

test("without managed model observation capability no stale account is published", async (t) => {
  const f = await fixture(t);
  delete f.ctx.setInterval;
  f.emit("session_start");
  f.emit("message_end", f.assistant());
  assert.equal((await f.read()).state, "unsupported");
  assert.equal(f.state.queries.length, 0);
});

test("serialization is token-free, bounded by UTF-8 bytes and omits terminal controls", async (t) => {
  const f = await fixture(t);
  f.state.rows = [{
    credentialId: 27,
    email: "safe@example.test",
    accountId: "x".repeat(513),
    orgId: "界".repeat(171),
    orgName: "bad\u001b[31mworkspace",
    get accessToken() { throw new Error("secret accessed"); },
    get refresh() { throw new Error("secret accessed"); },
  }];
  f.emit("message_end", f.assistant());
  const payload = await f.read();
  assert.equal(payload.state, "observed");
  assert.equal(payload.email, "safe@example.test");
  assert.equal(payload.accountId, undefined);
  assert.equal(payload.orgId, undefined);
  assert.equal(payload.orgName, undefined);
  assert.deepEqual(Object.keys(payload).sort(), ["schema", "launchId", "sessionId", "state", "provider", "model", "credentialId", "kind", "email"].sort());
  assert.ok((await readFile(f.file)).byteLength <= 8192);
  f.state.model.id = "界".repeat(86);
  f.emit("message_end", f.assistant(27, { model: f.state.model.id }));
  assert.equal((await f.read()).state, "unresolved");
});

test("independent pane factories preserve launch and native session provenance", async (t) => {
  const first = await fixture(t);
  const second = await fixture(t);
  second.state.sessionId = "other-native-session";
  const secondHandlers = factory(second.file, "launch-b");
  first.emit("message_end", first.assistant(11));
  secondHandlers.get("message_end")({ message: second.assistant() }, second.ctx);
  assert.equal((await first.read()).launchId, "launch-a");
  assert.equal((await first.read()).credentialId, 11);
  assert.equal((await second.read()).launchId, "launch-b");
  assert.equal((await second.read()).sessionId, "other-native-session");
  assert.equal((await second.read()).credentialId, 27);
});

test("unsupported event registration and inaccessible sideband never interrupt native handlers", async (t) => {
  const f = await fixture(t);
  const unsupported = factory(f.file, "launch-a", "turn_start");
  assert.equal(unsupported.get("message_end")({ message: f.assistant() }, f.ctx), undefined);
  assert.equal((await f.read()).state, "unsupported");
  assert.equal(f.state.queries.length, 0);
  const missing = factory(join(f.dir, "nonexistent", "account.json"), "launch-b");
  assert.doesNotThrow(() => missing.get("message_end")({ message: f.assistant() }, f.ctx));
  assert.deepEqual(await readdir(f.dir), ["account.json"]);
});

test("same-model account pin changes invalidate until the next actual message", async (t) => {
  const f = await fixture(t);
  f.state.sticky = { credentialId: 27, type: "oauth", explicit: true };
  f.emit("message_end", f.assistant(27));
  const before = await stat(f.file);
  f.tick();
  f.tick();
  assert.equal((await f.read()).credentialId, 27);
  assert.equal((await stat(f.file)).ino, before.ino);
  assert.equal(f.state.queries.length, 1);

  // The native account picker pins another account without changing the model.
  f.state.sticky = { credentialId: 11, type: "oauth", explicit: true };
  f.tick();
  assert.equal((await f.read()).state, "unresolved");
  assert.equal((await f.read()).credentialId, undefined);
  assert.equal(f.state.queries.length, 1);
  const unknown = await stat(f.file);
  f.tick();
  assert.equal((await stat(f.file)).ino, unknown.ino);
  f.emit("message_end", f.assistant(11));
  assert.equal((await f.read()).state, "observed");
  assert.equal((await f.read()).email, "first@example.test");
  assert.equal(f.state.queries.length, 2);

  // A previously proven pin disappearing also revokes that attribution.
  f.state.sticky = undefined;
  f.tick();
  assert.equal((await f.read()).state, "unresolved");
  assert.equal(f.state.queries.length, 2);
});

test("sticky disappearance only invalidates when that actual credential was pinned", async (t) => {
  const f = await fixture(t);
  f.emit("message_end", f.assistant(27));
  f.tick();
  assert.equal((await f.read()).state, "observed");
  f.state.sticky = { credentialId: 27, type: "oauth" };
  f.tick();
  assert.equal((await f.read()).state, "observed");
  f.state.sticky = undefined;
  f.tick();
  assert.equal((await f.read()).state, "unresolved");
  assert.equal(f.state.queries.length, 1);
});

test("late old-account output cannot restore attribution after an explicit pin switch", async (t) => {
  const f = await fixture(t);
  f.state.sticky = { credentialId: 27, type: "oauth" };
  f.emit("message_update", f.assistant(27));
  f.state.sticky = { credentialId: 11, type: "oauth" };
  f.emit("message_end", f.assistant(27));
  assert.equal((await f.read()).state, "unresolved");
  f.tick();
  f.emit("message_update", f.assistant(27));
  assert.equal((await f.read()).state, "unresolved");
  f.emit("message_end", f.assistant(11));
  assert.equal((await f.read()).email, "first@example.test");
});
