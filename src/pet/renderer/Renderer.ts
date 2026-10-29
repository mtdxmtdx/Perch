import type { SkinSource } from "../skin/source";

// 渲染器的统一接口。内置的是精灵图集（sprite），其他的（比如 Spine）做成插件，
// 皮肤在 skin.json 里写 "renderer": "<名字>" 选用。插件怎么写见 docs/renderer-plugin.md。

/** 程序会让渲染器播放的状态名。没有对应动画的状态，渲染器自己挑一个相近的（通常是 idle） */
export const PET_ANIMS = [
  "idle",
  "walk_right",
  "walk_left",
  "wave",
  "talk",
  "jump",
  "fall",
  "land",
  "drag",
  "sleep",
  "sit",
  "listen",
  "think",
  "working",
  "attention",
  "error",
  "done",
] as const;

/** 角色在画面里的位置。程序拿它把脚对齐到任务栏或窗口顶边，把气泡放在头顶 */
export interface PetLayout {
  /** 画面的基准尺寸（逻辑像素），窗口按它和"大小"设置定尺寸，宽高比也按它 */
  cellW: number;
  cellH: number;
  /** 脚底在画面里的高度比例（0 顶 ~ 1 底） */
  footRatio: number;
  /** 身体水平中心的比例（0 左 ~ 1 右） */
  centerRatio: number;
  /** 头顶的高度比例，气泡贴着它放 */
  topRatio: number;
  /** 有"坐"的动画：有才会坐在窗沿上 */
  canSit: boolean;
}

/** 一帧的不透明掩码：w×h 位图，行优先、低位在前打包 */
export interface HitMask {
  w: number;
  h: number;
  bits: number[];
}

/** 预先算好的每帧掩码，帧号由渲染器自己定 */
export interface FrameMasks {
  maskW: number;
  maskH: number;
  masks: number[][];
}

/**
 * 鼠标穿透用的画面更新，二选一：
 * - 加载时给了 frameMasks 的，换帧时报帧号；
 * - 没给的，画面变化时报当前画面的掩码（用 LiveMask 从画布读）。
 */
export type HitUpdate = { frame: number } | { mask: HitMask };

export interface LoadResult {
  layout: PetLayout;
  frameMasks?: FrameMasks;
}

export interface Renderer {
  /** 挂到桌宠窗口里：自己建画布（2D、WebGL 都行）放进 host，铺满 host */
  mount(host: HTMLElement): void;
  /** 加载皮肤。可以重复调用（换皮肤），换完继续播放当前状态 */
  load(src: SkinSource): Promise<LoadResult>;
  /** 播放某个桌宠状态的动画（见 PET_ANIMS）；重复调用同一个名字不会重新开始 */
  play(anim: string): void;
  onHit(cb: (update: HitUpdate) => void): void;
  /** 窗口尺寸变了 */
  resize(): void;
  /** 停掉动画、释放资源 */
  destroy(): void;
}
