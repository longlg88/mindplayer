import { renameSync, rmSync, writeFileSync } from "node:fs";

// This extension is loaded only for a new MindPlayer OMP process. State belongs
// to the extension instance, not the module, because OMP also binds factories
// in child sessions.
export default function (pi) {
  let selected = false;
  let picking;
  let selectionGeneration = 0;

  function revokeSelection() {
    selectionGeneration++;
    if (process.env.MINDPLAYER_OMP_PICKER_SELECTED) {
      rmSync(process.env.MINDPLAYER_OMP_PICKER_SELECTED, { force: true });
    }
  }

  const isMain = (ctx) => ctx.agent.kind === "main";
  const blockedMessage = "Choose a model first with /mindplayer-model. Use /login to add credentials.";

  function showBlocked(ctx) {
    ctx.ui.setStatus("mindplayer-model", "Model required: /mindplayer-model");
    ctx.ui.notify(blockedMessage, "warning");
  }

  async function chooseModel(ctx) {
    if (!isMain(ctx)) return;
    if (picking) return picking;
    revokeSelection();
    selected = false;
    picking = (async () => {
      try {
        if (!ctx.hasUI || ctx.mode !== "tui") {
          showBlocked(ctx);
          return;
        }
        // list() is OMP's authenticated, enabled chat-model catalog. It may
        // include cached models, not a live entitlement check for each model.
        const models = ctx.models.list().slice().sort((a, b) =>
          `${a.provider}/${a.id}`.localeCompare(`${b.provider}/${b.id}`),
        );
        if (models.length === 0) {
          ctx.ui.notify("No available chat models. Use /login, then /mindplayer-model.", "warning");
          showBlocked(ctx);
          return;
        }
        const options = models.map((model) => ({
          label: `${model.provider}/${model.id}`,
          description: model.name || undefined,
        }));
        const choice = await ctx.ui.select(
          "Choose a model and provider\nThis choice applies to chat; auxiliary models may use other routes.",
          options,
        );
        if (choice === undefined) {
          showBlocked(ctx);
          return;
        }
        const index = options.findIndex((option) => option.label === choice);
        if (index < 0 || !(await pi.setModel(models[index]))) {
          ctx.ui.notify("Could not switch to that model. Choose another with /mindplayer-model.", "error");
          showBlocked(ctx);
          return;
        }
        selected = true;
        ctx.ui.setStatus("mindplayer-model", undefined);
        ctx.ui.notify(`Selected ${choice}`, "info");
        const generation = selectionGeneration;
        // Let the chooser and its enclosing session/command handler return
        // before the host sends input to the normal editor.
        setImmediate(() => {
          if (!selected || generation !== selectionGeneration ||
              !process.env.MINDPLAYER_OMP_PICKER_SELECTED) return;
          try {
            const marker = process.env.MINDPLAYER_OMP_PICKER_SELECTED;
            writeFileSync(`${marker}.tmp`, "selected\n");
            renameSync(`${marker}.tmp`, marker);
          } catch (error) {
            ctx.ui.notify(`Could not acknowledge model selection: ${error instanceof Error ? error.message : String(error)}`, "error");
          }
        });
      } catch (error) {
        ctx.ui.notify(`Model selection failed: ${error instanceof Error ? error.message : String(error)}`, "error");
        showBlocked(ctx);
      }
    })();
    try {
      await picking;
    } finally {
      picking = undefined;
    }
  }

  pi.on("session_start", async (_event, ctx) => {
    if (!isMain(ctx)) return;
    if (!ctx.hasUI || ctx.mode !== "tui" || typeof ctx.models?.list !== "function" ||
        typeof ctx.ui.select !== "function" || typeof pi.setModel !== "function") {
      throw new Error("MindPlayer requires an interactive OMP with model picker support.");
    }
    // OMP has initialized the runner and its UI by the time session_start fires.
    // The host keeps the pane closed to input until this marker is present.
    if (process.env.MINDPLAYER_OMP_PICKER_READY) {
      writeFileSync(process.env.MINDPLAYER_OMP_PICKER_READY, "ready\n");
    }
    await chooseModel(ctx);
  });

  // /resume inside this process must retain the resumed session's model.
  // /new starts another new session and requires a fresh explicit choice.
  pi.on("session_switch", async (event, ctx) => {
    if (!isMain(ctx)) return;
    revokeSelection();
    selected = event.reason !== "new";
    if (selected) ctx.ui.setStatus("mindplayer-model", undefined);
    else await chooseModel(ctx);
  });

  pi.registerCommand("mindplayer-model", {
    description: "Choose an available model and provider",
    handler: async (_args, ctx) => { await chooseModel(ctx); },
  });

  pi.on("input", async (event, ctx) => {
    if (!isMain(ctx) || selected) return;
    // Input hooks run before command dispatch. Only commands needed to recover,
    // leave, or move sessions are allowed while a choice is outstanding.
    if (/^\/(?:mindplayer-model|login|exit|quit|new|resume)(?:\s|$)/i.test(event.text.trim())) return;
    showBlocked(ctx);
    return { handled: true };
  });

  // Synthetic continuations and extension-injected messages can bypass input.
  // abort() starts cancellation synchronously but waits for idle; awaiting it
  // from an agent callback would deadlock the turn that must become idle.
  const abortIfBlocked = (_event, ctx) => {
    if (isMain(ctx) && !selected) {
      showBlocked(ctx);
      void ctx.abort().catch((error) => {
        ctx.ui.notify(`Could not abort blocked request: ${error instanceof Error ? error.message : String(error)}`, "error");
      });
    }
  };
  pi.on("before_agent_start", abortIfBlocked);
  pi.on("agent_start", abortIfBlocked);
  pi.on("before_provider_request", abortIfBlocked);

}
