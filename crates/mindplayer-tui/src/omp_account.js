import { randomUUID } from "node:crypto";
import { renameSync, unlinkSync, writeFileSync } from "node:fs";

const SCHEMA = "mindplayer.omp-account.v1";
const MAX_BYTES = 8192;

function text(value, limit) {
  if (typeof value !== "string" || !value.trim() || Buffer.byteLength(value, "utf8") > limit) return undefined;
  if (/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(value)) return undefined;
  return value;
}

// OMP v18.22.1: message_update/message_end carry the stream driver's actual
// attempt credentialId. oauth.accounts is token-free; sticky only proves type
// when its durable ID exactly matches that message. Never resolve credentials.
export default function mindplayerAccount(pi) {
  const file = process.env.MINDPLAYER_OMP_ACCOUNT_FILE;
  const launchId = text(process.env.MINDPLAYER_OMP_ACCOUNT_LAUNCH_ID, 256);
  if (!file || !launchId || typeof pi?.on !== "function") return;

  let lastPayload;
  let evidence;
  let scope;
  let timer;
  let timerContext;
  let mainContext;
  let supportedEvents = true;

  function remove() {
    try { unlinkSync(file); } catch {}
    lastPayload = undefined;
  }

  function publish(target, state, fields = {}) {
    if (!target?.sessionId) {
      remove();
      return;
    }
    const { sessionId, sourceFile } = target;
    const payload = JSON.stringify({ schema: SCHEMA, launchId, sessionId, sourceFile, state, ...fields });
    if (payload === lastPayload) return;
    if (Buffer.byteLength(payload, "utf8") > MAX_BYTES) {
      remove();
      return;
    }
    const temporary = `${file}.${randomUUID()}.tmp`;
    try {
      writeFileSync(temporary, payload, { encoding: "utf8", mode: 0o600, flag: "wx" });
      renameSync(temporary, file);
      lastPayload = payload;
    } catch {
      // This display-only bridge must never interrupt native OMP or dump errors
      // that could contain credentials. Revoke stale identity on failed writes.
      remove();
    } finally {
      try { unlinkSync(temporary); } catch {}
    }
  }

  function current(ctx) {
    const sourceFile = text(ctx.sessionManager?.getSessionFile?.(), 2048);
    return {
      sessionId: text(ctx.sessionManager?.getSessionId?.(), 256),
      sourceFile: sourceFile?.startsWith("/") ? sourceFile : undefined,
      provider: text(ctx.model?.provider, 256),
      model: text(ctx.model?.id, 256),
    };
  }

  function invalidate(ctx, reason) {
    evidence = undefined;
    scope = current(ctx);
    publish(scope, "unresolved", { reason });
  }

  function checkScope(ctx) {
    const next = current(ctx);
    if (!scope || ["sessionId", "sourceFile", "provider", "model"].some((key) => next[key] !== scope[key])) {
      evidence = undefined;
      scope = next;
      publish(scope, "unresolved", { reason: "selection_changed" });
    }
    return scope;
  }

  function sticky(ctx, provider, sessionId) {
    const sessions = ctx.modelRegistry?.authStorage?.sessions;
    return typeof sessions?.sticky === "function" ? sessions.sticky(provider, sessionId) : undefined;
  }

  function checkSelection(ctx) {
    checkScope(ctx);
    if (!evidence) return;
    const pin = sticky(ctx, evidence.provider, evidence.sessionId);
    const pinId = pin?.credentialId;
    const knownPin = Number.isSafeInteger(pinId) && pinId > 0;
    if ((knownPin && pinId !== evidence.credentialId) || (evidence.pinMatched && pinId !== evidence.credentialId)) {
      invalidate(ctx, "account_selection_changed");
    } else if (pinId === evidence.credentialId) {
      evidence.pinMatched = true;
    }
  }

  function guarded(handler) {
    return (event, ctx) => {
      // Each child gets its own extension factory. No factory-time, child-start,
      // child-shutdown, or depth-only checks may write the main pane's file.
      if (ctx?.agent?.kind !== "main") return;
      mainContext = ctx;
      try {
        if (!supportedEvents) {
          publish(current(ctx), "unsupported", { reason: "event_api_unavailable" });
          return;
        }
        handler(event, ctx);
      } catch {
        let failed;
        try { failed = current(ctx); } catch {}
        publish(failed, "unsupported", { reason: "account_api_unavailable" });
      }
    };
  }

  function startTimer(ctx) {
    if (timer !== undefined) return true;
    // No public model/account-change event in v18.22.1. Its managed timer is
    // unref'd and cleared on shutdown. Read-only sticky changes only invalidate;
    // they never establish an account or cause unchanged-tick roster/file IO.
    if (typeof ctx.setInterval !== "function" || typeof ctx.clearTimer !== "function") return false;
    timerContext = ctx;
    timer = ctx.setInterval(() => {
      guarded((_event, active) => { checkSelection(active); })(undefined, mainContext);
    }, 500);
    return true;
  }

  function reset(_event, ctx) {
    invalidate(ctx, "awaiting_response");
    if (!startTimer(ctx)) publish(scope, "unsupported", { reason: "model_observer_unavailable" });
  }

  function observe(event, ctx) {
    if (event?.message?.role !== "assistant") return;
    const selected = checkScope(ctx);
    if (!startTimer(ctx)) {
      publish(selected, "unsupported", { reason: "model_observer_unavailable" });
      return;
    }
    const message = event.message;
    const provider = text(message.provider, 256);
    const model = text(message.model, 256);
    const credentialId = message.credentialId;
    if (!selected.sessionId || !provider || !model || provider !== selected.provider || model !== selected.model) {
      evidence = undefined;
      publish(selected, "unresolved", { reason: "message_selection_mismatch" });
      return;
    }
    if (!Number.isSafeInteger(credentialId) || credentialId <= 0) {
      evidence = undefined;
      publish(selected, "unresolved", { reason: "credential_not_observed" });
      return;
    }
    const pin = sticky(ctx, provider, selected.sessionId);
    if (Number.isSafeInteger(pin?.credentialId) && pin.credentialId > 0 && pin.credentialId !== credentialId) {
      invalidate(ctx, "account_selection_changed");
      return;
    }
    if (evidence?.sessionId === selected.sessionId && evidence.provider === provider && evidence.model === model && evidence.credentialId === credentialId) return;
    evidence = { sessionId: selected.sessionId, provider, model, credentialId };

    const auth = ctx.modelRegistry?.authStorage;
    if (typeof auth?.oauth?.accounts !== "function") {
      publish(selected, "unsupported", { reason: "account_api_unavailable" });
      return;
    }
    const accounts = auth.oauth.accounts(provider, selected.sessionId);
    if (!Array.isArray(accounts)) {
      publish(selected, "unsupported", { reason: "account_list_unavailable" });
      return;
    }
    const account = accounts.find((row) => row?.credentialId === credentialId);
    evidence.pinMatched = pin?.credentialId === credentialId;
    const fields = { provider, model, credentialId, kind: "credential" };
    if (account) {
      fields.kind = "oauth";
      for (const key of ["email", "accountId", "orgId", "orgName"]) {
        const value = text(account[key], 512);
        if (value !== undefined) fields[key] = value;
      }
    } else if (pin?.credentialId === credentialId && pin.type === "api_key") {
      fields.kind = "api_key";
    }
    publish(selected, "observed", fields);
  }

  function on(name, handler) {
    try { pi.on(name, guarded(handler)); } catch { supportedEvents = false; }
  }

  for (const name of ["session_start", "session_switch", "session_branch", "session_clear", "session_tree", "before_agent_start", "turn_start", "retry_fallback_applied", "auto_retry_start"]) {
    on(name, reset);
  }
  on("message_update", observe);
  on("message_end", observe);
  on("session_shutdown", (_event, ctx) => {
    if (timer !== undefined) timerContext.clearTimer(timer);
    timer = undefined;
    timerContext = undefined;
    invalidate(ctx, "session_stopped");
  });
}
