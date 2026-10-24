import type { Renderer } from "./Renderer";
import { COLS, type AnimSpec, type LoadedSkin } from "../skin/codexPet";

export class SpriteRenderer implements Renderer {
  private canvas?: HTMLCanvasElement;
  private ctx?: CanvasRenderingContext2D;
  private skin?: LoadedSkin;
  private animName = "";
  private spec?: AnimSpec;
  private step = 0;
  private timer?: number;
  private frameCb?: (i: number) => void;

  mount(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d")!;
    this.resize();
  }

  async load(skin: LoadedSkin) {
    this.skin = skin;
    // 换皮肤后继续播放当前状态
    const current = this.animName || "idle";
    this.animName = "";
    this.play(current);
  }

  play(anim: string) {
    if (!this.skin || anim === this.animName) return;
    const spec = this.skin.anims[anim] ?? this.skin.anims.idle;
    this.animName = anim;
    this.spec = spec;
    this.step = 0;
    this.schedule();
  }

  onFrame(cb: (frameIndex: number) => void) {
    this.frameCb = cb;
  }

  resize() {
    if (!this.canvas) return;
    const dpr = window.devicePixelRatio || 1;
    this.canvas.width = Math.round(window.innerWidth * dpr);
    this.canvas.height = Math.round(window.innerHeight * dpr);
    this.draw();
  }

  destroy() {
    clearTimeout(this.timer);
  }

  private schedule() {
    clearTimeout(this.timer);
    this.draw();
    const spec = this.spec;
    if (!spec) return;
    const atEnd = this.step >= spec.frames.length - 1;
    if (atEnd && !spec.loop) return;
    this.timer = window.setTimeout(() => {
      this.step = atEnd ? 0 : this.step + 1;
      this.schedule();
    }, spec.durations[this.step] ?? 150);
  }

  private draw() {
    const { ctx, canvas, skin, spec } = this;
    if (!ctx || !canvas || !skin || !spec) return;
    const col = spec.frames[this.step] ?? 0;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(
      skin.sheet,
      col * skin.cellW,
      spec.row * skin.cellH,
      skin.cellW,
      skin.cellH,
      0,
      0,
      canvas.width,
      canvas.height,
    );
    this.frameCb?.(spec.row * COLS + col);
  }
}
