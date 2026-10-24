import type { LoadedSkin } from "../skin/codexPet";

/** 渲染后端的统一接口。第一版是精灵图集，以后 Live2D 实现同一接口。 */
export interface Renderer {
  mount(canvas: HTMLCanvasElement): void;
  load(skin: LoadedSkin): Promise<void>;
  /** 播放某个桌宠状态的动画；重复调用同一个名字不会重新开始 */
  play(anim: string): void;
  /** 每次换帧时回调图集里的帧号（row * 8 + col），供鼠标穿透判定使用 */
  onFrame(cb: (frameIndex: number) => void): void;
  resize(): void;
  destroy(): void;
}
