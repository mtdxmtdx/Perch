import { useEffect, useLayoutEffect, useRef, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { api, type AttachmentMeta, type Message, type Persona, type ToolStatus } from "../shared/api";
import { useChat } from "../shared/useChat";
import { fromPaste, pickFiles, useAttachments } from "../shared/attach";
import { ToolLines } from "../shared/ToolLines";
import { ApprovalDialog } from "./ApprovalDialog";
import { useVoice } from "../shared/useVoice";
import { SessionList, useSessions } from "../shared/Sessions";
import { listen } from "@tauri-apps/api/event";
import "./chat.css";

/** 点图片放大看 */
let showLightbox: (url: string, caption: string) => void = () => {};

function Lightbox() {
  const [img, setImg] = useState<{ url: string; caption: string } | null>(null);
  showLightbox = (url, caption) => setImg({ url, caption });
  if (!img) return null;
  return (
    <div className="lightbox" onClick={() => setImg(null)} title="点击关闭">
      <img src={img.url} alt={img.caption} />
      <div className="caption">{img.caption}</div>
    </div>
  );
}

function ImageThumb({ file, name, caption }: { file: string; name: string; caption?: string }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let revoked: string | null = null;
    api
      .attachmentImage(file)
      .then((buf) => {
        revoked = URL.createObjectURL(new Blob([buf]));
        setUrl(revoked);
      })
      .catch(() => setUrl(null));
    return () => {
      if (revoked) URL.revokeObjectURL(revoked);
    };
  }, [file]);
  return url ? (
    <img className="thumb" src={url} alt={name} title={caption ?? name} onClick={() => showLightbox(url, caption ?? name)} />
  ) : (
    <span className="chip">
      <span>🖼 {name}</span>
    </span>
  );
}

function Attachments({ items }: { items: AttachmentMeta[] }) {
  const files = items.filter((a) => a.kind !== "tool");
  if (!files.length) return null;
  return (
    <div className="msg-atts">
      {files.map((a, i) =>
        a.kind === "image" && a.file ? (
          // 生成的图片 note 里是另存路径
          <ImageThumb key={i} file={a.file} name={a.name} caption={a.note ? `${a.name}\n已保存到 ${a.note}` : undefined} />
        ) : (
          <span key={i} className="chip" title={a.note ?? a.name}>
            <span>📄 {a.name}{a.note ? "（已截断）" : ""}</span>
          </span>
        ),
      )}
    </div>
  );
}

function Bubble({ m }: { m: Message }) {
  const toolLines = m.attachments
    .filter((a) => a.kind === "tool")
    .map((a, i) => ({ id: String(i), title: a.name, status: (a.note ?? "ok") as ToolStatus, detail: a.desc }));
  return (
    <div className={`msg ${m.role}`}>
      <ToolLines lines={toolLines} />
      {/* 用户消息：附件在上；回复：先文字后图片 */}
      {m.role === "user" && <Attachments items={m.attachments} />}
      {m.display &&
        (m.role === "assistant" ? (
          <div className="md">
            <Markdown remarkPlugins={[remarkGfm]}>{m.display}</Markdown>
          </div>
        ) : (
          <div className="plain">{m.display}</div>
        ))}
      {m.role === "assistant" && <Attachments items={m.attachments} />}
    </div>
  );
}

