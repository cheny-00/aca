// The plugin passed in when aca starts opencode (crates/aca-core/src/agent/opencode.rs), not installed into opencode's config directory.
// Only forwards: opencode's events become hook-protocol shape and go to `aca hook opencode`; how the state changes is on
// aca's side (agent/lifecycle.rs).
// Does nothing when not started by aca (no ACA_SESSION_ID in the environment). Silent on any error: failing to report
// mustn't get in the way of the conversation.
import { spawn } from "node:child_process";

export const AcaPlugin = async () => {
  const { ACA_SESSION_ID: id, ACA_BIN: bin, ACA_TOOL: tool } = process.env;
  if (!id || !bin) return {};
  // Subagents spawned by the task tool are sessions too and also report idle: not this conversation itself
  const children = new Set();
  const titles = new Map();
  // Each conversation's last reply: the reply's id, and the text of each reply
  const lastReply = new Map();
  const texts = new Map();
  // This turn: which conversation, whether its end was reported, whether you denied on a permission prompt (denying stops
  // it, without an error)
  let turn = null;

  // Report one after another: aca receives them in the same order they happen here
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
  // Before it has a title it's called "New session - <time>"
  const named = (t) => (t && !/^New session - \d{4}-\d\d-\d\dT/.test(t) ? t : "");
  // Report a turn's end only once (it reports idle twice in a row; there's another idle after an error or interrupt too)
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
