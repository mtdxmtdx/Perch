// 找不到任何皮肤时用的占位形象：程序画的小团子，按 Codex 图集布局生成。

import { CODEX_ROWS, COLS } from "./codexPet";

const CW = 192;
const CH = 208;

function drawBlob(ctx: CanvasRenderingContext2D, ox: number, oy: number, row: number, f: number) {
  const n = CODEX_ROWS[row].durations.length;
  const t = f / n;
  let dy = Math.sin(t * Math.PI * 2) * 4;
  let lean = 0;
  let eyes: "open" | "closed" | "x" = "open";
  if (row === 1 || row === 7) lean = 6;
  if (row === 2) lean = -6;
  if (row === 4) dy = [0, -20, -36, -18, 4][f] ?? 0;
  if (row === 5) eyes = "x";
  if (row === 6 && f % 3 === 2) eyes = "closed";

  const cx = ox + CW / 2 + lean;
  const cy = oy + CH - 70 + dy;
  ctx.fillStyle = "#ffb74d";
  ctx.strokeStyle = "#5c4033";
  ctx.lineWidth = 4;
  ctx.beginPath();
  ctx.ellipse(cx, cy, 58, 52, 0, 0, Math.PI * 2);
  ctx.fill();
  ctx.stroke();
  // 耳朵
  for (const s of [-1, 1]) {
    ctx.beginPath();
    ctx.moveTo(cx + s * 22, cy - 46);
    ctx.lineTo(cx + s * 44, cy - 74);
    ctx.lineTo(cx + s * 50, cy - 36);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();
  }
  ctx.fillStyle = "#5c4033";
  for (const s of [-1, 1]) {
    const ex = cx + s * 20 + lean / 2;
    const ey = cy - 6;
    if (eyes === "open") {
      ctx.beginPath();
      ctx.arc(ex, ey, 6, 0, Math.PI * 2);
      ctx.fill();
    } else if (eyes === "closed") {
      ctx.fillRect(ex - 7, ey - 1, 14, 3);
    } else {
      ctx.beginPath();
      ctx.moveTo(ex - 6, ey - 6);
      ctx.lineTo(ex + 6, ey + 6);
      ctx.moveTo(ex + 6, ey - 6);
      ctx.lineTo(ex - 6, ey + 6);
      ctx.stroke();
    }
  }
  // 挥手
  if (row === 3) {
    ctx.fillStyle = "#ffb74d";
    ctx.beginPath();
    ctx.ellipse(cx + 62, cy - 30 - (f % 2) * 10, 14, 18, 0.4, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
  }
}

export function makePlaceholderSheet(): HTMLCanvasElement {
  const canvas = document.createElement("canvas");
  canvas.width = CW * COLS;
  canvas.height = CH * CODEX_ROWS.length;
  const ctx = canvas.getContext("2d")!;
  CODEX_ROWS.forEach((r, row) => {
    for (let f = 0; f < r.durations.length; f++) drawBlob(ctx, f * CW, row * CH, row, f);
  });
  return canvas;
}
