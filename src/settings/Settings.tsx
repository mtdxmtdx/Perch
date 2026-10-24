import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  api,
  errorText,
  type ChatParams,
  type ImageParams,
  type Memory,
  type Persona,
  type Provider,
  type Roles,
  type SkinEntry,
  type ToolParams,
  type VoiceParams,
  type VoiceStatus,
  type DownloadProgress,
  type AgentsParams,
  type AgentsOverview,
  type AgentId,
} from "../shared/api";
import { SessionList, useSessions } from "../shared/Sessions";
import "./settings.css";

interface Draft {
  providers: Provider[];
  roles: Roles;
  personas: Persona[];
  activePersona: string;
  chat: ChatParams;
  tools: ToolParams;
  image: ImageParams;
  voice: VoiceParams;
  agents: AgentsParams;
}

const newId = () => crypto.randomUUID().slice(0, 8);

// 模型名只是示例，以厂商文档为准
const PRESETS: { label: string; base_url: string; model: string; style: string; vision?: boolean }[] = [
  { label: "DeepSeek", base_url: "https://api.deepseek.com/v1", model: "deepseek-flash", style: "deepseek", vision: true },
  { label: "通义千问（阿里云百炼）", base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus", style: "qwen" },
  { label: "硅基流动", base_url: "https://api.siliconflow.cn/v1", model: "deepseek-ai/DeepSeek-V3", style: "openai" },
  { label: "OpenRouter", base_url: "https://openrouter.ai/api/v1", model: "openai/gpt-4o-mini", style: "openrouter" },
  { label: "OpenAI", base_url: "https://api.openai.com/v1", model: "gpt-4o-mini", style: "openai" },
  { label: "自定义", base_url: "https://", model: "", style: "openai" },
];

const EFFORTS: [string, string][] = [
  ["", "不设置（默认）"],
  ["off", "关闭思考"],
  ["low", "低"],
  ["medium", "中"],
  ["high", "高"],
];

const STYLES: [string, string][] = [
  ["openai", "OpenAI 格式（reasoning_effort）"],
  ["openrouter", "OpenRouter 格式（reasoning.effort）"],
  ["qwen", "通义千问格式（enable_thinking）"],
  ["deepseek", "DeepSeek 格式（thinking + reasoning_effort）"],
];

type Tab = "models" | "personas" | "tools" | "voice" | "agents" | "memory" | "params" | "general";

export function Settings() {
  const [tab, setTab] = useState<Tab>("models");
  const [draft, setDraft] = useState<Draft | null>(null);
  const [hasKey, setHasKey] = useState<Record<string, boolean>>({});
  const [reasoningIgnored, setReasoningIgnored] = useState<Record<string, boolean>>({});
  const [keys, setKeys] = useState<Record<string, string>>({});
  const [skins, setSkins] = useState<SkinEntry[]>([]);
  const [picturesDir, setPicturesDir] = useState("");
  const [dirty, setDirtyState] = useState(false);
  const dirtyRef = useRef(false);
  const setDirty = (d: boolean) => {
    dirtyRef.current = d;
    setDirtyState(d);
  };
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null);

  const load = async () => {
    const s = await api.settingsGet();
    setDraft({ providers: s.providers.map(({ hasKey: _, reasoningIgnored: __, ...p }) => p), roles: s.roles, personas: s.personas, activePersona: s.activePersona, chat: s.chat, tools: s.tools, image: s.image, voice: s.voice, agents: s.agents });
    setPicturesDir(s.picturesDir);
    setHasKey(Object.fromEntries(s.providers.map((p) => [p.id, p.hasKey])));
    setReasoningIgnored(Object.fromEntries(s.providers.map((p) => [p.id, p.reasoningIgnored])));
    setSkins(s.skins);
    setKeys({});
    setDirty(false);
  };

  useEffect(() => {
    void load();
    // 别处（托盘、聊天窗口）改了当前人格时同步，但不覆盖未保存的修改
    const un = listen("settings-changed", () => {
      if (!dirtyRef.current) void load();
    });
    return () => void un.then((f) => f());
  }, []);

  if (!draft) return <div className="settings" />;

  const update = (fn: (d: Draft) => Draft) => {
    setDraft((d) => (d ? fn(d) : d));
    setDirty(true);
    setStatus(null);
  };

  const save = async () => {
    try {
      await api.settingsSave(draft);
      for (const [id, key] of Object.entries(keys)) {
        if (key.trim()) await api.providerSetKey(id, key);
      }
      await load();
      setStatus({ ok: true, text: "已保存" });
    } catch (e) {
      setStatus({ ok: false, text: errorText(e) });
    }
  };

  return (
    <div className="settings">
      <nav>
        {(
          [
            ["models", "模型"],
            ["personas", "人格"],
            ["tools", "工具与生图"],
            ["voice", "语音"],
            ["agents", "Agent 接入"],
            ["memory", "记忆"],
            ["params", "对话参数"],
            ["general", "常规"],
          ] as [Tab, string][]
        ).map(([t, label]) => (
          <button key={t} className={tab === t ? "active" : ""} onClick={() => setTab(t)}>
            {label}
          </button>
        ))}
      </nav>
      <main>
        {tab === "models" && <ModelsTab draft={draft} update={update} keys={keys} setKeys={(k) => { setKeys(k); setDirty(true); }} hasKey={hasKey} reasoningIgnored={reasoningIgnored} />}
        {tab === "personas" && <PersonasTab draft={draft} update={update} skins={skins} />}
        {tab === "tools" && <ToolsTab draft={draft} update={update} picturesDir={picturesDir} />}
        {tab === "voice" && <VoiceTab draft={draft} update={update} />}
        {tab === "agents" && <AgentsTab draft={draft} update={update} />}
        {tab === "memory" && <MemoryTab />}
        {tab === "params" && <ParamsTab draft={draft} update={update} />}
        {tab === "general" && <GeneralTab />}
      </main>
      {tab !== "memory" && (
        <footer>
          {status && <span className={status.ok ? "ok" : "error"}>{status.text}</span>}
          {dirty && !status && <span className="muted">有未保存的修改</span>}
          <button onClick={() => void load()} disabled={!dirty}>
            撤销修改
          </button>
          <button className="primary" onClick={() => void save()} disabled={!dirty}>
            保存
          </button>
        </footer>
      )}
    </div>
  );
}

