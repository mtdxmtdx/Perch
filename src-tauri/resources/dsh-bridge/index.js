// Perch 的 DeepSeek Harness 插件：把会话状态转发给本机的桌宠。
// 只发送状态和工作目录；只有在桌宠开启"完成时总结"时，才附带最后一条回复的文字。
import { readFileSync } from "node:fs";

export const name = "perch-dsh-bridge";

const config = (() => {
  try {
    return JSON.parse(readFileSync(new URL("./config.json", import.meta.url), "utf8"));
  } catch {
    return null;
  }
})();

function post(body) {
  if (!config) return;
  try {
    fetch(`http://127.0.0.1:${config.port}/v1/agent-event/dsh`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-perch-token": config.token },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(1500),
    }).catch(() => {});
  } catch {
    // 桌宠没运行时什么都不做，绝不影响 DSH
  }
}

function textOf(message) {
  const content = message?.content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((b) => b?.type === "text" && typeof b.text === "string")
    .map((b) => b.text)
    .join("");
}

export function apply(ctx) {
  const lastText = new Map();
  const base = (session) => ({ session_id: String(session?.id ?? ""), cwd: String(session?.header?.cwd ?? "") });

  ctx.on("session/created", (session) => post({ ...base(session), event: "session_start" }));
  ctx.on("session/disposed", (session) => {
    lastText.delete(session?.id);
    post({ ...base(session), event: "session_end" });
  });
  ctx.on("session/event", (session, event) => {
    const data = event?.data ?? {};
    switch (event?.type) {
      case "turn/start":
        lastText.delete(session.id);
        return post({ ...base(session), event: "prompt" });
      case "tool/call":
        return post({ ...base(session), event: "tool_start", tool: String(data.name ?? "") });
      case "tool/result":
        return post({ ...base(session), event: "tool_end", error: Boolean(data.message?.isError ?? data.error) });
      case "approval/asked":
        return post({ ...base(session), event: "need_permission" });
      case "approval/decided":
        return post({ ...base(session), event: "resume" });
      case "assistant/message": {
        const t = textOf(data.message);
        if (t) lastText.set(session.id, t);
        return;
      }
      case "turn/end": {
        const kind = data.reason?.kind;
        if (kind === "error") return post({ ...base(session), event: "error", message: String(data.reason?.error?.message ?? "") });
        if (kind === "aborted") return post({ ...base(session), event: "stop", canceled: true });
        const final_message = config?.summarize ? (lastText.get(session.id) ?? "") : "";
        return post({ ...base(session), event: "stop", final_message });
      }
    }
  });
}
