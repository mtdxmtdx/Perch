import type { ToolLine, ToolStatus } from "./api";
import "./tool-lines.css";

const ICON: Record<ToolStatus, string> = { running: "⏳", ok: "✓", error: "✕", denied: "⊘" };
const LABEL: Record<ToolStatus, string> = { running: "进行中", ok: "完成", error: "失败", denied: "已拒绝" };

/** 一轮对话里的工具操作：读取、修改、画图…… */
export function ToolLines({ lines, compact = false }: { lines: ToolLine[]; compact?: boolean }) {
  if (!lines.length) return null;
  return (
    <ul className={`tool-lines ${compact ? "compact" : ""}`}>
      {lines.map((l) => (
        <li key={l.id} className={l.status} title={l.detail ?? LABEL[l.status]}>
          <span className="icon">{ICON[l.status]}</span>
          <span className="title">{l.title}</span>
          {!compact && l.detail && <span className="detail">{l.detail}</span>}
        </li>
      ))}
    </ul>
  );
}
