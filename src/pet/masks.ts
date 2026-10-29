// 鼠标穿透用的不透明掩码（给 Rust 判定光标是否落在角色身上）。
// 精灵图集：加载时给每帧预先算好，换帧时只报帧号。
// 骨骼动画这类没法预先算每帧的：渲染时从画面实时读（LiveMask），再发给 Rust。

import { COLS, type LoadedSkin } from "./skin/codexPet";
import type { FrameMasks, HitMask, PetLayout } from "./renderer/Renderer";

/** 掩码大约多少格宽：和图集一格 192px、4 倍降采样的精度一致 */
const MASK_COLS = 48;
const ALPHA_MIN = 40;

/**
 * 把 w×h 的 alpha 图降采样打包成位图：每个 downsample×downsample 的块里有一个像素够不透明就算实心。
 * 位图按行优先、低位在前打包。
 */
export function packMask(w: number, h: number, alpha: (x: number, y: number) => number, downsample: number): HitMask {
  const ds = Math.max(1, Math.floor(downsample));
  const mw = Math.ceil(w / ds);
  const mh = Math.ceil(h / ds);
  const bits = new Array<number>(Math.ceil((mw * mh) / 8)).fill(0);
  for (let my = 0; my < mh; my++) {
    for (let mx = 0; mx < mw; mx++) {
      let solid = false;
      for (let y = my * ds; y < Math.min(h, (my + 1) * ds) && !solid; y++) {
        for (let x = mx * ds; x < Math.min(w, (mx + 1) * ds); x++) {
          if (alpha(x, y) >= ALPHA_MIN) {
            solid = true;
            break;
          }
        }
      }
      if (solid) {
        const i = my * mw + mx;
        bits[i >> 3] |= 1 << (i & 7);
      }
    }
  }
  return { w: mw, h: mh, bits };
}

/**
 * 运行时掩码：渲染器每画完一帧调用 sample(画布)，按间隔节流，画面没变时不重复上报。
 * 画布可以是 2D 也可以是 WebGL 的（WebGL 要在画完的同一个任务里调用，或者开 preserveDrawingBuffer）。
 */
export class LiveMask {
  private scratch?: OffscreenCanvas;
  private ctx?: OffscreenCanvasRenderingContext2D;
  private last = 0;
  private lastKey = "";

  /** minIntervalMs：两次读画面的最短间隔 */
  constructor(private minIntervalMs = 80) {}

  /** 返回新掩码；还没到间隔、或者和上次一样时返回 null。force 忽略间隔（比如换了动画） */
  sample(source: CanvasImageSource & { width: number; height: number }, force = false): HitMask | null {
    const now = performance.now();
    if (!force && now - this.last < this.minIntervalMs) return null;
    const { width: w, height: h } = source;
    if (!w || !h) return null;
    this.last = now;
    if (!this.scratch || this.scratch.width !== w || this.scratch.height !== h) {
      this.scratch = new OffscreenCanvas(w, h);
      this.ctx = this.scratch.getContext("2d", { willReadFrequently: true })!;
    }
    const ctx = this.ctx!;
    ctx.clearRect(0, 0, w, h);
    ctx.drawImage(source, 0, 0);
    const { data } = ctx.getImageData(0, 0, w, h);
    const mask = packMask(w, h, (x, y) => data[(y * w + x) * 4 + 3], w / MASK_COLS);
    const key = mask.bits.join(",");
    if (key === this.lastKey) return null;
    this.lastKey = key;
    return mask;
  }

  /** 换皮肤后清掉记录，保证下一次一定上报 */
  reset() {
    this.last = 0;
    this.lastKey = "";
  }
}

/** 精灵图集：每帧的掩码，加上从 idle 行算出的脚底、中心和头顶 */
export function spriteMasks(skin: LoadedSkin): { layout: PetLayout; frameMasks: FrameMasks } {
  const canvas = new OffscreenCanvas(skin.sheetW, skin.sheetH);
  const ctx = canvas.getContext("2d", { willReadFrequently: true })!;
  ctx.drawImage(skin.sheet, 0, 0);
  const { data } = ctx.getImageData(0, 0, skin.sheetW, skin.sheetH);
  const alpha = (x: number, y: number) => data[(y * skin.sheetW + x) * 4 + 3];

  const cw = Math.floor(skin.cellW);
  const ch = Math.floor(skin.cellH);
  const ds = cw / MASK_COLS;

  const masks: number[][] = [];
  let maskW = 0;
  let maskH = 0;
  for (let row = 0; row < skin.rows; row++) {
    for (let col = 0; col < COLS; col++) {
      const ox = Math.floor(col * skin.cellW);
      const oy = Math.floor(row * skin.cellH);
      const m = packMask(cw, ch, (x, y) => alpha(ox + x, oy + y), ds);
      maskW = m.w;
      maskH = m.h;
      masks.push(m.bits);
    }
  }

  // 用 idle 行（第 0 行）所有帧的并集确定脚底和身体中心
  let minY = ch;
  let maxY = -1;
  let minX = cw;
  let maxX = -1;
  for (let col = 0; col < COLS; col++) {
    const ox = Math.floor(col * skin.cellW);
    for (let y = 0; y < ch; y++) {
      for (let x = 0; x < cw; x++) {
        if (alpha(ox + x, y) >= ALPHA_MIN) {
          if (y > maxY) maxY = y;
          if (y < minY) minY = y;
          if (x < minX) minX = x;
          if (x > maxX) maxX = x;
        }
      }
    }
  }

  return {
    layout: {
      cellW: skin.cellW,
      cellH: skin.cellH,
      footRatio: maxY >= 0 ? (maxY + 1) / ch : 1,
      centerRatio: maxX >= 0 ? (minX + maxX + 1) / 2 / cw : 0.5,
      topRatio: maxY >= 0 ? minY / ch : 0,
      canSit: skin.canSit,
    },
    frameMasks: { maskW, maskH, masks },
  };
}
