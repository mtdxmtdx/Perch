// 新用户引导：还没配对话模型时，在气泡里选一家服务商、粘贴 API Key，测试通过就保存。

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorText } from "../shared/api";
import { CHAT_PRESETS, providerFrom } from "../shared/presets";

export function ModelSetup({ onPendingChange, onFinished }: { onPendingChange: (p: boolean) => void; onFinished: (msg: string) => void }) {
  const [open, setOpen] = useState(false);
  const [idx, setIdx] = useState(0);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    void api.modelSetupPending().then(setOpen);
    const uns = [
      listen("model-setup", () => {
        setOpen(true);
        setErr(null);
      }),
      // 在设置页里配好了模型：卡片自己收起
      listen("settings-changed", () => void api.modelSetupPending().then((p) => !p && setOpen(false))),
    ];
    return () => uns.forEach((u) => void u.then((f) => f()));
  }, []);

  useEffect(() => onPendingChange(open), [open, onPendingChange]);

  if (!open) return null;
  const preset = CHAT_PRESETS[idx];

  const save = async () => {
    setBusy(true);
    setErr(null);
    try {
      await api.modelSetupSave(providerFrom(preset, ""), key);
      setOpen(false);
      setKey("");
      onFinished(`接好啦（${preset.label}）！现在可以跟我聊天了～模型名、推理强度这些以后可以在 设置 → 模型 里改。`);
    } catch (e) {
      setErr(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const later = () => {
    void api.modelSetupDismiss();
    setOpen(false);
    onFinished("好的～想聊天的时候，在 设置 → 模型 里添加一个就行。");
  };

  return (
    <div className="agent-offer model-setup">
      <div className="offer-title">我还没接上对话模型，暂时没法陪你聊天～选一家服务商，把 API Key 给我吧：</div>
      <select value={idx} disabled={busy} onChange={(e) => setIdx(Number(e.target.value))}>
        {CHAT_PRESETS.map((p, i) => (
          <option key={p.label} value={i}>
            {p.label}
          </option>
        ))}
      </select>
      {preset.hint && <div className="offer-note">{preset.hint}</div>}
      <input
        type="password"
        value={key}
        disabled={busy}
        placeholder="粘贴 API Key"
        onChange={(e) => setKey(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && key.trim()) void save();
        }}
      />
      <div className="offer-note">
        {preset.keyUrl && (
          <>
            <button className="link" onClick={() => void api.openKeyPage(preset.keyUrl!)}>
              去哪里拿 Key？
            </button>{" "}
          </>
        )}
        Key 只存在 Windows 凭据管理器里。用别的服务商可以去{" "}
        <button className="link" onClick={() => void api.openSettings()}>
          设置
        </button>{" "}
        里添加。
      </div>
      {err && <div className="error">{err}</div>}
      <div className="offer-actions">
        <button disabled={busy} onClick={later}>
          以后再说
        </button>
        <button className="primary" disabled={busy || !key.trim()} onClick={() => void save()}>
          {busy ? "测试中…" : "连接"}
        </button>
      </div>
    </div>
  );
}
