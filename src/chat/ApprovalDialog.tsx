import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, type ApprovalRequest } from "../shared/api";

function DiffView({ diff }: { diff: string }) {
  return (
    <pre className="diff">
      {diff.split("\n").map((raw, i) => {
        // CRLF 文件的 diff 行末带 \r，显示时去掉，否则会多出空行
        const line = raw.replace(/\r$/, "");
        const cls = line.startsWith("+++") || line.startsWith("---") ? "file" : line.startsWith("@@") ? "hunk" : line.startsWith("+") ? "add" : line.startsWith("-") ? "del" : "";
        return (
          <div key={i} className={cls}>
            {line || " "}
          </div>
        );
      })}
    </pre>
  );
}

/**
 * 模型要写入或修改文件时的确认框；排队处理，任意窗口回答后所有窗口同步关闭。
 * inline：嵌在气泡里显示（不是全屏遮罩）。
 */
export function ApprovalDialog({ inline = false, onPendingChange }: { inline?: boolean; onPendingChange?: (pending: boolean) => void }) {
  const [queue, setQueue] = useState<ApprovalRequest[]>([]);

  useEffect(() => {
    onPendingChange?.(queue.length > 0);
  }, [queue.length, onPendingChange]);

  useEffect(() => {
    const subs = Promise.all([
      listen<ApprovalRequest>("tool-approval", (e) => setQueue((q) => [...q, e.payload])),
      listen<number>("tool-approval-done", (e) => setQueue((q) => q.filter((r) => r.id !== e.payload))),
    ]);
    return () => void subs.then((fns) => fns.forEach((f) => f()));
  }, []);

  const req = queue[0];
  if (!req) return null;
  const answer = (d: "allow" | "allowTurn" | "deny") => {
    setQueue((q) => q.filter((r) => r.id !== req.id));
    void api.toolApprove(req.id, d);
  };
  const verb = req.created ? "新建文件" : req.tool === "write_file" ? "覆盖文件" : "修改文件";

  return (
    <div className={inline ? "approval-inline" : "approval-backdrop"}>
      <div className="approval" role="dialog" aria-modal={!inline}>
        <header>
          <strong>{verb}</strong>
          <span className="stats">
            <span className="add">+{req.added}</span> <span className="del">-{req.removed}</span>
          </span>
        </header>
        <div className="path" title={req.path}>
          {req.path}
        </div>
        <DiffView diff={req.diff} />
        <footer>
          <span className="muted small">{queue.length > 1 ? `还有 ${queue.length - 1} 个待确认` : inline ? "5 分钟不确认自动拒绝" : "5 分钟内不确认会自动拒绝"}</span>
          <button className="danger" onClick={() => answer("deny")}>
            拒绝
          </button>
          <button onClick={() => answer("allowTurn")} title="这一轮里后续的写入和修改不再询问">
            本轮都允许
          </button>
          <button className="primary" onClick={() => answer("allow")}>
            允许
          </button>
        </footer>
      </div>
    </div>
  );
}
