import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { HitMask, HitUpdate, Renderer } from "./renderer/Renderer";
import { createRenderer } from "./renderer/registry";
import { loadSkinSource, PLACEHOLDER_SOURCE, rendererKind, type SkinSource } from "./skin/source";

export function PetView() {
  const hostRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = hostRef.current!;
    let renderer: Renderer | null = null;
    let kind = "";
    let anim = "idle";
    let disposed = false;
    // 皮肤尺寸和位置发给 Rust 之前，运行时掩码先存着，发完再补上（set_skin_metrics 会清掉旧掩码）
    let metricsSent = false;
    let pendingMask: HitMask | null = null;

    const onHit = (u: HitUpdate) => {
      if ("frame" in u) {
        void invoke("set_frame", { index: u.frame });
      } else if (metricsSent) {
        void invoke("set_live_mask", { w: u.mask.w, h: u.mask.h, bits: u.mask.bits });
      } else {
        pendingMask = u.mask;
      }
    };

    const apply = async (src: SkinSource) => {
      const k = rendererKind(src.info);
      if (!renderer || k !== kind) {
        // 不同渲染器要的画布不一样（2D / WebGL），整个换掉
        const next = await createRenderer(k);
        if (disposed) return;
        renderer?.destroy();
        host.replaceChildren();
        renderer = next;
        kind = k;
        renderer.mount(host);
        renderer.onHit(onHit);
      }
      metricsSent = false;
      pendingMask = null;
      const { layout, frameMasks } = await renderer.load(src);
      if (disposed) return;
      renderer.play(anim);
      await invoke("set_skin_metrics", { metrics: { ...layout, ...frameMasks } });
      metricsSent = true;
      if (pendingMask) onHit({ mask: pendingMask });
    };

    const loadSkin = async () => {
      try {
        await apply(await loadSkinSource());
      } catch (e) {
        console.error("加载皮肤失败，改用占位形象", e);
        try {
          await apply(PLACEHOLDER_SOURCE);
        } catch (e2) {
          console.error("占位形象也加载失败", e2);
        }
      }
    };

    const onResize = () => renderer?.resize();
    window.addEventListener("resize", onResize);

    // 先挂好监听，再加载皮肤：Rust 在皮肤就绪后才开始发动画事件
    const unlisten = Promise.all([
      listen<string>("pet-anim", (e) => {
        anim = e.payload;
        renderer?.play(anim);
      }),
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
      renderer?.destroy();
      void unlisten.then((fns) => fns.forEach((f) => f()));
    };
  }, []);

  return (
    <div
      ref={hostRef}
      style={{ width: "100%", height: "100%", cursor: "grab" }}
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
