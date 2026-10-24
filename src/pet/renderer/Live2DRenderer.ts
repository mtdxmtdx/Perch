import type { Renderer } from "./Renderer";
import type { LoadedSkin } from "../skin/codexPet";

/** 【留接口】Live2D 渲染后端，后续开发。需要同样实现 onFrame/掩码，才能做鼠标穿透。 */
export class Live2DRenderer implements Renderer {
  mount(_canvas: HTMLCanvasElement) {}
  async load(_skin: LoadedSkin) {
    throw new Error("Live2D 渲染尚未实现");
  }
  play(_anim: string) {}
  onFrame(_cb: (frameIndex: number) => void) {}
  resize() {}
  destroy() {}
}
