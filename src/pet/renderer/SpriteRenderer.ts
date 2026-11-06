import type { Facing, HitUpdate, LoadResult, Renderer } from "./Renderer";
import type { SkinSource } from "../skin/source";
import { COLS, loadCodexSkin, type AnimSpec, type LoadedSkin } from "../skin/codexPet";
import { LiveMask, spriteMasks } from "../masks";

/**
 * 内置渲染器：Codex 格式的精灵图集，画在 2D 画布上。
 * 鼠标穿透默认预先算好每帧掩码；skin.json 写 "hitTest": "live" 时改为每次换帧从画面读。
 */
/** 这几个状态只有一行画面，方向相反时要镜像；走路左右各有一行，不用 */
const AIR_ANIMS = new Set(["jump", "fall", "land"]);

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
  private facing?: Facing;

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

  face(dir: Facing) {
    this.facing = dir;
    if (AIR_ANIMS.has(this.animName)) this.draw();
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
    const mirror = !!skin.airFacing && !!this.facing && skin.airFacing !== this.facing && AIR_ANIMS.has(this.animName);
    ctx.setTransform(mirror ? -1 : 1, 0, 0, 1, mirror ? canvas.width : 0, 0);
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
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    if (this.live) {
      const mask = this.live.sample(canvas);
      if (mask) this.hitCb?.({ mask });
    } else {
      this.hitCb?.({ frame: spec.row * COLS + col });
    }
  }
}
