import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, mkdtemp, readFile as readMarker, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const source = await readFile(new URL("../src/omp_model_picker.js", import.meta.url), "utf8");
const { default: picker } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);

function setup() {
  const handlers = new Map();
  const commands = new Map();
  const calls = { options: [], switched: [], notices: [], status: [], aborts: 0 };
  const state = {
    models: [
      { provider: "vercel", id: "same", name: "Gateway model" },
      { provider: "direct", id: "same", name: "Direct model" },
    ],
    choice: undefined,
    result: true,
  };
  const pi = {
    on(name, handler) { handlers.set(name, handler); },
    registerCommand(name, command) { commands.set(name, command); },
    async setModel(model) {
      calls.switched.push(model);
      if (state.result instanceof Error) throw state.result;
      return state.result;
    },
  };
  const ctx = {
    agent: { kind: "main" },
    hasUI: true,
    mode: "tui",
    models: { list: () => state.models },
    ui: {
      async select(_title, options) { calls.options = options; return state.choice; },
      notify(message, type) { calls.notices.push({ message, type }); },
      setStatus(key, text) { calls.status.push({ key, text }); },
    },
    async abort() { calls.aborts++; },
  };
  picker(pi);
  return { handlers, commands, calls, state, ctx };
}

test("cancel blocks input and automatic turns until an explicit successful selection", async () => {
  const { handlers, commands, calls, state, ctx } = setup();
  await handlers.get("session_start")({}, ctx);
  assert.deepEqual(calls.options.map(({ label }) => label), ["direct/same", "vercel/same"]);
  assert.equal(calls.options[1].description, "Gateway model");
  assert.deepEqual(await handlers.get("input")({ text: "hello" }, ctx), { handled: true });
  assert.deepEqual(await handlers.get("input")({ text: "/btw hello" }, ctx), { handled: true });
  assert.equal(await handlers.get("input")({ text: "/login" }, ctx), undefined);
  assert.equal(await handlers.get("input")({ text: "/mindplayer-model" }, ctx), undefined);
  for (const event of ["before_agent_start", "agent_start", "before_provider_request"]) {
    await handlers.get(event)({}, ctx);
  }
  assert.equal(calls.aborts, 3);
  state.choice = "vercel/same";
  await commands.get("mindplayer-model").handler("", ctx);
  assert.equal(calls.switched[0], state.models[0]);
  assert.equal(await handlers.get("input")({ text: "hello" }, ctx), undefined);
  await handlers.get("agent_start")({}, ctx);
  assert.equal(calls.aborts, 3);
});

test("failed or cancelled switching stays blocked and retry re-queries models", async () => {
  const { handlers, commands, state, ctx } = setup();
  state.choice = "direct/same";
  state.result = false;
  await handlers.get("session_start")({}, ctx);
  assert.deepEqual(await handlers.get("input")({ text: "hello" }, ctx), { handled: true });
  state.result = new Error("authentication failed");
  await commands.get("mindplayer-model").handler("", ctx);
  assert.deepEqual(await handlers.get("input")({ text: "hello" }, ctx), { handled: true });
  state.models = [{ provider: "another", id: "new", name: "New model" }];
  state.choice = "another/new";
  state.result = true;
  await commands.get("mindplayer-model").handler("", ctx);
  assert.equal(await handlers.get("input")({ text: "hello" }, ctx), undefined);
  state.choice = undefined;
  await commands.get("mindplayer-model").handler("", ctx);
  assert.deepEqual(await handlers.get("input")({ text: "hello" }, ctx), { handled: true });
});

