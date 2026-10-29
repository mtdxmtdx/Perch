// Codex 宠物图集：8 列；v1 为 9 行（1536×1872），v2 为 11 行（多出的 2 行是看向各方向，不用）。
// 行与帧时长来自 awesome-codex-pet 的 hatch-pet-v1/references/animation-rows.md。

import { invoke } from "@tauri-apps/api/core";
import { makePlaceholderSheet } from "./placeholder";

export const COLS = 8;

const repeat = (ms: number, n: number, last: number) => [...Array(n - 1).fill(ms), last];

/** 每行各帧的时长（ms），帧数即数组长度 */
export const CODEX_ROWS: { name: string; durations: number[] }[] = [
  { name: "idle", durations: [280, 110, 110, 140, 140, 320] },
  { name: "running-right", durations: repeat(120, 8, 220) },
  { name: "running-left", durations: repeat(120, 8, 220) },
  { name: "waving", durations: repeat(140, 4, 280) },
  { name: "jumping", durations: repeat(140, 5, 280) },
  { name: "failed", durations: repeat(140, 8, 240) },
  { name: "waiting", durations: repeat(150, 6, 260) },
  { name: "running", durations: repeat(120, 6, 220) },
  { name: "review", durations: repeat(150, 6, 280) },
];

export interface AnimSpec {
  row: number;
  frames: number[];
  durations: number[];
  /** false 时播完停在最后一帧 */
  loop: boolean;
}

export interface LoadedSkin {
  id: string;
  name: string;
  sheet: CanvasImageSource;
  sheetW: number;
  sheetH: number;
  rows: number;
  cellW: number;
  cellH: number;
  anims: Record<string, AnimSpec>;
  /** skin.json 里定义了"坐"的动画（sit）：有才会坐在窗沿上 */
  canSit: boolean;
}

function fullRow(row: number, loop = true, speed = 1): AnimSpec {
  const durations = CODEX_ROWS[row].durations.map((d) => d / speed);
  return { row, frames: durations.map((_, i) => i), durations, loop };
}

function pick(row: number, frames: number[], loop = false): AnimSpec {
  return { row, frames, durations: frames.map((f) => CODEX_ROWS[row].durations[f]), loop };
}

/** 桌宠状态 → 图集动画的默认映射，skin.json 可以逐项覆盖 */
export function defaultAnims(): Record<string, AnimSpec> {
  return {
    idle: fullRow(0),
    walk_right: fullRow(1),
    walk_left: fullRow(2),
    wave: fullRow(3),
    jump: pick(4, [1, 2]),
    fall: pick(4, [3]),
    land: pick(4, [4]),
    drag: pick(4, [2]),
    sleep: fullRow(6, true, 0.4),
    // 坐在窗沿上：Codex 图集没有这一行，皮肤要在 skin.json 里自己指定；没指定的皮肤不会坐
    sit: fullRow(0),
    // 以下给后续里程碑用（对话、agent 状态）
    talk: fullRow(3),
    think: fullRow(8),
    working: fullRow(7),
    attention: fullRow(6),
    // 录音中（在听你说话）
    listen: fullRow(6, true, 0.7),
    error: fullRow(5, false),
    done: pick(4, [0, 1, 2, 3, 4]),
  };
}

interface AnimOverride {
  row?: number;
  frames?: number[];
  durations?: number[];
  loop?: boolean;
  speed?: number;
}

function applyOverrides(anims: Record<string, AnimSpec>, overrides: unknown, rows: number) {
  const table = (overrides as { animations?: Record<string, AnimOverride> } | null)?.animations;
  if (!table) return;
  for (const [name, o] of Object.entries(table)) {
    const row = o.row ?? anims[name]?.row;
    if (row === undefined || row < 0 || row >= rows) continue;
    const base = CODEX_ROWS[row]?.durations ?? repeat(150, COLS, 150);
    const frames = o.frames ?? base.map((_, i) => i);
    const speed = o.speed ?? 1;
    const durations = (o.durations ?? frames.map((f) => base[f] ?? 150)).map((d) => d / speed);
    anims[name] = { row, frames, durations, loop: o.loop ?? anims[name]?.loop ?? true };
  }
}

interface SkinInfo {
  id: string;
  name: string;
  pet: { spriteVersionNumber?: number };
  overrides: unknown;
}

export async function loadCurrentSkin(): Promise<LoadedSkin> {
  const info = await invoke<SkinInfo | null>("skin_info");
  if (!info) {
    const sheet = makePlaceholderSheet();
    return {
      id: "placeholder",
      name: "占位",
      sheet,
      sheetW: sheet.width,
      sheetH: sheet.height,
      rows: 9,
      cellW: sheet.width / COLS,
      cellH: sheet.height / 9,
      anims: defaultAnims(),
      canSit: false,
    };
  }
  const buf = await invoke<ArrayBuffer>("skin_image");
  const bmp = await createImageBitmap(new Blob([buf]));
  // Perch 自己的扩展：skin.json 可以用 rows 声明更多行（比如多一行"坐"），否则按 Codex 规定 9 行（v2 为 11 行）
  const extra = info.overrides as { rows?: unknown; animations?: Record<string, unknown> } | null;
  const declared = typeof extra?.rows === "number" && Number.isInteger(extra.rows) && extra.rows >= 9 && extra.rows <= 32 ? extra.rows : null;
  const rows = declared ?? (info.pet.spriteVersionNumber === 2 ? 11 : 9);
  const anims = defaultAnims();
  applyOverrides(anims, info.overrides, rows);
  return {
    id: info.id,
    name: info.name,
    sheet: bmp,
    sheetW: bmp.width,
    sheetH: bmp.height,
    rows,
    cellW: bmp.width / COLS,
    cellH: bmp.height / rows,
    anims,
    canSit: !!extra?.animations?.sit,
  };
}
