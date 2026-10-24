import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { SpriteRenderer } from "./renderer/SpriteRenderer";
import { loadCurrentSkin } from "./skin/codexPet";
import { computeMetrics } from "./masks";

export function PetView() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const renderer = new SpriteRenderer();
    renderer.mount(canvasRef.current!);
    renderer.onFrame((index) => void invoke("set_frame", { index }));

    let disposed = false;
    const loadSkin = async () => {
      try {
        const skin = await loadCurrentSkin();
        const metrics = computeMetrics(skin);
        if (disposed) return;
        await renderer.load(skin);
        await invoke("set_skin_metrics", { metrics });
      } catch (e) {
        console.error("加载皮肤失败", e);
      }
    };

    const onResize = () => renderer.resize();
    window.addEventListener("resize", onResize);

    // 先挂好监听，再加载皮肤：Rust 在皮肤就绪后才开始发动画事件
    const unlisten = Promise.all([
      listen<string>("pet-anim", (e) => renderer.play(e.payload)),
      listen("pet-skin-changed", () => void loadSkin()),
      // 把文件拖到桌宠身上：弹出输入气泡并带上这些文件
      getCurrentWebview().onDragDropEvent((e) => {
        if (e.payload.type === "drop" && e.payload.paths.length) void invoke("pet_drop_files", { paths: e.payload.paths });
      }),
    ]);
    void unlisten.then(loadSkin);

    return () => {
      disposed = true;
      window.removeEventListener("resize", onResize);
      renderer.destroy();
      void unlisten.then((fns) => fns.forEach((f) => f()));
    };
  }, []);

  return (
    <canvas
      ref={canvasRef}
      style={{ width: "100%", height: "100%", display: "block", cursor: "grab" }}
      onMouseDown={(e) => {
        if (e.button === 0) void invoke("pointer_down");
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        void invoke("show_menu");
      }}
    />
  );
}
