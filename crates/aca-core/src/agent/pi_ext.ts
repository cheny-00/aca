// The extension passed in with -e when aca starts pi (crates/aca-core/src/agent/pi.rs), not installed into pi's config directory.
// Only forwards: pi's events become hook-protocol shape and go to `aca hook pi`; how the state changes is on aca's side
// (agent/lifecycle.rs).
// Does nothing when not started by aca (no ACA_SESSION_ID in the environment). Silent on any error: failing to report
// mustn't get in the way of the conversation.
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";

export default function (pi: any) {
  const { ACA_SESSION_ID: id, ACA_BIN: bin, ACA_TOOL: tool, ACA_PI_MCP: mcp } = process.env;
  if (!id || !bin) return;
  // MCP servers from enabled aca plugins (a list aca wrote, { name: { command, args } }): for this start only
  if (mcp) {
    try {
      for (const [name, server] of Object.entries(JSON.parse(readFileSync(mcp, "utf8")))) pi.registerMcpServer(name, server);
    } catch {}
  }
  // Report one after another: aca receives them in the same order they happen here
  let chain: Promise<void> = Promise.resolve();
  const report = (ctx: any, event: string, extra: Record<string, unknown> = {}) => {
    const sm = ctx?.sessionManager;
    const payload = {
      hook_event_name: event,
      session_id: sm?.getSessionId?.() ?? "",
      transcript_path: sm?.getSessionFile?.() ?? "",
      cwd: ctx?.cwd ?? "",
      title: sm?.getSessionName?.() ?? "",
      ...extra,
    };
    chain = chain.then(
      () =>
        new Promise<void>((done) => {
          try {
            const p = spawn(bin, ["hook", tool || "pi"], { stdio: ["pipe", "ignore", "ignore"] });
            p.on("error", () => done());
            p.on("close", () => done());
            p.stdin.on("error", () => {});
            p.stdin.end(JSON.stringify(payload));
          } catch {
            done();
          }
        }),
    );
    return chain;
  };
  const text = (content: any) =>
    typeof content === "string" ? content : (content ?? []).filter((c: any) => c?.type === "text").map((c: any) => c.text).join("");

  // This turn's last reply: on upstream errors pi retries itself, with an agent_end per attempt; agent_settled is the real end
  let last: any = null;
  // A prompt an extension pops up during a turn (permission confirmations and the like): one popped up while idle is for a
  // command you typed yourself, so you're right there; not reported
  let asking = false;

  pi.on("session_start", (e: any, ctx: any) => {
    if (e.reason !== "reload") report(ctx, "SessionStart", { source: e.reason });
  });
  pi.on("session_info_changed", (_e: any, ctx: any) => report(ctx, "Notification", { notification_type: "title" }));
  pi.on("before_agent_start", (e: any, ctx: any) => {
    last = null;
    report(ctx, "UserPromptSubmit", { prompt: e.messageType === "user" ? e.prompt : text(e.message?.content) });
  });
  pi.on("agent_end", (e: any) => {
    last = [...(e.messages ?? [])].reverse().find((m: any) => m?.role === "assistant") ?? null;
  });
  pi.on("agent_settled", (e: any, ctx: any) => {
    if (e.aborted) report(ctx, "Interrupt");
    else if (last?.stopReason === "error") report(ctx, "StopFailure", { error: last.errorMessage || "" });
    else report(ctx, "Stop", { last_assistant_message: text(last?.content) });
    last = null;
  });
  pi.on("ui_prompt_start", (e: any, ctx: any) => {
    if (ctx?.isIdle?.()) return;
    asking = true;
    report(ctx, "Notification", { notification_type: "elicitation_dialog", message: e.title || e.kind });
  });
  pi.on("ui_prompt_end", (_e: any, ctx: any) => {
    if (asking) report(ctx, "PostToolUse");
    asking = false;
  });
  pi.on("session_before_compact", (e: any, ctx: any) => report(ctx, "PreCompact", { trigger: e.reason }));
  pi.on("session_compact", (e: any, ctx: any) => report(ctx, "PostCompact", { trigger: e.reason }));
  pi.on("session_compact_failed", (e: any, ctx: any) => report(ctx, "PostCompact", { trigger: e.reason }));
  // On exit, let pi go only after it finishes reporting; /new, /resume and /fork switch conversations, followed right away
  // by the new conversation's session_start
  pi.on("session_shutdown", (e: any, ctx: any) => {
    if (e.reason === "reload") return;
    return report(ctx, "SessionEnd", { reason: e.reason === "quit" ? "quit" : e.reason === "resume" ? "resume" : "clear" });
  });
}