type TabProps = { draft: Draft; update: (fn: (d: Draft) => Draft) => void };

function ProviderSelect({ value, onChange, providers, emptyLabel }: { value: string | null; onChange: (v: string | null) => void; providers: Provider[]; emptyLabel: string }) {
  return (
    <select value={value ?? ""} onChange={(e) => onChange(e.target.value || null)}>
      <option value="">{emptyLabel}</option>
      {providers.map((p) => (
        <option key={p.id} value={p.id}>
          {p.name || p.model || p.id}
        </option>
      ))}
    </select>
  );
}

function ModelsTab({
  draft,
  update,
  keys,
  setKeys,
  hasKey,
  reasoningIgnored,
}: TabProps & { keys: Record<string, string>; setKeys: (k: Record<string, string>) => void; hasKey: Record<string, boolean>; reasoningIgnored: Record<string, boolean> }) {
  const [tests, setTests] = useState<Record<string, { ok: boolean; text: string } | "running">>({});

  const setProvider = (id: string, patch: Partial<Provider>) =>
    update((d) => ({ ...d, providers: d.providers.map((p) => (p.id === id ? { ...p, ...patch } : p)) }));

  const add = (presetIdx: number) => {
    const pr = PRESETS[presetIdx];
    const p: Provider = { id: newId(), name: pr.label, base_url: pr.base_url, model: pr.model, supports_vision: !!pr.vision, supports_tools: true, reasoning_effort: "", reasoning_style: pr.style };
    update((d) => ({ ...d, providers: [...d.providers, p], roles: d.roles.chat ? d.roles : { ...d.roles, chat: p.id } }));
  };

  const remove = (id: string) =>
    update((d) => {
      const clear = (v: string | null) => (v === id ? null : v);
      return {
        ...d,
        providers: d.providers.filter((p) => p.id !== id),
        roles: { chat: clear(d.roles.chat), vision: clear(d.roles.vision), aux: clear(d.roles.aux), image: clear(d.roles.image) },
        personas: d.personas.map((p) => ({ ...p, provider: clear(p.provider) })),
      };
    });

  const test = async (p: Provider) => {
    setTests((t) => ({ ...t, [p.id]: "running" }));
    try {
      // 只用来生图的模型不支持对话接口，测试时不能发聊天消息
      const r = draft.roles;
      const imageOnly = r.image === p.id && ![r.chat, r.vision, r.aux].includes(p.id) && !draft.personas.some((x) => x.provider === p.id);
      const reply = await api.providerTest(p, keys[p.id], imageOnly);
      setTests((t) => ({ ...t, [p.id]: { ok: true, text: reply } }));
    } catch (e) {
      setTests((t) => ({ ...t, [p.id]: { ok: false, text: errorText(e) } }));
    }
  };

  return (
    <div className="tab">
      <section>
        <h3>模型用途</h3>
        <div className="grid2">
          <label>主对话</label>
          <ProviderSelect value={draft.roles.chat} providers={draft.providers} emptyLabel="（未选择）" onChange={(v) => update((d) => ({ ...d, roles: { ...d.roles, chat: v } }))} />
          <label>视觉（主模型不能看图时用它转述图片）</label>
          <ProviderSelect value={draft.roles.vision} providers={draft.providers} emptyLabel="（不使用）" onChange={(v) => update((d) => ({ ...d, roles: { ...d.roles, vision: v } }))} />
          <label>辅助（记忆提取、总结；可选更便宜的模型）</label>
          <ProviderSelect value={draft.roles.aux} providers={draft.providers} emptyLabel="（与主对话相同）" onChange={(v) => update((d) => ({ ...d, roles: { ...d.roles, aux: v } }))} />
          <label>生图（OpenAI 兼容的 /images/generations）</label>
          <ProviderSelect value={draft.roles.image} providers={draft.providers} emptyLabel="（不使用）" onChange={(v) => update((d) => ({ ...d, roles: { ...d.roles, image: v } }))} />
        </div>
      </section>

      <section>
        <div className="section-head">
          <h3>模型列表</h3>
          <select value="" onChange={(e) => e.target.value !== "" && add(Number(e.target.value))}>
            <option value="">＋ 添加模型…</option>
            {PRESETS.map((p, i) => (
              <option key={p.label} value={i}>
                {p.label}
              </option>
            ))}
          </select>
        </div>
        {draft.providers.length === 0 && <p className="muted">还没有模型。从右上角选一个厂商添加，填好 API Key 后保存。</p>}
        <p className="muted small">预设里的模型名只是示例，请以厂商文档为准。API Key 保存在 Windows 凭据管理器里，不会写进配置文件。</p>
        {draft.providers.map((p) => {
          const t = tests[p.id];
          return (
            <div className="card" key={p.id}>
              <div className="grid2">
                <label>名称</label>
                <input value={p.name} onChange={(e) => setProvider(p.id, { name: e.target.value })} />
                <label>接口地址</label>
                <input value={p.base_url} onChange={(e) => setProvider(p.id, { base_url: e.target.value })} placeholder="https://…/v1" />
                <label>模型名</label>
                <input value={p.model} onChange={(e) => setProvider(p.id, { model: e.target.value })} />
                <label>API Key</label>
                <input
                  type="password"
                  value={keys[p.id] ?? ""}
                  placeholder={hasKey[p.id] ? "已保存（留空则不修改）" : "未设置"}
                  onChange={(e) => setKeys({ ...keys, [p.id]: e.target.value })}
                />
                <label>推理强度</label>
                <div className="reasoning-row">
                  <select value={p.reasoning_effort} onChange={(e) => setProvider(p.id, { reasoning_effort: e.target.value })}>
                    {EFFORTS.map(([v, l]) => (
                      <option key={v} value={v}>
                        {l}
                      </option>
                    ))}
                  </select>
                  <select
                    value={p.reasoning_style}
                    disabled={!p.reasoning_effort}
                    title="不同服务传推理参数的写法不同"
                    onChange={(e) => setProvider(p.id, { reasoning_style: e.target.value })}
                  >
                    {STYLES.map(([v, l]) => (
                      <option key={v} value={v}>
                        {l}
                      </option>
                    ))}
                  </select>
                </div>
                <span />
                <div className="small muted">
                  {reasoningIgnored[p.id] && p.reasoning_effort ? (
                    <span className="warn">这个模型没有接受推理参数，已自动忽略，聊天不受影响。可以换一种参数格式试试。</span>
                  ) : (
                    p.reasoning_style === "deepseek"
                      ? "DeepSeek 新模型默认开启思考（强度相当于“中”）。低 / 中 / 高 对应 low / high / max，选“关闭思考”回答更快。只用于主对话。"
                      : "只对支持推理的模型有效，只用于主对话。服务不接受时会自动忽略，不影响聊天。"
                  )}
                </div>
                <label>能力</label>
                <div className="checks">
                  <label>
                    <input type="checkbox" checked={p.supports_vision} onChange={(e) => setProvider(p.id, { supports_vision: e.target.checked })} /> 能看图
                  </label>
                  <label>
                    <input type="checkbox" checked={p.supports_tools} onChange={(e) => setProvider(p.id, { supports_tools: e.target.checked })} /> 支持工具调用
                  </label>
                </div>
              </div>
              <div className="card-actions">
                {t && t !== "running" && <span className={t.ok ? "ok" : "error"}>{t.text}</span>}
                <button onClick={() => void test(p)} disabled={t === "running"}>
                  {t === "running" ? "测试中…" : "测试连接"}
                </button>
                <button className="danger" onClick={() => remove(p.id)}>
                  删除
                </button>
              </div>
            </div>
          );
        })}
      </section>
    </div>
  );
}

