import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "../shared/api";
import { useChat } from "../shared/useChat";
import { fromPaste, fromPaths, pickFiles, useAttachments } from "../shared/attach";
import { ToolLines } from "../shared/ToolLines";
import { ApprovalDialog } from "../chat/ApprovalDialog";
import { AgentOffer } from "./AgentOffer";
import { useVoice } from "../shared/useVoice";
import "./bubble.css";

/** 气泡里的小缩略图，点了打开聊天窗口看大图 */
function MiniThumb({ file, title }: { file: string; title: string }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let u: string | null = null;
    void api.attachmentImage(file).then((buf) => {
      u = URL.createObjectURL(new Blob([buf]));
      setUrl(u);
    });
    return () => {
      if (u) URL.revokeObjectURL(u);
    };
  }, [file]);
  return url ? <img className="mini-thumb" src={url} alt={title} title={`${title}（点击在聊天窗口查看）`} onClick={() => void api.openChat()} /> : null;
}

const WIDTH = 320;
const TAIL = 10;

interface Anchor {
  placement: "above" | "side";
  tailRatio: number;
}

export function Bubble() {
  const chat = useChat(20);
  const atts = useAttachments();
  const [mode, setMode] = useState<"input" | "speak">("input");
  const [anchor, setAnchor] = useState<Anchor>({ placement: "above", tailRatio: 0.5 });
  const [text, setText] = useState("");
  // 录音时在输入框里实时预览识别结果
  const voice = useVoice({ text, setText });
  const [sessionFrom, setSessionFrom] = useState(Number.MAX_SAFE_INTEGER);
  const [maxChars, setMaxChars] = useState(160);
  // 桌宠主动说的话（agent 汇报等）
  const [said, setSaid] = useState<string | null>(null);
  const [hover, setHover] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const replyRef = useRef<HTMLDivElement>(null);
  // 打开文件对话框时气泡会失焦，这段时间不要自动关闭
  const suppressBlur = useRef(false);
  // 有待确认的修改时，点别处也不收起气泡
  const approvalPending = useRef(false);
  const onPendingChange = useCallback((p: boolean) => {
    approvalPending.current = p;
  }, []);
  // 接入询问还没回答时同样不收起
  const offerPending = useRef(false);
  const [offerOpen, setOfferOpen] = useState(false);
  const onOfferPending = useCallback((p: boolean) => {
    offerPending.current = p;
    setOfferOpen(p);
  }, []);

  useEffect(() => {
    void api.settingsGet().then((s) => setMaxChars(s.chat.bubble_chars));
    const subs = Promise.all([
      listen<"input" | "speak">("bubble-mode", (e) => {
        setMode(e.payload);
        // 用户主动打开输入气泡时，清掉之前的汇报
        if (e.payload === "input") setSaid(null);
        // 每次打开都是新的一轮，只显示这次之后的回复
        setSessionFrom(Date.now() / 1000 - 1);
        chat.setError(null);
        if (e.payload === "input") setTimeout(() => inputRef.current?.focus(), 60);
      }),
      listen<Anchor>("bubble-anchor", (e) => setAnchor(e.payload)),
      listen<string>("bubble-say", (e) => setSaid(e.payload)),
      listen<string[]>("bubble-attach", (e) => atts.add(fromPaths(e.payload))),
      listen("settings-changed", () => void api.settingsGet().then((s) => setMaxChars(s.chat.bubble_chars))),
      getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        if (!focused && !suppressBlur.current && !approvalPending.current && !offerPending.current) void api.bubbleClose();
      }),
    ]);
    return () => void subs.then((fns) => fns.forEach((f) => f()));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 按内容高度调整窗口大小
  useLayoutEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const report = () => void api.bubbleResize(WIDTH, Math.ceil(el.getBoundingClientRect().height));
    const ro = new ResizeObserver(report);
    ro.observe(el);
    report();
    return () => ro.disconnect();
  }, []);

  const lastReply = useMemo(
    () => [...chat.messages].reverse().find((m) => m.role === "assistant" && m.createdAt >= sessionFrom) ?? null,
    [chat.messages, sessionFrom],
  );
  const reply = chat.streaming !== null ? chat.streaming || (chat.tools.length ? null : "…") : lastReply?.display ?? null;
  const images = chat.streaming === null ? (lastReply?.attachments ?? []).filter((a) => a.kind === "image" && a.file) : [];

  const shown = reply && reply.length > maxChars ? reply.slice(0, maxChars) + "…" : reply;

  // 回复超出气泡高度时跟着滚到最新的字
  useLayoutEffect(() => {
    const el = replyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [shown]);
  const truncated = !!reply && reply.length > maxChars;

  // 说话模式：8 秒后自动收起，鼠标停在气泡上或接入询问还没回答时不收
  useEffect(() => {
    if (mode !== "speak" || !said || hover || offerOpen) return;
    const t = setTimeout(() => void api.bubbleClose(), 8000);
    return () => clearTimeout(t);
  }, [mode, said, hover, offerOpen]);

  const submit = async () => {
    if (chat.busy || (!text.trim() && atts.items.length === 0)) return;
    const payload = atts.items.map((a) => a.input);
    const t = text;
    setText("");
    atts.clear();
    const ok = await chat.send(t, payload);
    if (!ok) setText(t);
  };

  const attach = async () => {
    suppressBlur.current = true;
    try {
      atts.add(await pickFiles());
    } finally {
      suppressBlur.current = false;
      inputRef.current?.focus();
    }
  };

  return (
    <div className={`bubble-wrap ${anchor.placement}`} ref={wrapRef} onMouseEnter={() => setHover(true)} onMouseLeave={() => setHover(false)}>
      <div className={`bubble ${atts.dragging ? "dragging" : ""}`}>
        <div className="bubble-head">
          <span className="muted">{chat.persona?.name ?? ""}</span>
          <button className="icon" title="展开成聊天窗口" onClick={() => void api.openChat()}>
            ⤢
          </button>
        </div>

        {said && (
          <div
            className="bubble-said"
            title={mode === "speak" ? "点一下回复" : undefined}
            onClick={() => {
              if (mode === "speak") void api.bubbleOpen("input");
            }}
          >
            <span>{said}</span>
            <button className="icon" title="知道了" onClick={(e) => { e.stopPropagation(); setSaid(null); if (mode === "speak") void api.bubbleClose(); }}>
              ×
            </button>
          </div>
        )}
        {shown && (
          <div ref={replyRef} className={`bubble-reply ${chat.streaming !== null ? "streaming" : ""}`}>
            {shown}
            {truncated && (
              <button className="link" onClick={() => void api.openChat()}>
                展开看全文
              </button>
            )}
          </div>
        )}
        {chat.streaming !== null && <ToolLines lines={chat.tools} compact />}
        {images.length > 0 && (
          <div className="mini-thumbs">
            {images.map((a) => (
              <MiniThumb key={a.file!} file={a.file!} title={a.name} />
            ))}
          </div>
        )}
        <ApprovalDialog inline onPendingChange={onPendingChange} />
        <AgentOffer onPendingChange={onOfferPending} onFinished={setSaid} />
        {chat.error && <div className="error">{chat.error}</div>}
        {voice.state === "recording" && (
          <div className="voice-status">
            <span className="dot" />
            <span>正在听…</span>
            <span className="meter">
              <span style={{ width: `${Math.round(voice.level * 100)}%` }} />
            </span>
            <span className="muted small">松开结束 · Esc 取消</span>
          </div>
        )}
        {voice.state === "recognizing" && <div className="voice-status muted">识别中…</div>}
        {voice.heard && voice.state === "idle" && <div className="voice-heard muted">🎤 {voice.heard}</div>}
        {voice.error && <div className="error">{voice.error}</div>}

        {mode === "input" && (
          <>
            {atts.items.length > 0 && (
              <div className="chips">
                {atts.items.map((a) => (
                  <span className="chip" key={a.key} title={a.label}>
                    <span>{a.preview ? "🖼 " : "📄 "}{a.label}</span>
                    <button onClick={() => atts.remove(a.key)}>×</button>
                  </span>
                ))}
              </div>
            )}
            <div className="bubble-input">
              <textarea
                ref={inputRef}
                rows={1}
                value={text}
                placeholder={chat.busy ? "等它说完…" : "说点什么…（/draw 描述 可以画图）"}
                onChange={(e) => setText(e.target.value)}
                onPaste={async (e) => {
                  const imgs = await fromPaste(e);
                  if (imgs.length) atts.add(imgs);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                    e.preventDefault();
                    void submit();
                  } else if (e.key === "Escape") {
                    if (voice.state === "recording") voice.cancel();
                    else void api.bubbleClose();
                  }
                }}
              />
              {chat.busy ? (
                <button className="icon" title="停止" onClick={() => void chat.cancel()}>
                  ■
                </button>
              ) : (
                <>
                  {voice.enabled && (
                    <button
                      className={`icon mic ${voice.state === "recording" ? "on" : ""}`}
                      title="按住说话，松开识别"
                      disabled={voice.state === "recognizing"}
                      {...voice.holdProps}
                    >
                      🎤
                    </button>
                  )}
                  <button className="icon" title="添加图片或文件（也可以直接拖进来、粘贴图片）" onClick={() => void attach()}>
                    📎
                  </button>
                </>
              )}
            </div>
          </>
        )}
      </div>
      {anchor.placement === "above" && (
        <div className="bubble-tail" style={{ left: `calc(${anchor.tailRatio * 100}% - ${TAIL}px)` }} />
      )}
    </div>
  );
}
