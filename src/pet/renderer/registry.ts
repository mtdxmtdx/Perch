// 渲染器登记表和插件加载。
// 内置只有精灵图集（sprite）。其他渲染器放在数据目录的 plugins/<文件夹>/ 里，
// 皮肤用到它时才加载插件代码（插件格式见 docs/renderer-plugin.md）。

import { invoke } from "@tauri-apps/api/core";
import { PET_ANIMS, type Renderer } from "./Renderer";
import { SpriteRenderer } from "./SpriteRenderer";
import { LiveMask, packMask } from "../masks";

/** 插件接口版本，接口有不兼容的改动时加一 */
export const PLUGIN_API_VERSION = 1;

const factories = new Map<string, () => Renderer>();

export function registerRenderer(kind: string, create: () => Renderer) {
  factories.set(kind, create);
}

registerRenderer("sprite", () => new SpriteRenderer());

/** 交给插件入口函数的东西 */
export interface PluginApi {
  apiVersion: number;
  registerRenderer: typeof registerRenderer;
  /** 会被要求播放的状态名 */
  anims: readonly string[];
  /** 运行时从画面读掩码 */
  LiveMask: typeof LiveMask;
  packMask: typeof packMask;
}

interface PluginCode {
  id: string;
  name: string;
  version: string;
  apiVersion: number;
  code: string;
}

/** 已经试过加载的插件（按渲染器名），同一个渲染器只加载一次 */
const tried = new Map<string, Promise<void>>();

async function loadPlugin(kind: string) {
  const p = await invoke<PluginCode | null>("renderer_plugin", { kind });
  if (!p) throw new Error(`没有提供「${kind}」渲染器的插件`);
  if (p.apiVersion !== PLUGIN_API_VERSION) {
    throw new Error(`插件 ${p.id} ${p.version} 的接口版本是 ${p.apiVersion}，这个版本的 Perch 需要 ${PLUGIN_API_VERSION}`);
  }
  // 插件是单个 ES 模块文件，默认导出入口函数
  const url = URL.createObjectURL(new Blob([p.code], { type: "text/javascript" }));
  try {
    const mod = await import(/* @vite-ignore */ url);
    if (typeof mod.default !== "function") throw new Error(`插件 ${p.id} 没有默认导出入口函数`);
    const api: PluginApi = { apiVersion: PLUGIN_API_VERSION, registerRenderer, anims: PET_ANIMS, LiveMask, packMask };
    await mod.default(api);
  } finally {
    URL.revokeObjectURL(url);
  }
  if (!factories.has(kind)) throw new Error(`插件 ${p.id} 加载了，但没有登记「${kind}」渲染器`);
  console.info(`已加载渲染器插件 ${p.name} ${p.version}`);
}

/** 建一个渲染器；不是内置的就先加载提供它的插件 */
export async function createRenderer(kind: string): Promise<Renderer> {
  if (!factories.has(kind)) {
    let job = tried.get(kind);
    if (!job) {
      job = loadPlugin(kind);
      tried.set(kind, job);
      // 失败了下次换皮肤时再试（可能是刚装上插件）
      job.catch(() => tried.delete(kind));
    }
    await job;
  }
  return factories.get(kind)!();
}
