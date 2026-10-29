// 为图集每一帧生成低分辨率的不透明掩码（给 Rust 做鼠标穿透判定），并找出脚底和身体中心。

import { COLS, type LoadedSkin } from "./skin/codexPet";

/** 掩码相对一格精灵的降采样倍数 */
const DOWNSAMPLE = 4;
const ALPHA_MIN = 40;

export interface SkinMetrics {
  cellW: number;
  cellH: number;
  maskW: number;
  maskH: number;
  /** 帧号 = row * 8 + col；每帧按行优先、低位在前打包 */
  masks: number[][];
  footRatio: number;
  centerRatio: number;
  /** 头顶位置，气泡贴着它放 */
  topRatio: number;
  /** 皮肤有"坐"的动画 */
  canSit: boolean;
}

export function computeMetrics(skin: LoadedSkin): SkinMetrics {
  const canvas = new OffscreenCanvas(skin.sheetW, skin.sheetH);
  const ctx = canvas.getContext("2d", { willReadFrequently: true })!;
  ctx.drawImage(skin.sheet, 0, 0);
  const { data } = ctx.getImageData(0, 0, skin.sheetW, skin.sheetH);
  const alpha = (x: number, y: number) => data[(y * skin.sheetW + x) * 4 + 3];

  const cw = Math.floor(skin.cellW);
  const ch = Math.floor(skin.cellH);
  const maskW = Math.ceil(cw / DOWNSAMPLE);
  const maskH = Math.ceil(ch / DOWNSAMPLE);

  const masks: number[][] = [];
  for (let row = 0; row < skin.rows; row++) {
    for (let col = 0; col < COLS; col++) {
      const bits = new Array<number>(Math.ceil((maskW * maskH) / 8)).fill(0);
      const ox = Math.floor(col * skin.cellW);
      const oy = Math.floor(row * skin.cellH);
      for (let my = 0; my < maskH; my++) {
        for (let mx = 0; mx < maskW; mx++) {
          let solid = false;
          for (let y = my * DOWNSAMPLE; y < Math.min(ch, (my + 1) * DOWNSAMPLE) && !solid; y++) {
            for (let x = mx * DOWNSAMPLE; x < Math.min(cw, (mx + 1) * DOWNSAMPLE); x++) {
              if (alpha(ox + x, oy + y) >= ALPHA_MIN) {
                solid = true;
                break;
              }
            }
          }
          if (solid) {
            const i = my * maskW + mx;
            bits[i >> 3] |= 1 << (i & 7);
          }
        }
      }
      masks.push(bits);
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
    cellW: skin.cellW,
    cellH: skin.cellH,
    maskW,
    maskH,
    masks,
    footRatio: maxY >= 0 ? (maxY + 1) / ch : 1,
    centerRatio: maxX >= 0 ? (minX + maxX + 1) / 2 / cw : 0.5,
    topRatio: maxY >= 0 ? minY / ch : 0,
    canSit: skin.canSit,
  };
}