export function ChatWindow() {
  const chat = useChat(50);
  const atts = useAttachments();
  const [text, setText] = useState("");
  const voice = useVoice({ text, setText });
  const [personas, setPersonas] = useState<Persona[]>([]);
  const sessions = useSessions();
  const [showSessions, setShowSessions] = useState(false);
  const active = sessions.filter((x) => x.running || x.status === "attention").length;
  const listRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  // 最近一次用户主动滚动（滚轮、拖滚动条、按键）的时间。
  // 程序滚到底时也会触发 scroll 事件，而且晚一帧才到，那时内容可能又变高了，不能据此认为用户往上翻了。
  const userScrollAt = useRef(0);
  const markUserScroll = () => {
    userScrollAt.current = Date.now();
  };

  useEffect(() => {
    const load = () => void api.settingsGet().then((s) => setPersonas(s.personas));
    load();
    const un = listen("settings-changed", load);
    return () => void un.then((f) => f());
  }, []);

  // 内容变高时（新消息、流式输出、图片加载完、窗口从隐藏变可见）保持在底部；用户往上翻着看历史时不打扰
  useLayoutEffect(() => {
    const list = listRef.current;
    const content = contentRef.current;
    if (!list || !content) return;
    const toBottom = () => {
      if (stick.current) list.scrollTop = list.scrollHeight;
    };
    const ro = new ResizeObserver(toBottom);
    ro.observe(content);
    ro.observe(list);
    toBottom();
    return () => ro.disconnect();
  }, []);

  const submit = async () => {
    if (chat.busy || (!text.trim() && atts.items.length === 0)) return;
    const payload = atts.items.map((a) => a.input);
    const t = text;
    setText("");
    atts.clear();
    stick.current = true;
    const list = listRef.current;
    if (list) list.scrollTop = list.scrollHeight;
    const ok = await chat.send(t, payload);
    if (!ok) setText(t);
  };

  return (
    <div className={`chat ${atts.dragging ? "dragging" : ""}`}>
      <header className="chat-head">
        <select
          value={chat.persona?.id ?? ""}
          onChange={(e) => void api.personaActivate(e.target.value)}
          title="切换人格（每个人格有自己的对话）"
        >
          {personas.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <div style={{ flex: 1 }} />
        <button className={`icon ${showSessions ? "on" : ""}`} title="Agent 会话" onClick={() => setShowSessions((v) => !v)}>
          🤖{active > 0 ? ` ${active}` : ""}
        </button>
        <button className="icon" title="设置" onClick={() => void api.openSettings()}>
          ⚙
        </button>
      </header>
      {showSessions && (
        <div className="sessions-panel">
          <SessionList list={sessions} />
        </div>
      )}

      <div
        className="chat-list"
        ref={listRef}
        onWheel={markUserScroll}
        onPointerDown={markUserScroll}
        onKeyDown={markUserScroll}
        onScroll={(e) => {
          const el = e.currentTarget;
          if (el.scrollHeight - el.scrollTop - el.clientHeight < 40) stick.current = true;
          else if (Date.now() - userScrollAt.current < 1000) stick.current = false;
        }}
      >
        <div className="chat-content" ref={contentRef}>
        {chat.hasMore && (
          <button className="more" onClick={() => void chat.loadMore()}>
            加载更早的消息
          </button>
        )}
        {chat.messages.length === 0 && chat.streaming === null && (
          <div className="empty muted">还没有聊过天。说点什么吧，图片和文件可以直接拖进来。</div>
        )}
        {chat.messages.map((m) => (
          <Bubble key={m.id} m={m} />
        ))}
        {chat.streaming !== null && (
          <div className="msg assistant">
            <ToolLines lines={chat.tools} />
            <div className="md streaming">
              {chat.streaming ? <Markdown remarkPlugins={[remarkGfm]}>{chat.streaming}</Markdown> : <span className="muted">思考中…</span>}
            </div>
          </div>
        )}
        </div>
      </div>

      <footer className="chat-input">
        {chat.error && <div className="error">{chat.error}</div>}
        {voice.state === "recording" && (
          <div className="voice-status">
            <span className="dot" />
            <span>正在听…</span>
            <span className="meter">
              <span style={{ width: `${Math.round(voice.level * 100)}%` }} />
            </span>
            <span className="muted small">松开结束</span>
          </div>
        )}
        {voice.state === "recognizing" && <div className="voice-status muted">识别中…</div>}
        {voice.error && <div className="error">{voice.error}</div>}
        {atts.items.length > 0 && (
          <div className="chips">
            {atts.items.map((a) => (
              <span className="chip" key={a.key} title={a.label}>
                {a.preview ? <img src={a.preview} alt="" className="chip-img" /> : "📄"}
                <span>{a.label}</span>
                <button onClick={() => atts.remove(a.key)}>×</button>
              </span>
            ))}
          </div>
        )}
        <div className="row">
          <button className="icon" title="添加图片或文件" onClick={async () => atts.add(await pickFiles())}>
            📎
          </button>
          {voice.enabled && (
            <button
              className={`icon mic ${voice.state === "recording" ? "on" : ""}`}
              title="按住说话，松开识别"
              disabled={voice.state === "recognizing" || chat.busy}
              {...voice.holdProps}
            >
              🎤
            </button>
          )}
          <textarea
            value={text}
            rows={2}
            placeholder="Enter 发送，Shift+Enter 换行；可以粘贴图片、拖入文件；/draw 描述 直接画图"
            onChange={(e) => setText(e.target.value)}
            onPaste={async (e) => {
              const imgs = await fromPaste(e);
              if (imgs.length) atts.add(imgs);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                void submit();
              }
            }}
          />
          {chat.busy ? (
            <button onClick={() => void chat.cancel()}>停止</button>
          ) : (
            <button className="primary" onClick={() => void submit()} disabled={!text.trim() && atts.items.length === 0}>
              发送
            </button>
          )}
        </div>
      </footer>
      <ApprovalDialog />
      <Lightbox />
    </div>
  );
}