test("empty registry, subagents, and session transitions", async () => {
  const { handlers, calls, state, ctx } = setup();
  state.models = [];
  await handlers.get("session_start")({}, ctx);
  assert.equal(calls.options.length, 0);
  assert.deepEqual(await handlers.get("input")({ text: "hi" }, ctx), { handled: true });
  await handlers.get("session_switch")({ reason: "resume" }, ctx);
  assert.equal(await handlers.get("input")({ text: "hi" }, ctx), undefined);
  await handlers.get("session_switch")({ reason: "new" }, ctx);
  assert.deepEqual(await handlers.get("input")({ text: "hi" }, ctx), { handled: true });
  ctx.agent.kind = "sub";
  await handlers.get("session_start")({}, ctx);
  await handlers.get("agent_start")({}, ctx);
  assert.equal(calls.aborts, 0);
});

test("readiness marker is written after all guards register", async () => {
  const dir = await mkdtemp(join(tmpdir(), "mindplayer-picker-"));
  const previous = process.env.MINDPLAYER_OMP_PICKER_READY;
  const marker = join(dir, "ready");
  process.env.MINDPLAYER_OMP_PICKER_READY = marker;
  try {
    const { handlers, commands, ctx } = setup();
    assert.ok(handlers.has("input"));
    assert.ok(handlers.has("before_provider_request"));
    assert.ok(commands.has("mindplayer-model"));
    await assert.rejects(readMarker(marker, "utf8"), { code: "ENOENT" });
    await handlers.get("session_start")({}, ctx);
    assert.equal(await readMarker(marker, "utf8"), "ready\n");
    process.env.MINDPLAYER_OMP_PICKER_READY = dir;
    const second = setup();
    await assert.rejects(second.handlers.get("session_start")({}, second.ctx));
  } finally {
    if (previous === undefined) delete process.env.MINDPLAYER_OMP_PICKER_READY;
    else process.env.MINDPLAYER_OMP_PICKER_READY = previous;
    await rm(dir, { recursive: true, force: true });
  }
});

test("selection acknowledgement follows a successful choice and is revoked on retry", async () => {
  const dir = await mkdtemp(join(tmpdir(), "mindplayer-selection-"));
  const previous = process.env.MINDPLAYER_OMP_PICKER_SELECTED;
  const marker = join(dir, "selected");
  process.env.MINDPLAYER_OMP_PICKER_SELECTED = marker;
  const settle = () => new Promise((resolve) => setImmediate(resolve));
  try {
    const { handlers, commands, state, ctx } = setup();
    await handlers.get("session_start")({}, ctx);
    await assert.rejects(readMarker(marker, "utf8"), { code: "ENOENT" });

    state.choice = "direct/same";
    state.result = false;
    await commands.get("mindplayer-model").handler("", ctx);
    await settle();
    await assert.rejects(readMarker(marker, "utf8"), { code: "ENOENT" });

    state.result = true;
    await commands.get("mindplayer-model").handler("", ctx);
    await settle();
    assert.equal(await readMarker(marker, "utf8"), "selected\n");

    state.choice = undefined;
    await commands.get("mindplayer-model").handler("", ctx);
    await settle();
    await assert.rejects(readMarker(marker, "utf8"), { code: "ENOENT" });

    // A cancelled retry must also invalidate an earlier acknowledgement that
    // was scheduled but not yet written.
    state.choice = "vercel/same";
    await commands.get("mindplayer-model").handler("", ctx);
    state.choice = undefined;
    await commands.get("mindplayer-model").handler("", ctx);
    await settle();
    await assert.rejects(readMarker(marker, "utf8"), { code: "ENOENT" });

    state.choice = "vercel/same";
    await commands.get("mindplayer-model").handler("", ctx);
    await settle();
    assert.equal(await readMarker(marker, "utf8"), "selected\n");
    state.choice = undefined;
    await handlers.get("session_switch")({ reason: "new" }, ctx);
    await settle();
    await assert.rejects(readMarker(marker, "utf8"), { code: "ENOENT" });
  } finally {
    if (previous === undefined) delete process.env.MINDPLAYER_OMP_PICKER_SELECTED;
    else process.env.MINDPLAYER_OMP_PICKER_SELECTED = previous;
    await rm(dir, { recursive: true, force: true });
  }
});
