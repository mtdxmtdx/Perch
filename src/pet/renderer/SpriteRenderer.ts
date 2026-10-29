import type { HitUpdate, LoadResult, Renderer } from "./Renderer";
import type { SkinSource } from "../skin/source";
import { COLS, loadCodexSkin, type AnimSpec, type LoadedSkin } from "../skin/codexPet";
import { LiveMask, spriteMasks } from "../masks";

/**
 * 内置渲染器：Codex 格式的精灵图集，画在 2D 画布上。
 * 鼠标穿透默认预先算好每帧掩码；skin.json 写 "hitTest": "live" 时改为每次换帧从画面读。
 */
export class SpriteRenderer implements Renderer {
  private canvas?: HTMLCanvasElement;
  private ctx?: CanvasRenderingContext2D;
  private skin?: LoadedSkin;
  private animName = "";
  private spec?: AnimSpec;
  private step = 0;
  private timer?: number;
  private hitCb?: (u: HitUpdate) => void;
  private live?: LiveMask;

  mount(host: HTMLElement) {
    const canvas = document.createElement("canvas");
    canvas.style.cssText = "width:100%;height:100%;display:block";
    host.appendChild(canvas);
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d")!;
    this.resize();
  }

  async load(src: SkinSource): Promise<LoadResult> {
    const skin = await loadCodexSkin(src);
    const { layout, frameMasks } = spriteMasks(skin);
    const liveHit = (src.info?.overrides as { hitTest?: unknown } | null)?.hitTest === "live";
    this.live = liveHit ? new LiveMask(0) : undefined;
    this.skin = skin;
    // 换皮肤后继续播放当前状态
    const current = this.animName || "idle";
    this.animName = "";
    this.play(current);
    return { layout, frameMasks: liveHit ? undefined : frameMasks };
  }

  play(anim: string) {
    if (!this.skin || anim === this.animName) return;
    const spec = this.skin.anims[anim] ?? this.skin.anims.idle;
    this.animName = anim;
    this.spec = spec;
    this.step = 0;
    this.schedule();
  }

  onHit(cb: (u: HitUpdate) => void) {
    this.hitCb = cb;
  }

  resize() {
    if (!this.canvas) return;
    const dpr = window.devicePixelRatio || 1;
    this.canvas.width = Math.round(window.innerWidth * dpr);
    this.canvas.height = Math.round(window.innerHeight * dpr);
    this.live?.reset();
    this.draw();
  }

  destroy() {
    clearTimeout(this.timer);
    this.canvas?.remove();
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
    if (this.live) {
      const mask = this.live.sample(canvas);
      if (mask) this.hitCb?.({ mask });
    } else {
      this.hitCb?.({ frame: spec.row * COLS + col });
    }
  }
}
