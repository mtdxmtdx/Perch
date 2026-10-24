// 首次启动的接入询问：本机检测到 Claude Code / Codex / DSH 但还没接入时，在气泡里问一次。
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorText, type AgentId, type OfferItem, type OnboardResult } from "../shared/api";

/** 把接入结果说成一两句话 */
function summary(results: OnboardResult[]): string {
  if (!results.length) return "好的～以后想让我盯着它们，去 设置 → Agent 接入 打开就行。";
  const ok = results.filter((r) => !r.error);
  const failed = results.filter((r) => r.error);
  const parts: string[] = [];
  if (ok.length) {
    parts.push(`接好啦：${ok.map((r) => r.name).join("、")}。`);
    if (ok.some((r) => r.agent === "codex")) parts.push("记得在 Codex 里输入 /hooks 信任一次。");
    if (ok.some((r) => r.agent === "dsh")) parts.push("重启 DeepSeek Harness 后生效。");
  }
  for (const r of failed) parts.push(`${r.name} 没接上：${r.error}（可以稍后在 设置 → Agent 接入 重试）`);
  return parts.join("");
}

export function AgentOffer({ onPendingChange, onFinished }: { onPendingChange: (p: boolean) => void; onFinished: (msg: string) => void }) {
  const [items, setItems] = useState<OfferItem[] | null>(null);
  const [picked, setPicked] = useState<AgentId[]>([]);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const show = (list: OfferItem[] | null) => {
      setItems(list && list.length ? list : null);
      setPicked((list ?? []).filter((i) => i.ready).map((i) => i.agent));
    };
    void api.agentsOffer().then(show);
    const un = listen<OfferItem[]>("agent-offer", (e) => show(e.payload));
    return () => void un.then((f) => f());
  }, []);

  useEffect(() => onPendingChange(!!items), [items, onPendingChange]);

  if (!items) return null;

  const answer = async (agents: AgentId[]) => {
    setBusy(true);
    try {
      const results = await api.agentsOnboard(agents);
      setItems(null);
      onFinished(summary(results));
    } catch (e) {
      onFinished(`没接上：${errorText(e)}`);
      setItems(null);
    } finally {
      setBusy(false);
    }
  };

  const toggle = (a: AgentId, on: boolean) => setPicked((p) => (on ? [...p, a] : p.filter((x) => x !== a)));

  return (
    <div className="agent-offer">
      <div className="offer-title">发现你装了这些编程助手，要我帮你盯着它们干活吗？</div>
      <ul>
        {items.map((it) => (
          <li key={it.agent} className={it.ready ? "" : "disabled"}>
            <label>
              <input
                type="checkbox"
                disabled={!it.ready || busy}
                checked={picked.includes(it.agent)}
                onChange={(e) => toggle(it.agent, e.target.checked)}
              />
              <span className="name">{it.name}</span>
            </label>
            {it.hint && <div className="hint">{it.hint}</div>}
          </li>
        ))}
      </ul>
      <div className="offer-note">会在它们的配置里加几条转发事件的 hook（改之前先备份），随时可以在设置里关掉。</div>
      <div className="offer-actions">
        <button disabled={busy} onClick={() => void answer([])}>
          以后再说
        </button>
        <button className="primary" disabled={busy || !picked.length} onClick={() => void answer(picked)}>
          {busy ? "接入中…" : "接入"}
        </button>
      </div>
    </div>
  );
}
