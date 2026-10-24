// Agent 会话列表：每个会话的工具、项目、状态、耗时
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, type SessionStatus, type SessionView } from "./api";
import "./sessions.css";

const STATUS: Record<SessionStatus, string> = {
  idle: "空闲",
  thinking: "思考中",
  working: "工作中",
  attention: "等你授权",
  done: "完成",
  error: "出错",
};

function fmt(s: number) {
  if (s < 60) return `${s} 秒`;
  if (s < 3600) return `${Math.floor(s / 60)} 分 ${s % 60} 秒`;
  return `${Math.floor(s / 3600)} 小时 ${Math.floor((s % 3600) / 60)} 分`;
}

export function useSessions() {
  const [list, setList] = useState<SessionView[]>([]);
  useEffect(() => {
    void api.agentsOverview().then((o) => setList(o.sessions));
    const un = listen<SessionView[]>("agents-changed", (e) => setList(e.payload));
    // 让耗时数字走起来
    const t = setInterval(() => setList((l) => l.map((s) => ({ ...s, seconds: s.seconds + 1 }))), 1000);
    return () => {
      clearInterval(t);
      void un.then((f) => f());
    };
  }, []);
  return list;
}

export function SessionList({ list }: { list: SessionView[] }) {
  if (!list.length) return <p className="muted small">现在没有正在进行的会话。</p>;
  return (
    <ul className="sessions">
      {list.map((s) => (
        <li key={s.agent + s.id} className={s.status} title={s.cwd}>
          <span className="dot" />
          <span className="who">{s.agentName}</span>
          <span className="proj">{s.project || "（未知项目）"}</span>
          <span className="st">
            {STATUS[s.status]}
            {s.status === "working" && s.tool ? ` · ${s.tool}` : ""}
          </span>
          <span className="time muted">{s.running ? fmt(s.seconds) : `${fmt(s.seconds)}前`}</span>
        </li>
      ))}
    </ul>
  );
}
