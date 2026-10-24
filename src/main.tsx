import { createRoot } from "react-dom/client";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { PetView } from "./pet/PetView";
import { Bubble } from "./bubble/Bubble";
import { ChatWindow } from "./chat/ChatWindow";
import { Settings } from "./settings/Settings";
import { StatusTags } from "./status/StatusTags";
import "./styles.css";

// 所有窗口共用一个前端包，按窗口 label 渲染不同界面
const label = getCurrentWebviewWindow().label;

if (label === "pet" || label === "bubble" || label === "status") {
  // 透明窗口不需要浏览器默认的右键菜单和拖拽
  window.addEventListener("contextmenu", (e) => e.preventDefault());
} else {
  document.documentElement.classList.add("opaque");
}
window.addEventListener("dragstart", (e) => {
  if ((e.target as HTMLElement).tagName === "IMG") e.preventDefault();
});

const views: Record<string, () => React.JSX.Element | null> = {
  pet: PetView,
  bubble: Bubble,
  chat: ChatWindow,
  settings: Settings,
  status: StatusTags,
};
const View = views[label] ?? PetView;

createRoot(document.getElementById("root")!).render(<View />);
