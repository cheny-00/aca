// aca 起 pi 时用 -e 带进来的扩展(crates/aca-core/src/agent/pi.rs),不装进 pi 的配置目录。
// 只转发:pi 的事件换成 hook 协议的形状交给 `aca hook pi`,状态怎么变在 aca 那边(agent/lifecycle.rs)。
// 不是 aca 起的(环境里没有 ACA_SESSION_ID)什么都不做。出什么错都不吭声:报不上不该碍着对话。
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";

export default function (pi: any) {
  const { ACA_SESSION_ID: id, ACA_BIN: bin, ACA_TOOL: tool, ACA_PI_MCP: mcp } = process.env;
  if (!id || !bin) return;
  // 开着的 aca 插件给的 MCP 服务(aca 写好的一份 { 名字: { command, args } }):只对这一次启动
  if (mcp) {
    try {
      for (const [name, server] of Object.entries(JSON.parse(readFileSync(mcp, "utf8")))) pi.registerMcpServer(name, server);
    } catch {}
  }
  // 一个报完再报下一个:aca 那边收到的先后和这边发生的一样
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

  // 这一轮最后一条回复:上游出错时 pi 自己重试,每试一次都有一个 agent_end,agent_settled 才是真的结束
  let last: any = null;
  // 一轮里扩展弹的框(权限确认这类):闲着时弹的是你自己敲的命令,人就在跟前,不报
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
  // 退出时等它报完再让 pi 走;/new、/resume、/fork 换的是对话,紧跟着就是新对话的 session_start
  pi.on("session_shutdown", (e: any, ctx: any) => {
    if (e.reason === "reload") return;
    return report(ctx, "SessionEnd", { reason: e.reason === "quit" ? "quit" : e.reason === "resume" ? "resume" : "clear" });
  });
}
