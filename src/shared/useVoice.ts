// 按住说话：按下 🎤 开始录音，松开识别。气泡和聊天窗口共用；快捷键触发的录音也会在这里显示状态。
// 录音过程中把实时识别结果放进输入框预览（接在原有草稿后面）。

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorText, type VoiceEvent } from "./api";

export type VoiceState = "idle" | "recording" | "recognizing";

/** 错误提示停留多久 */
const ERROR_MS = 4000;
/** "🎤 识别到的话"停留多久 */
const HEARD_MS = 6000;

/** input：输入框当前的文字和修改方法，用来做预览 */
export function useVoice(input: { text: string; setText: (t: string) => void }) {
  const [state, setState] = useState<VoiceState>("idle");
  const [level, setLevel] = useState(0);
  const [error, setError] = useState<string | null>(null);
  /** 设置里开启了语音输入（关着时不显示 🎤） */
  const [enabled, setEnabled] = useState(false);
  /** 识别出来并已经发送的话，显示一会儿 */
  const [heard, setHeard] = useState<string | null>(null);
  const inputRef = useRef(input);
  inputRef.current = input;
  // 开始录音时输入框里原有的草稿；预览接在它后面
  const base = useRef<string | null>(null);

  const show = (t: string) => {
    const b = base.current ?? "";
    inputRef.current.setText(b ? `${b}${t}` : t);
  };
  const restore = () => {
    if (base.current !== null) inputRef.current.setText(base.current);
    base.current = null;
  };

  useEffect(() => {
    const un = listen<VoiceEvent>("voice", ({ payload: e }) => {
      switch (e.state) {
        case "recording":
          setState("recording");
          setError(null);
          setHeard(null);
          base.current = inputRef.current.text;
          break;
        case "level":
          setLevel(e.level);
          break;
        case "partial":
          if (base.current !== null) show(e.text);
          break;
        case "recognizing":
          setState("recognizing");
          setLevel(0);
          break;
        case "idle":
          setState("idle");
          setLevel(0);
          restore();
          break;
        case "result":
          setState("idle");
          if (e.sent) {
            // 已经直接发出去了：输入框回到原来的草稿
            restore();
            setHeard(e.text);
          } else {
            show(e.text);
            base.current = null;
          }
          break;
        case "error":
          setState("idle");
          restore();
          setError(e.error);
          break;
      }
    });
    return () => void un.then((f) => f());
  }, []);

  useEffect(() => {
    const load = () => void api.settingsGet().then((s) => setEnabled(s.voice.enabled));
    load();
    const un = listen("settings-changed", load);
    return () => void un.then((f) => f());
  }, []);

  // 提示过一会儿自动消失
  useEffect(() => {
    if (!error) return;
    const t = setTimeout(() => setError(null), ERROR_MS);
    return () => clearTimeout(t);
  }, [error]);
  useEffect(() => {
    if (!heard) return;
    const t = setTimeout(() => setHeard(null), HEARD_MS);
    return () => clearTimeout(t);
  }, [heard]);

  const start = useCallback(async () => {
    setError(null);
    try {
      await api.voiceStart();
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  const stop = useCallback(() => void api.voiceStop().catch((e) => setError(errorText(e))), []);
  const cancel = useCallback(() => void api.voiceCancel(), []);

  /** 绑到 🎤 按钮上：按住说话，松开识别 */
  const holdProps = {
    onPointerDown: (e: React.PointerEvent<HTMLButtonElement>) => {
      e.currentTarget.setPointerCapture(e.pointerId);
      void start();
    },
    onPointerUp: () => stop(),
    onPointerCancel: () => cancel(),
  };

  return { enabled, state, level, error, setError, heard, setHeard, start, stop, cancel, holdProps };
}
