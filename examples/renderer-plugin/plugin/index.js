// 示例渲染器插件：用 2D 画布逐帧画一个会呼吸的团子。
// 演示插件的写法：入口函数、登记渲染器、从皮肤文件夹读文件、每帧从画面读掩码做鼠标穿透。
// 真正的插件（比如 Spine）通常用打包工具把依赖打成这样一个 ES 模块文件。

export default function setup(api) {
  api.registerRenderer("blob", () => new BlobRenderer(api));
}

class BlobRenderer {
  constructor(api) {
    this.live = new api.LiveMask(80);
    this.anim = "idle";
    this.color = "#7ec8e3";
  }

  mount(host) {
    this.canvas = document.createElement("canvas");
    this.canvas.style.cssText = "width:100%;height:100%;display:block";
    host.appendChild(this.canvas);
    this.ctx = this.canvas.getContext("2d");
    this.resize();
    const tick = (t) => {
      this.draw(t / 1000);
      this.raf = requestAnimationFrame(tick);
    };
    this.raf = requestAnimationFrame(tick);
  }

  async load(src) {
    // 皮肤文件夹里的 blob.json 可以改颜色
    try {
      const cfg = JSON.parse(new TextDecoder().decode(await src.readFile("blob.json")));
      if (typeof cfg.color === "string") this.color = cfg.color;
    } catch {
      // 没有就用默认颜色
    }
    this.live.reset();
    // 画面 200×200，团子底部在 y=180，水平居中，头顶大约在 y=70
    return { layout: { cellW: 200, cellH: 200, footRatio: 0.9, centerRatio: 0.5, topRatio: 0.35, canSit: true } };
  }

  play(anim) {
    if (anim !== this.anim) {
      this.anim = anim;
      this.changed = true;
    }
  }

  onHit(cb) {
    this.hitCb = cb;
  }

  resize() {
    const dpr = window.devicePixelRatio || 1;
    this.canvas.width = Math.round(window.innerWidth * dpr);
    this.canvas.height = Math.round(window.innerHeight * dpr);
    this.live.reset();
  }

  destroy() {
    cancelAnimationFrame(this.raf);
    this.canvas?.remove();
  }

  draw(t) {
    const { ctx, canvas } = this;
    const s = canvas.width / 200;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.setTransform(s, 0, 0, s, 0, 0);

    // 不同状态的样子：睡觉压扁、坐着更扁更宽、走路左右晃、跳起来离地
    const a = this.anim;
    let w = 58 + Math.sin(t * 2) * 2;
    let h = 52 - Math.sin(t * 2) * 2;
    let x = 100;
    let lift = 0;
    if (a === "sleep") [w, h] = [66, 38];
    if (a === "sit") [w, h] = [68, 40];
    if (a === "walk_left" || a === "walk_right") x += Math.sin(t * 10) * 4;
    if (a === "jump" || a === "fall" || a === "drag") lift = 14;
    // 说话、打招呼时左边伸出一只手：鼠标点得到它，说明掩码是跟着画面走的
    const wave = a === "wave" || a === "talk";

    ctx.fillStyle = this.color;
    ctx.strokeStyle = "#2d4a5a";
    ctx.lineWidth = 4;
    ctx.beginPath();
    ctx.ellipse(x, 180 - h - lift, w, h, 0, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    if (wave) {
      ctx.beginPath();
      ctx.ellipse(x - w - 12, 150 - lift + Math.sin(t * 12) * 8, 14, 10, -0.5, 0, Math.PI * 2);
      ctx.fill();
      ctx.stroke();
    }
    ctx.fillStyle = "#2d4a5a";
    const eyeY = 180 - h - lift - 6;
    for (const dx of [-18, 18]) {
      if (a === "sleep") ctx.fillRect(x + dx - 7, eyeY, 14, 3);
      else {
        ctx.beginPath();
        ctx.arc(x + dx, eyeY, 5, 0, Math.PI * 2);
        ctx.fill();
      }
    }

    // 每帧都试着读一次掩码，LiveMask 自己节流、去重
    const mask = this.live.sample(canvas, this.changed);
    this.changed = false;
    if (mask) this.hitCb?.({ mask });
  }
}
