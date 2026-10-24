// 气泡和聊天窗口共用的对话状态：历史消息 + 正在生成的回复。
// 两个窗口都监听同一个 chat-event，所以在哪边发消息另一边都会同步。

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorText, type AttachmentIn, type ChatEvent, type Message, type ToolLine } from "./api";

export function useChat(pageSize = 50) {
  const [persona, setPersona] = useState<{ id: string; name: string } | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [streaming, setStreaming] = useState<string | null>(null);
  // 正在进行的这一轮里的工具调用
  const [tools, setTools] = useState<ToolLine[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const personaRef = useRef<string | null>(null);

  const reload = useCallback(async () => {
    const h = await api.chatHistory(undefined, pageSize);
    personaRef.current = h.persona;
    setPersona({ id: h.persona, name: h.personaName });
    setMessages(h.messages);
    setHasMore(h.messages.length >= pageSize);
    setStreaming(null);
  }, [pageSize]);

  const loadMore = useCallback(async () => {
    const first = messages[0];
    if (!first) return;
    const h = await api.chatHistory(first.id, pageSize);
    setMessages((m) => [...h.messages, ...m]);
    setHasMore(h.messages.length >= pageSize);
  }, [messages, pageSize]);

  useEffect(() => {
    void reload();
    const subs = Promise.all([
      listen<ChatEvent>("chat-event", ({ payload: ev }) => {
        if (ev.persona !== personaRef.current) return;
        switch (ev.type) {
          case "user":
            setError(null);
            setMessages((m) => [...m, ev.message]);
            break;
          case "start":
            setBusy(true);
            setStreaming("");
            setTools([]);
            break;
          case "tool":
            setTools((t) => {
              const line = { id: ev.id, title: ev.title, status: ev.status, detail: ev.detail };
              return t.some((x) => x.id === ev.id) ? t.map((x) => (x.id === ev.id ? line : x)) : [...t, line];
            });
            break;
          case "delta":
            setStreaming((s) => (s ?? "") + ev.text);
            break;
          case "done":
            setMessages((m) => [...m, ev.message]);
            setStreaming(null);
            setTools([]);
            setBusy(false);
            break;
          case "error":
            setStreaming(null);
            setTools([]);
            setBusy(false);
            setError(ev.error);
            break;
        }
      }),
      listen("settings-changed", () => void reload()),
      listen("chat-cleared", () => void reload()),
    ]);
    return () => void subs.then((fns) => fns.forEach((f) => f()));
  }, [reload]);

  const send = useCallback(async (text: string, attachments: AttachmentIn[]) => {
    setError(null);
    setBusy(true);
    try {
      await api.chatSend(text, attachments);
      return true;
    } catch (e) {
      // 发送前就失败的（没配模型、附件读不了）不会有 error 事件，在这里显示
      setError(errorText(e));
      return false;
    } finally {
      setBusy(false);
    }
  }, []);

  return { persona, messages, streaming, tools, busy, error, setError, hasMore, loadMore, send, cancel: api.chatCancel, reload };
}
