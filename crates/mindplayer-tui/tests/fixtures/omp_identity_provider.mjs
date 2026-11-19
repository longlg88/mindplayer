import { appendFileSync, writeFileSync } from "node:fs";
import { AssistantMessageEventStream } from "@oh-my-pi/pi-ai";

// Native AuthStorage chooses real rows in an isolated fixture database. The
// transport never connects to a network and never stamps credentialId itself.
export default function (pi) {
  for (const [provider, modelId] of [
    ["mindplayer-oauth-fixture", "native-identity-chat"],
    ["mindplayer-key-fixture", "native-identity-key"],
  ]) {
    pi.registerProvider(provider, {
      api: "mindplayer-identity-local",
      baseUrl: "http://127.0.0.1:9",
      oauth: {
        name: "Local identity fixture",
        login: async () => { throw new Error("fixture never logs in"); },
        refreshToken: async () => { throw new Error("fixture never refreshes"); },
        getApiKey: (credential) => credential.access,
      },
      usage: {
        id: provider,
        fetchUsage: async () => ({ provider, fetchedAt: Date.now(), limits: [] }),
      },
      models: [{
        id: modelId, name: modelId, reasoning: false, input: ["text"],
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
        contextWindow: 128000, maxTokens: 4096,
      }],
      streamSimple(model, _context, options) {
        const served = new Map([
          ["fixture-oauth-a", "a"], ["fixture-oauth-b", "b"], ["fixture-api-key", "key"],
        ]).get(options.apiKey);
        if (!served) throw new Error("native auth did not provide a fixture credential");
        appendFileSync(process.env.MINDPLAYER_FIXTURE_RECEIPT,
          JSON.stringify({ served, provider: model.provider, sessionId: options.sessionId }) + "\n");
        const stream = new AssistantMessageEventStream();
        queueMicrotask(() => {
          const message = {
            role: "assistant", content: [{ type: "text", text: "LOCAL-IDENTITY-REPLY" }],
            api: model.api, provider: model.provider, model: model.id,
            usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2,
              cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
            stopReason: "stop", timestamp: Date.now(),
          };
          stream.push({ type: "start", partial: message });
          stream.push({ type: "done", reason: "stop", message });
          stream.end();
        });
        return stream;
      },
    });
  }
  function pin(ctx, email) {
    if (ctx.agent.kind !== "main") return;
    const auth = ctx.modelRegistry.authStorage;
    const session = ctx.sessionManager.getSessionId();
    const account = auth.oauth.accounts("mindplayer-oauth-fixture", session)
      .find((row) => row.email === email);
    if (!account || !auth.sessions.pin("mindplayer-oauth-fixture", session, account.credentialId)) {
      throw new Error("could not select the isolated fixture account");
    }
    writeFileSync(process.env.MINDPLAYER_FIXTURE_PIN, email);
  }
  pi.on("session_start", (_event, ctx) => pin(ctx, "second@example.test"));
  pi.registerCommand("fixture-account", {
    description: "Select a local synthetic account",
    handler: async (_args, ctx) => pin(ctx, "first@example.test"),
  });
}