function PersonasTab({ draft, update, skins }: TabProps & { skins: SkinEntry[] }) {
  const [sel, setSel] = useState(draft.activePersona);
  const p = draft.personas.find((x) => x.id === sel) ?? draft.personas[0];
  const set = (patch: Partial<Persona>) =>
    update((d) => ({ ...d, personas: d.personas.map((x) => (x.id === p.id ? { ...x, ...patch } : x)) }));

  const add = () => {
    const np: Persona = { id: newId(), name: "新人格", system_prompt: "", begin_dialogs: [], provider: null, temperature: null, skin: null };
    update((d) => ({ ...d, personas: [...d.personas, np] }));
    setSel(np.id);
  };
  const remove = () => {
    if (draft.personas.length <= 1) return;
    update((d) => {
      const personas = d.personas.filter((x) => x.id !== p.id);
      return { ...d, personas, activePersona: d.activePersona === p.id ? personas[0].id : d.activePersona };
    });
    setSel(draft.personas.find((x) => x.id !== p.id)!.id);
  };
  const clearChat = async () => {
    if (confirm(`清空「${p.name}」的全部对话和摘要？长期记忆不受影响。此操作不能撤销。`)) await api.chatClear(p.id);
  };

  return (
    <div className="tab personas">
      <aside>
        {draft.personas.map((x) => (
          <button key={x.id} className={x.id === p.id ? "active" : ""} onClick={() => setSel(x.id)}>
            {x.name || "（未命名）"}
            {x.id === draft.activePersona && <span className="badge">当前</span>}
          </button>
        ))}
        <button onClick={add}>＋ 新建人格</button>
      </aside>
      <div className="persona-edit">
        <div className="grid2">
          <label>名称</label>
          <input value={p.name} onChange={(e) => set({ name: e.target.value })} />
          <label>主对话模型</label>
          <ProviderSelect value={p.provider} providers={draft.providers} emptyLabel="（跟随「模型用途」）" onChange={(v) => set({ provider: v })} />
          <label>温度</label>
          <input
            type="number"
            step="0.1"
            min="0"
            max="2"
            placeholder="默认"
            value={p.temperature ?? ""}
            onChange={(e) => set({ temperature: e.target.value === "" ? null : Number(e.target.value) })}
          />
          <label>绑定皮肤</label>
          <select value={p.skin ?? ""} onChange={(e) => set({ skin: e.target.value || null })}>
            <option value="">（不绑定）</option>
            {skins.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </select>
        </div>
        <label>系统提示词（人设）</label>
        <textarea rows={8} value={p.system_prompt} onChange={(e) => set({ system_prompt: e.target.value })} />
        <div className="section-head">
          <label>预设对话（用来定口吻，会放在历史消息之前）</label>
          <button onClick={() => set({ begin_dialogs: [...p.begin_dialogs, { user: "", assistant: "" }] })}>＋ 添加一组</button>
        </div>
        {p.begin_dialogs.map((d, i) => (
          <div className="dialog-pair" key={i}>
            <input placeholder="用户说" value={d.user} onChange={(e) => set({ begin_dialogs: p.begin_dialogs.map((x, j) => (j === i ? { ...x, user: e.target.value } : x)) })} />
            <input placeholder="它回答" value={d.assistant} onChange={(e) => set({ begin_dialogs: p.begin_dialogs.map((x, j) => (j === i ? { ...x, assistant: e.target.value } : x)) })} />
            <button className="icon" onClick={() => set({ begin_dialogs: p.begin_dialogs.filter((_, j) => j !== i) })}>
              ×
            </button>
          </div>
        ))}
        <div className="card-actions">
          <button onClick={() => update((d) => ({ ...d, activePersona: p.id }))} disabled={draft.activePersona === p.id}>
            设为当前人格
          </button>
          <button className="danger" onClick={() => void clearChat()}>
            清空它的对话
          </button>
          <button className="danger" onClick={remove} disabled={draft.personas.length <= 1}>
            删除人格
          </button>
        </div>
      </div>
    </div>
  );
}

function ToolsTab({ draft, update, picturesDir }: TabProps & { picturesDir: string }) {
  const t = draft.tools;
  const setTools = (patch: Partial<ToolParams>) => update((d) => ({ ...d, tools: { ...d.tools, ...patch } }));
  const addDir = async () => {
    const picked = await openDialog({ directory: true, multiple: true, title: "选择允许桌宠读写的文件夹" });
    const dirs = picked ? (Array.isArray(picked) ? picked : [picked]) : [];
    const fresh = dirs.filter((d) => !t.allowed_dirs.includes(d));
    if (fresh.length) setTools({ allowed_dirs: [...t.allowed_dirs, ...fresh] });
  };
  const imageProvider = draft.providers.find((p) => p.id === draft.roles.image);

  return (
    <div className="tab">
      <section>
        <h3>文件工具（读文件、写文件、修改文件）</h3>
        <p className="muted small">
          模型支持工具调用时，桌宠可以读写下面这些文件夹里的文件（不能执行命令）。读取不需要确认；写入和修改会弹出确认框，显示改动内容。读到的文件内容会发给云端模型。
        </p>
        <div className="checks" style={{ margin: "6px 0 10px" }}>
          <label>
            <input type="checkbox" checked={t.enabled} onChange={(e) => setTools({ enabled: e.target.checked })} /> 启用文件工具和生图工具
          </label>
        </div>
        <div className="section-head">
          <label>允许访问的文件夹（包含子文件夹）</label>
          <button onClick={() => void addDir()}>＋ 添加文件夹</button>
        </div>
        {t.allowed_dirs.length === 0 && <p className="muted small">还没有添加。不添加的话，文件工具不会提供给模型。</p>}
        {t.allowed_dirs.map((d) => (
          <div className="memory" key={d} style={{ gridTemplateColumns: "1fr auto" }}>
            <span>📁 {d}</span>
            <button className="icon" title="移除" onClick={() => setTools({ allowed_dirs: t.allowed_dirs.filter((x) => x !== d) })}>
              ×
            </button>
          </div>
        ))}
        <div className="grid2 params" style={{ marginTop: 10 }}>
          <label>
            每轮最多调用工具次数
            <div className="small muted">防止模型陷入循环</div>
          </label>
          <input type="number" min={1} max={100} value={t.max_calls} onChange={(e) => setTools({ max_calls: Math.max(1, Math.min(100, Number(e.target.value) || 1)) })} />
        </div>
      </section>

      <section>
        <h3>生图</h3>
        <p className="muted small">
          {imageProvider ? `当前生图模型：${imageProvider.name || imageProvider.model}（在"模型"页的模型用途里修改）。` : "还没有选择生图模型，请到\"模型\"页的模型用途里选择。"}
          聊天里输入 <code>/draw 描述</code> 直接画图；对模型说"画一张……"时它也会调用生图。图片会另存到 {picturesDir}。
        </p>
        <div className="grid2 params">
          <label>
            每天最多生成几张
            <div className="small muted">按次计费，防止花超</div>
          </label>
          <input type="number" min={0} max={1000} value={draft.image.daily_limit} onChange={(e) => update((d) => ({ ...d, image: { ...d.image, daily_limit: Math.max(0, Number(e.target.value) || 0) } }))} />
          <label>
            图片尺寸
            <div className="small muted">传给接口的 size，不同服务支持的值不同</div>
          </label>
          <input value={draft.image.size} onChange={(e) => update((d) => ({ ...d, image: { ...d.image, size: e.target.value } }))} placeholder="1024x1024" />
        </div>
      </section>
    </div>
  );
}

function fmtMB(n: number) {
  return `${(n / 1048576).toFixed(n > 10 * 1048576 ? 0 : 1)}MB`;
}

function VoiceTab({ draft, update }: TabProps) {
  const v = draft.voice;
  const setVoice = (patch: Partial<VoiceParams>) => update((d) => ({ ...d, voice: { ...d.voice, ...patch } }));
  const [status, setStatus] = useState<VoiceStatus | null>(null);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [dlError, setDlError] = useState<string | null>(null);
  const refresh = () => void api.voiceStatus().then(setStatus);

  useEffect(() => {
    refresh();
    const subs = Promise.all([
      listen<DownloadProgress>("voice-download", (e) => setProgress(e.payload)),
      listen<string | null>("voice-download-done", (e) => {
        setProgress(null);
        setDlError(e.payload);
        refresh();
      }),
      listen("settings-changed", refresh),
    ]);
    return () => void subs.then((fns) => fns.forEach((f) => f()));
  }, []);

  const download = async () => {
    setDlError(null);
    setStatus((s) => (s ? { ...s, downloading: true } : s));
    try {
      await api.voiceDownload();
    } catch (e) {
      setDlError(errorText(e));
    }
    refresh();
  };
  const pickDir = async () => {
    const d = await openDialog({ directory: true, title: "选择 SenseVoice 模型文件夹（里面有 model.int8.onnx 和 tokens.txt）" });
    if (typeof d === "string") setVoice({ model_dir: d });
  };
  const pickRuntimeDir = async () => {
    const d = await openDialog({ directory: true, title: "选择语音运行库文件夹（里面有 sherpa-onnx-c-api.dll 和 onnxruntime.dll）" });
    if (typeof d === "string") setVoice({ runtime_dir: d });
  };
  const ready = !!status && status.runtimeReady && status.modelReady;
  const needSize = status ? [!status.runtimeReady && "运行库约 7.5MB", !status.modelReady && "模型约 240MB"].filter(Boolean).join(" + ") : "";
  const restartNeeded = !!status?.runtimeLoadedFrom && !!status.runtimeDir && status.runtimeLoadedFrom !== status.runtimeDir;

  return (
    <div className="tab">
      <section>
        <h3>语音输入</h3>
        <div className="checks">
          <label>
            <input type="checkbox" checked={v.enabled} onChange={(e) => setVoice({ enabled: e.target.checked })} /> 开启语音输入（本地识别，录音不上传）
          </label>
        </div>
        <p className="muted small">
          可选功能，默认关闭。用 SenseVoice（中、英、日、韩、粤语）离线识别，配合 Silero VAD 去掉静音。需要语音运行库（sherpa-onnx，约 7.5MB）和识别模型（约
          240MB）：可以一键下载（只需一次，支持断点续传），也可以在下面指定已有的文件夹。关着时不加载这些组件，也不占用快捷键。
        </p>
        {v.enabled && status && !ready && <div className="warn">已开启，但还缺{!status.runtimeReady ? "语音运行库" : ""}{!status.runtimeReady && !status.modelReady ? "和" : ""}{!status.modelReady ? "识别模型" : ""}，下载或指定之后才能用。</div>}
      </section>

      <section>
        <h3>语音组件</h3>
        {status && (
          <div className="card">
            {status.runtimeReady ? (
              <div className="ok">✓ 运行库已就绪 <span className="muted small">{status.runtimeDir}</span></div>
            ) : (
              <div className="warn">还没有语音运行库 <span className="muted small">（下载到 {status.runtimeDownloadDir}）</span></div>
            )}
            {restartNeeded && <div className="muted small">本次运行已从 {status.runtimeLoadedFrom} 加载运行库，换的文件夹重启后生效。</div>}
            {status.modelReady ? (
              <div className="ok">✓ 识别模型已就绪</div>
            ) : (
              <div className="warn">还没有识别模型 <span className="muted small">（下载到 {status.downloadDir}）</span></div>
            )}
            {progress && (
              <div className="dl-progress">
                <div className="small">
                  正在下载 {progress.file}：{fmtMB(progress.downloaded)} / {fmtMB(progress.total)}（来自 {progress.source}）
                </div>
                <div className="bar">
                  <span style={{ width: `${progress.total ? (progress.downloaded / progress.total) * 100 : 0}%` }} />
                </div>
              </div>
            )}
            {dlError && dlError !== "已取消" && <div className="error">{dlError}</div>}
            <div className="card-actions">
              {status.downloading || progress ? (
                <button onClick={() => void api.voiceDownloadCancel()}>取消下载</button>
              ) : (
                !ready && (
                  <button className="primary" onClick={() => void download()}>
                    下载语音组件（{needSize}）
                  </button>
                )
              )}
            </div>
          </div>
        )}
        <div className="grid2 params" style={{ marginTop: 10 }}>
          <label>
            自己的运行库文件夹（可选）
            <div className="small muted">里面要有 sherpa-onnx v2.0.0 的 sherpa-onnx-c-api.dll 和 onnxruntime.dll；留空用下载的</div>
          </label>
          <div className="reasoning-row" style={{ gridTemplateColumns: "1fr auto auto" }}>
            <input value={v.runtime_dir ?? ""} placeholder="（使用下载的运行库）" onChange={(e) => setVoice({ runtime_dir: e.target.value || null })} />
            <button onClick={() => void pickRuntimeDir()}>选择…</button>
            <button onClick={() => setVoice({ runtime_dir: null })} disabled={!v.runtime_dir}>
              清除
            </button>
          </div>
          <label>
            自己的模型文件夹（可选）
            <div className="small muted">已经有 SenseVoice 模型时可以直接指定，留空用自动下载的</div>
          </label>
          <div className="reasoning-row" style={{ gridTemplateColumns: "1fr auto auto" }}>
            <input value={v.model_dir ?? ""} placeholder="（使用自动下载的模型）" onChange={(e) => setVoice({ model_dir: e.target.value || null })} />
            <button onClick={() => void pickDir()}>选择…</button>
            <button onClick={() => setVoice({ model_dir: null })} disabled={!v.model_dir}>
              清除
            </button>
          </div>
        </div>
      </section>

      <section>
        <h3>使用</h3>
        <div className="grid2 params">
          <label>
            按住说话的快捷键
            <div className="small muted">全局有效，按住说、松开识别；留空不用快捷键。例如 Ctrl+Shift+Space、Alt+Q、F9</div>
          </label>
          <div>
            <input value={v.hotkey} onChange={(e) => setVoice({ hotkey: e.target.value })} placeholder="Ctrl+Shift+Space" />
            {status?.hotkeyError && <div className="error">{status.hotkeyError}</div>}
          </div>
          <label>识别之后</label>
          <div className="checks">
            <label>
              <input type="radio" checked={v.auto_send} onChange={() => setVoice({ auto_send: true })} /> 直接发送
            </label>
            <label>
              <input type="radio" checked={!v.auto_send} onChange={() => setVoice({ auto_send: false })} /> 填进输入框，确认后再发
            </label>
          </div>
          <label>麦克风</label>
          <select value={v.device ?? ""} onChange={(e) => setVoice({ device: e.target.value || null })}>
            <option value="">（系统默认）</option>
            {status?.devices.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        </div>
        <p className="muted small">也可以在气泡里按住 🎤 说话。录音时按 Esc 取消。</p>
      </section>
    </div>
  );
}

function AgentsTab({ draft, update }: TabProps) {
  const a = draft.agents;
  const setA = (patch: Partial<AgentsParams>) => update((d) => ({ ...d, agents: { ...d.agents, ...patch } }));
  const [ov, setOv] = useState<AgentsOverview | null>(null);
  const [busy, setBusy] = useState<AgentId | "repair" | null>(null);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const sessions = useSessions();
  const refresh = () => void api.agentsOverview().then(setOv);

  useEffect(refresh, []);

  const toggle = async (agent: AgentId, on: boolean) => {
    setBusy(agent);
    setMsg(null);
    try {
      await api.agentsSet(agent, on);
      setMsg({ ok: true, text: on ? "已接入" : "已移除，原来的配置保持不变" });
    } catch (e) {
      setMsg({ ok: false, text: errorText(e) });
    }
    setBusy(null);
    refresh();
  };
  const repair = async () => {
    setBusy("repair");
    setMsg(null);
    try {
      await api.agentsRepair();
      setMsg({ ok: true, text: "已修复" });
    } catch (e) {
      setMsg({ ok: false, text: errorText(e) });
    }
    setBusy(null);
    refresh();
  };
  const needRepair = ov?.agents.some((x) => x.enabled && x.state !== "ok");

  return (
    <div className="tab">
      <section>
        <h3>接入的工具</h3>
        <p className="muted small">
          开启后，桌宠会在对应工具的配置里写入 hooks（写之前先备份，关闭时只移除桌宠自己加的部分）。只汇报状态、不接管授权，桌宠没运行时这些工具照常工作。
          {ov?.port ? ` 本地事件服务：127.0.0.1:${ov.port}` : ""}
        </p>
        {ov?.agents.map((x) => (
          <div className="card agent-row" key={x.agent}>
            <div className="agent-head">
              <strong>{x.name}</strong>
              <span className={`badge-state ${!x.enabled ? "off" : x.state === "ok" ? "ok" : "outdated"}`}>
                {!x.detected && !x.enabled ? "本机没检测到" : !x.enabled ? "未开启" : x.state === "ok" ? "已接入" : "需要修复"}
              </span>
              <span style={{ flex: 1 }} />
              <button className={x.enabled ? "" : "primary"} disabled={busy !== null} onClick={() => void toggle(x.agent, !x.enabled)}>
                {busy === x.agent ? "处理中…" : x.enabled ? "关闭" : "开启"}
              </button>
            </div>
            {x.note && <div className="muted small">{x.note}</div>}
          </div>
        ))}
        {msg && <div className={msg.ok ? "ok" : "error"} style={{ marginTop: 8 }}>{msg.text}</div>}
        {needRepair && (
          <div className="card-actions">
            <span className="warn">有接入没装好，或者配置过期了（端口、令牌、程序位置变了）。</span>
            <button className="primary" disabled={busy !== null} onClick={() => void repair()}>
              {busy === "repair" ? "修复中…" : "修复"}
            </button>
          </div>
        )}
      </section>

      <section>
        <h3>汇报方式</h3>
        <div className="grid2 params">
          <label>
            完成时用人格口吻总结
            <div className="small muted">把它最后的回复交给辅助模型，说成一两句话（内容会发给云端模型）；关掉则只说"干完活啦"</div>
          </label>
          <div className="checks">
            <label>
              <input type="checkbox" checked={a.summarize} onChange={(e) => setA({ summarize: e.target.checked })} /> 开启
            </label>
          </div>
          <label>
            状态标签
            <div className="small muted">在桌宠身边显示"Claude Code · 思考中"这样的小标签</div>
          </label>
          <div className="checks">
            <label>
              <input type="checkbox" checked={a.status_tag} onChange={(e) => setA({ status_tag: e.target.checked })} /> 显示
            </label>
          </div>
          <label>Windows 通知</label>
          <div className="checks">
            <label>
              <input type="checkbox" checked={a.notify} onChange={(e) => setA({ notify: e.target.checked })} /> 完成、等你授权、出错时发通知
            </label>
          </div>
          <label>
            至少运行多久才汇报完成（秒）
            <div className="small muted">对话式使用时每句回复都算一轮，太短的只播动画、不弹气泡</div>
          </label>
          <input type="number" min={0} max={3600} value={a.min_report_seconds} onChange={(e) => setA({ min_report_seconds: Math.max(0, Number(e.target.value) || 0) })} />
          <label>
            本地端口
            <div className="small muted">改了要重启桌宠，再点「修复」</div>
          </label>
          <input type="number" min={1024} max={65535} value={a.port} onChange={(e) => setA({ port: Number(e.target.value) || 47821 })} />
        </div>
      </section>

      <section>
        <h3>当前会话</h3>
        <SessionList list={sessions} />
      </section>
    </div>
  );
}

function MemoryTab() {
  const [items, setItems] = useState<Memory[]>([]);
  const [edit, setEdit] = useState<{ id: number | null; kind: string; content: string } | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const load = () => void api.memoryList().then(setItems);

  useEffect(() => {
    load();
    const un = listen("memories-changed", load);
    return () => void un.then((f) => f());
  }, []);

  const save = async () => {
    if (!edit) return;
    try {
      await api.memorySave(edit.id, edit.kind, edit.content);
      setEdit(null);
      setErr(null);
      load();
    } catch (e) {
      setErr(errorText(e));
    }
  };

  const groups: [string, string][] = [
    ["core", "核心记忆（身份、喜好、习惯、长期计划）"],
    ["event", "事件记忆（有时间意义的具体事情）"],
  ];

  return (
    <div className="tab">
      <div className="section-head">
        <p className="muted small">所有人格共享这些记忆。它们由辅助模型每隔几轮对话自动提取，每次对话都会全部放进提示词。</p>
        <button onClick={() => setEdit({ id: null, kind: "core", content: "" })}>＋ 手动添加</button>
      </div>
      {err && <div className="error">{err}</div>}
      {edit && edit.id === null && <MemoryEditor edit={edit} setEdit={setEdit} save={save} />}
      {groups.map(([kind, title]) => (
        <section key={kind}>
          <h3>{title}</h3>
          {items.filter((m) => m.kind === kind).length === 0 && <p className="muted small">暂无</p>}
          {items
            .filter((m) => m.kind === kind)
            .map((m) =>
              edit?.id === m.id ? (
                <MemoryEditor key={m.id} edit={edit} setEdit={setEdit} save={save} />
              ) : (
                <div className="memory" key={m.id}>
                  <span>{m.content}</span>
                  <span className="muted small">{new Date(m.updatedAt * 1000).toLocaleDateString()}</span>
                  <button className="icon" title="编辑" onClick={() => setEdit({ id: m.id, kind: m.kind, content: m.content })}>
                    ✎
                  </button>
                  <button className="icon" title="删除" onClick={() => void api.memoryDelete(m.id).then(load)}>
                    ×
                  </button>
                </div>
              ),
            )}
        </section>
      ))}
    </div>
  );
}

function MemoryEditor({ edit, setEdit, save }: { edit: { id: number | null; kind: string; content: string }; setEdit: (e: typeof edit | null) => void; save: () => void }) {
  return (
    <div className="memory editing">
      <select value={edit.kind} onChange={(e) => setEdit({ ...edit, kind: e.target.value })}>
        <option value="core">核心</option>
        <option value="event">事件</option>
      </select>
      <input autoFocus value={edit.content} onChange={(e) => setEdit({ ...edit, content: e.target.value })} onKeyDown={(e) => e.key === "Enter" && save()} />
      <button className="primary" onClick={save}>
        保存
      </button>
      <button onClick={() => setEdit(null)}>取消</button>
    </div>
  );
}

/** 常规：开机自启动（直接写注册表，立即生效，不用点保存） */
function GeneralTab() {
  const [on, setOn] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => void api.autostartGet().then(setOn), []);

  const toggle = async (v: boolean) => {
    setBusy(true);
    setErr(null);
    try {
      await api.autostartSet(v);
    } catch (e) {
      setErr(errorText(e));
    }
    setOn(await api.autostartGet());
    setBusy(false);
  };

  return (
    <div className="tab">
      <section>
        <h3>启动</h3>
        <div className="checks">
          <label>
            <input type="checkbox" checked={!!on} disabled={on === null || busy} onChange={(e) => void toggle(e.target.checked)} /> 开机时自动启动 Perch
          </label>
        </div>
        <p className="muted small">默认关闭。只对当前 Windows 用户生效，改了立即生效；也可以在任务管理器的「启动应用」里管理。</p>
        {err && <div className="error">{err}</div>}
      </section>
    </div>
  );
}

function ParamsTab({ draft, update }: TabProps) {
  const field = (key: keyof ChatParams, label: string, hint: string, min: number, max: number) => (
    <>
      <label>
        {label}
        <div className="small muted">{hint}</div>
      </label>
      <input
        type="number"
        min={min}
        max={max}
        value={draft.chat[key]}
        onChange={(e) => update((d) => ({ ...d, chat: { ...d.chat, [key]: Math.max(min, Math.min(max, Number(e.target.value) || min)) } }))}
      />
    </>
  );
  return (
    <div className="tab">
      <section className="grid2 params">
        {field("context_tokens", "历史消息预算（token）", "超过后最早的一段会被总结成摘要", 2000, 200000)}
        {field("memory_extract_every", "记忆提取间隔（轮）", "每隔几轮用户消息提取一次长期记忆", 1, 50)}
        {field("memory_limit", "记忆条数上限", "超过后让辅助模型合并去重", 10, 500)}
        {field("file_chars", "单个文件最多注入字数", "超出部分截断，并告诉模型被截断了", 1000, 500000)}
        {field("bubble_chars", "气泡最多显示字数", "超出后提示展开看全文", 40, 2000)}
      </section>
    </div>
  );
}
