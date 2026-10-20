// aca 起 opencode 时带进来的插件(crates/aca-core/src/agent/opencode.rs),不装进 opencode 的配置目录。
// 只转发:opencode 的事件换成 hook 协议的形状交给 `aca hook opencode`,状态怎么变在 aca 那边(agent/lifecycle.rs)。
// 不是 aca 起的(环境里没有 ACA_SESSION_ID)什么都不做。出什么错都不吭声:报不上不该碍着对话。
import { spawn } from "node:child_process";

export const AcaPlugin = async () => {
  const { ACA_SESSION_ID: id, ACA_BIN: bin, ACA_TOOL: tool } = process.env;
  if (!id || !bin) return {};
  // task 工具派出去的子 agent 也是一段会话、也报 idle:不是这段对话本身
  const children = new Set();
  const titles = new Map();
  // 每段对话最后一条回复:回复的 id,和各条回复里的字
  const lastReply = new Map();
  const texts = new Map();
  // 这一轮:哪段对话、结束报过没有、你在权限框上点了拒绝没有(拒绝了它就停下,不报错)
  let turn = null;

  // 一个报完再报下一个:aca 那边收到的先后和这边发生的一样
  let chain = Promise.resolve();
  const report = (event, sid, extra = {}) => {
    const payload = { hook_event_name: event, session_id: sid ?? "", title: titles.get(sid) ?? "", ...extra };
    chain = chain.then(
      () =>
        new Promise((done) => {
          try {
            const p = spawn(bin, ["hook", tool || "opencode"], { stdio: ["pipe", "ignore", "ignore"] });
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
  const mine = (sid) => sid && !children.has(sid);
  // 还没起好标题时它叫「New session - <时刻>」
  const named = (t) => (t && !/^New session - \d{4}-\d\d-\d\dT/.test(t) ? t : "");
  // 一轮结束只报一次(它连着报两次 idle;出错、打断之后也还有一次 idle)
  const end = (sid, event, extra) => {
    if (!turn || turn.sid !== sid || turn.over) return;
    turn.over = true;
    report(event, sid, extra);
  };

  return {
    "chat.message": async (input, output) => {
      if (!mine(input.sessionID)) return;
      const prompt = (output?.parts ?? []).filter((p) => p.type === "text" && !p.synthetic).map((p) => p.text).join("\n");
      turn = { sid: input.sessionID, over: false, rejected: false };
      report("UserPromptSubmit", input.sessionID, { prompt });
    },
    "experimental.session.compacting": async (input) => {
      if (mine(input.sessionID)) report("PreCompact", input.sessionID, { trigger: "auto" });
    },
    event: async ({ event }) => {
      const p = event.properties ?? {};
      switch (event.type) {
        case "session.created": {
          const s = p.info ?? {};
          if (s.parentID) return void children.add(s.id);
          if (named(s.title)) titles.set(s.id, s.title);
          return void report("SessionStart", s.id, { source: "startup", cwd: s.directory ?? "" });
        }
        case "session.updated": {
          const s = p.info ?? {};
          if (s.parentID || !mine(s.id) || !named(s.title) || titles.get(s.id) === s.title) return;
          titles.set(s.id, s.title);
          return void report("Notification", s.id, { notification_type: "title" });
        }
        case "message.updated": {
          const m = p.info ?? {};
          if (m.role === "assistant" && mine(m.sessionID)) lastReply.set(m.sessionID, m.id);
          return;
        }
        case "message.part.updated": {
          const part = p.part ?? {};
          if (part.type === "text" && !part.synthetic && mine(part.sessionID)) texts.set(part.messageID, part.text ?? "");
          return;
        }
        case "permission.asked": {
          if (!mine(p.sessionID)) return;
          const m = p.metadata ?? {};
          const input = { command: m.command ?? (p.patterns ?? []).join(" "), file_path: m.filepath ?? m.filePath, url: m.url };
          return void report("PermissionRequest", p.sessionID, { tool_name: p.permission, tool_input: input });
        }
        case "permission.replied":
          if (!mine(p.sessionID)) return;
          if (p.reply === "reject" && turn) turn.rejected = true;
          return void report("PostToolUse", p.sessionID);
        case "question.asked": {
          if (!mine(p.sessionID)) return;
          const questions = (p.questions ?? []).map((q) => ({ question: q.question, header: q.header, options: q.options ?? [], multiSelect: !!q.multiple }));
          return void report("PreToolUse", p.sessionID, { tool_name: "question", tool_input: { questions } });
        }
        case "question.replied":
        case "question.rejected":
          if (mine(p.sessionID)) report("PostToolUse", p.sessionID);
          return;
        case "session.error": {
          if (!mine(p.sessionID)) return;
          const e = p.error ?? {};
          if (e.name === "MessageAbortedError") return end(p.sessionID, "Interrupt");
          const msg = e.data?.message || e.name || "";
          return end(p.sessionID, "StopFailure", { error: e.data?.statusCode ? `${e.data.statusCode}: ${msg}` : msg });
        }
        case "session.idle": {
          if (!mine(p.sessionID)) return;
          if (turn?.rejected) return end(p.sessionID, "Interrupt");
          return end(p.sessionID, "Stop", { last_assistant_message: texts.get(lastReply.get(p.sessionID)) ?? "" });
        }
        case "session.compacted":
          if (mine(p.sessionID)) report("PostCompact", p.sessionID, { trigger: "auto" });
          return;
      }
    },
  };
};
