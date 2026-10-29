// 当前皮肤的信息和文件读取，交给渲染器用。

import { invoke } from "@tauri-apps/api/core";

export interface SkinInfo {
  id: string;
  name: string;
  /** pet.json 原文 */
  pet: { spriteVersionNumber?: number; spritesheetPath?: string; [k: string]: unknown };
  /** skin.json 原文（没有就是 null） */
  overrides: unknown;
}

export interface SkinSource {
  /** null：一个皮肤都没有，用占位形象 */
  info: SkinInfo | null;
  /** 读皮肤文件夹里的文件，path 是相对皮肤文件夹的路径 */
  readFile(path: string): Promise<ArrayBuffer>;
}

/** 皮肤要用的渲染器：skin.json 里的 renderer，没写就是精灵图集 */
export function rendererKind(info: SkinInfo | null): string {
  const r = (info?.overrides as { renderer?: unknown } | null)?.renderer;
  return typeof r === "string" && r.trim() ? r.trim() : "sprite";
}

export const PLACEHOLDER_SOURCE: SkinSource = {
  info: null,
  readFile: () => Promise.reject(new Error("占位形象没有文件")),
};

export async function loadSkinSource(): Promise<SkinSource> {
  const info = await invoke<SkinInfo | null>("skin_info");
  if (!info) return PLACEHOLDER_SOURCE;
  return { info, readFile: (path) => invoke<ArrayBuffer>("skin_file", { path }) };
}
