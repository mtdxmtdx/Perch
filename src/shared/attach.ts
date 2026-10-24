// 附件的收集：拖入的文件、对话框选择的文件、粘贴的图片

import { useEffect, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import type { AttachmentIn } from "./api";

export interface PendingAttachment {
  key: string;
  label: string;
  input: AttachmentIn;
  /** 粘贴图片的预览 */
  preview?: string;
}

const baseName = (p: string) => p.split(/[\\/]/).pop() ?? p;

export function fromPaths(paths: string[]): PendingAttachment[] {
  return paths.map((path) => ({ key: path + Math.random(), label: baseName(path), input: { kind: "path", path } }));
}

function readAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(r.result as string);
    r.onerror = () => reject(r.error);
    r.readAsDataURL(file);
  });
}

/** 从粘贴事件里取出图片 */
export async function fromPaste(e: React.ClipboardEvent): Promise<PendingAttachment[]> {
  const out: PendingAttachment[] = [];
  for (const item of Array.from(e.clipboardData.items)) {
    if (item.kind !== "file" || !item.type.startsWith("image/")) continue;
    const file = item.getAsFile();
    if (!file) continue;
    const url = await readAsDataUrl(file);
    const name = file.name && file.name !== "image.png" ? file.name : `粘贴的图片.${item.type.split("/")[1] || "png"}`;
    out.push({ key: url.slice(-32) + Math.random(), label: name, preview: url, input: { kind: "imageData", name, mime: item.type, data: url } });
  }
  return out;
}

export async function pickFiles(): Promise<PendingAttachment[]> {
  const picked = await open({
    multiple: true,
    filters: [
      { name: "支持的文件", extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp", "txt", "md", "json", "csv", "pdf", "docx", "log", "py", "js", "ts", "rs"] },
      { name: "所有文件", extensions: ["*"] },
    ],
  });
  if (!picked) return [];
  return fromPaths(Array.isArray(picked) ? picked : [picked]);
}

export function useAttachments() {
  const [items, setItems] = useState<PendingAttachment[]>([]);
  const [dragging, setDragging] = useState(false);

  useEffect(() => {
    const un = getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      if (p.type === "over" || p.type === "enter") setDragging(true);
      else if (p.type === "leave") setDragging(false);
      else if (p.type === "drop") {
        setDragging(false);
        const paths = p.paths;
        setItems((cur) => [...cur, ...fromPaths(paths)]);
      }
    });
    return () => void un.then((f) => f());
  }, []);

  return {
    items,
    dragging,
    add: (more: PendingAttachment[]) => setItems((cur) => [...cur, ...more]),
    remove: (key: string) => setItems((cur) => cur.filter((i) => i.key !== key)),
    clear: () => setItems([]),
  };
}
