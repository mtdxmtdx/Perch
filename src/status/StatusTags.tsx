// Agent 状态标签：桌宠身边的小胶囊，"Claude Code · 思考中"。
// 窗口点击穿透、由控制线程跟着桌宠摆放；这里只负责画出来并上报尺寸（没有标签时上报 0，窗口停到屏幕外）。
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, type SessionStatus, type SessionView } from "../shared/api";
import { useSessions } from "../shared/Sessions";
import "./status.css";

const LABEL: Record<SessionStatus, string> = {
  idle: "",
  thinking: "思考中",
  working: "工作中",
  attention: "等待授权",
  done: "完成",
  error: "出错了",
};

/** 越需要注意的排越前 */
const ORDER: SessionStatus[] = ["attention", "error", "working", "thinking", "done"];
const MAX_TAGS = 3;

const SHORT_NAME: Record<string, string> = { dsh: "DSH" };

function clock(s: number) {
  const m = Math.floor(s / 60);
  return m >= 60 ? `${Math.floor(m / 60)}:${String(m % 60).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}` : `${m}:${String(s % 60).padStart(2, "0")}`;
}

function Tag({ s }: { s: SessionView }) {
  const label = LABEL[s.status] + (s.status === "working" && s.tool ? ` · ${s.tool}` : "");
  const live = s.running && (s.status === "thinking" || s.status === "working");
  return (
    <div className={`tag ${s.status}`}>
      <span className="dot" />
      <span className="who">{SHORT_NAME[s.agent] ?? s.agentName}</span>
      <span className="what">{label}</span>
      {s.project && <span className="proj">{s.project}</span>}
      {live && <span className="time">{clock(s.seconds)}</span>}
    </div>
  );
}

export function StatusTags() {
  const sessions = useSessions();
  const [enabled, setEnabled] = useState(true);
  const [side, setSide] = useState<"left" | "right">("right");
  const box = useRef<HTMLDivElement>(null);
  const reported = useRef("");

  useEffect(() => {
    const load = () => void api.settingsGet().then((s) => setEnabled(s.agents.status_tag !== false));
    load();
    const uns = [listen("settings-changed", load), listen<"left" | "right">("status-side", (e) => setSide(e.payload))];
    return () => uns.forEach((u) => void u.then((f) => f()));
  }, []);

  const shown = enabled
    ? sessions.filter((s) => s.status !== "idle").sort((a, b) => ORDER.indexOf(a.status) - ORDER.indexOf(b.status))
    : [];
  const extra = shown.length - MAX_TAGS;

  // 每次内容变化后量一下尺寸告诉后端（窗口大小跟着内容走）
  useLayoutEffect(() => {
    const el = box.current;
    const size = shown.length && el ? [Math.ceil(el.offsetWidth), Math.ceil(el.offsetHeight)] : [0, 0];
    const key = size.join("x");
    if (key !== reported.current) {
      reported.current = key;
      void api.statusResize(size[0], size[1]);
    }
  });

  if (!shown.length) return null;
  return (
    <div ref={box} className={`tags ${side}`}>
      {shown.slice(0, MAX_TAGS).map((s) => (
        <Tag key={s.agent + s.id} s={s} />
      ))}
      {extra > 0 && <div className="more">还有 {extra} 个会话</div>}
    </div>
  );
}
