// 生成 Perch 默认皮肤「栖栖」：用代码画小鸟的各个部件，按参数摆出每一帧，渲染成 Codex 宠物格式的精灵图。
// 造型和配色参考软件图标（src-tauri/icons/source.png）。格式见 docs/skin-format.md。
// 用法：pnpm skin:bird            → 写到 src-tauri/resources/skins/perch-bird/
//       pnpm skin:bird <输出目录>
import { Resvg } from "@resvg/resvg-js";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = resolve(process.argv[2] ?? join(here, "../../src-tauri/resources/skins/perch-bird"));

// 图集：8 列 × 12 行，每格 192×208；脚底基线在格子里的 y=196，水平中心 x=96。
// 前 11 行是 Codex v2 的布局，第 12 行（坐）是 Perch 的扩展，靠 skin.json 的 rows: 12 声明
const CW = 192;
const CH = 208;
const COLS = 8;
const ROWS = 12;
const BASE_Y = 196;

// 配色（取自图标）
const C = {
  line: "#1a2032",
  back: "#425a82",
  wing: "#405078",
  wingLight: "#7088c0",
  belly: "#f2eae2",
  bellyShade: "#d4d3db",
  tail: "#384058",
  beak: "#3a4058",
  eye: "#181828",
  key: "#2b3048",
  keyTop: "#5b6488",
  keyLit: "#9fb4e8",
  fx: "#425a82",
};
const LW = 3.4; // 描边宽度

const r1 = (v) => Math.round(v * 10) / 10;

/**
 * 画一只小鸟（朝右），原点在两脚中间的地面上，y 向下为正。
 * 参数都有默认值，每一帧只改需要的几个。
 */
function bird(p = {}) {
  const q = {
    dy: 0, // 整体上下移动（跳、蹦）
    sx: 1, // 压扁/拉长（以脚为基准）
    sy: 1,
    lean: 0, // 整体前倾（度，正数向前低头）
    tilt: 0, // 头部歪头：整只鸟绕身体中心小幅转动
    wing: 0, // 翅膀角度（度，负数向上抬）
    wingOpen: 0, // 0 收拢 ~ 1 完全展开（飞行）
    crest: 0, // 头冠 0 平 ~ 1 竖起
    eye: "open", // open | half | closed | happy | x | wide
    beak: 0, // 0 闭 ~ 1 张开
    puff: 0, // 0 正常 ~ 1 羽毛炸开
    feet: 1, // 0 收起（空中）~ 1 站立
    tuck: 0, // 0 正常 ~ 1 把头埋起来睡觉
    tailDroop: 0, // 尾巴往下垂的角度（度）：坐在窗沿上时尾巴搭在边沿外
    ...p,
  };
  const puffS = 1 + q.puff * 0.2;
  const bodyRx = 44 * puffS;
  const bodyRy = 41 * puffS;
  const cy = -58 - q.puff * 6;
  // 脚单独放：前倾、歪头时脚留在地上，不跟着身体转
  const feet = [];
  const parts = [];

  // 脚（在身体后面画）
  if (q.feet > 0) {
    const leg = 16 * q.feet;
    for (const x of [-7, 11]) {
      feet.push(
        `<path d="M ${x} ${-17 - (1 - q.feet) * 6} L ${x} ${r1(-17 + leg)} M ${x - 7} 0 Q ${x - 2} -5 ${x} ${r1(-17 + leg)} Q ${x + 3} -4 ${x + 8} 0" fill="none" stroke="${C.line}" stroke-width="${LW + 1.6}" stroke-linecap="round" stroke-linejoin="round"/>`,
        `<path d="M ${x} ${-17 - (1 - q.feet) * 6} L ${x} ${r1(-17 + leg)} M ${x - 7} 0 Q ${x - 2} -5 ${x} ${r1(-17 + leg)} Q ${x + 3} -4 ${x + 8} 0" fill="none" stroke="${C.tail}" stroke-width="${LW - 0.6}" stroke-linecap="round" stroke-linejoin="round"/>`,
      );
    }
  } else {
    // 空中：脚收在肚子下面
    parts.push(`<path d="M -6 ${cy + bodyRy - 4} q 4 6 9 0 M 9 ${cy + bodyRy - 4} q 4 6 9 0" fill="none" stroke="${C.line}" stroke-width="${LW}" stroke-linecap="round"/>`);
  }

  // 尾巴：下垂时绕尾巴根部转下去，并稍微缩短，免得尾巴尖超出格子
  const tailTf = q.tailDroop ? `rotate(${r1(-q.tailDroop)} -27 ${cy + 22}) translate(-27 ${cy + 22}) scale(0.85) translate(27 ${-(cy + 22)})` : "";
  parts.push(
    `<path transform="${tailTf}" d="M -26 ${cy + 12} Q -58 ${cy + 16} -88 ${cy + 34} Q -58 ${cy + 40} -28 ${cy + 34} Z" fill="${C.tail}" stroke="${C.line}" stroke-width="${LW}" stroke-linejoin="round"/>`,
  );

  // 头冠（在身体后面，根部藏进头里）
  const crestRot = -18 * q.crest;
  const crestS = 1 + 0.35 * q.crest;
  parts.push(
    `<g transform="translate(-4 ${cy - bodyRy + 8}) rotate(${r1(crestRot)}) scale(1 ${r1(crestS)})">` +
      `<path d="M -22 8 Q -26 -8 -36 -20 Q -18 -16 -10 -4 Q -12 -20 -6 -34 Q 8 -18 10 6 Z" fill="${C.back}" stroke="${C.line}" stroke-width="${LW}" stroke-linejoin="round"/>` +
      `</g>`,
  );

  // 身体（头身一体的圆团）
  const bodyPath = `M 0 ${cy - bodyRy} A ${bodyRx} ${bodyRy} 0 1 1 0 ${cy + bodyRy} A ${bodyRx} ${bodyRy} 0 1 1 0 ${cy - bodyRy} Z`;
  parts.push(`<clipPath id="CLIP"><path d="${bodyPath}"/></clipPath>`);
  parts.push(`<path d="${bodyPath}" fill="${C.back}"/>`);
  // 米白色的脸和肚子 + 肚子下沿的浅灰
  parts.push(
    `<g clip-path="url(#CLIP)">` +
      `<ellipse cx="${r1(16 * puffS)}" cy="${r1(cy + 6)}" rx="${r1(34 * puffS)}" ry="${r1(37 * puffS)}" fill="${C.belly}"/>` +
      `<ellipse cx="${r1(6 * puffS)}" cy="${r1(cy + bodyRy - 4)}" rx="${r1(34 * puffS)}" ry="${r1(12 * puffS)}" fill="${C.bellyShade}"/>` +
      `</g>`,
  );
  // 炸毛：身体边缘冒出几撮羽毛
  if (q.puff > 0.3) {
    const tufts = [
      [-40, -34],
      [-46, 4],
      [-30, 36],
      [30, 38],
      [44, -8],
      [26, -42],
    ];
    for (const [tx, ty] of tufts) {
      const ang = Math.atan2(ty, tx);
      const bx = Math.cos(ang) * bodyRx * 0.96;
      const by = cy + Math.sin(ang) * bodyRy * 0.96;
      const ox = Math.cos(ang) * 12 * q.puff;
      const oy = Math.sin(ang) * 12 * q.puff;
      const px = -Math.sin(ang) * 6;
      const py = Math.cos(ang) * 6;
      const fill = ty > 0 && tx > -10 ? C.belly : C.back;
      parts.push(
        `<path d="M ${r1(bx - px)} ${r1(by - py)} L ${r1(bx + ox)} ${r1(by + oy)} L ${r1(bx + px)} ${r1(by + py)} Z" fill="${fill}" stroke="${C.line}" stroke-width="${LW - 0.8}" stroke-linejoin="round"/>`,
      );
    }
  }
  parts.push(`<path d="${bodyPath}" fill="none" stroke="${C.line}" stroke-width="${LW}"/>`);

  // 眼睛
  const ex = 20 + q.puff * 4;
  const ey = cy - 8 + q.tuck * 6;
  const eyeStroke = `fill="none" stroke="${C.eye}" stroke-width="${LW}" stroke-linecap="round"`;
  switch (q.eye) {
    case "open":
    case "wide": {
      const rr = q.eye === "wide" ? 8.4 : 7;
      parts.push(`<ellipse cx="${ex}" cy="${ey}" rx="${rr - 0.8}" ry="${rr}" fill="${C.eye}"/>`);
      parts.push(`<circle cx="${ex + 2.2}" cy="${ey - 2.6}" r="${q.eye === "wide" ? 2.8 : 2.3}" fill="#ffffff"/>`);
      break;
    }
    case "half":
      parts.push(`<ellipse cx="${ex}" cy="${ey + 1.5}" rx="6.2" ry="3.4" fill="${C.eye}"/>`);
      break;
    case "closed":
      parts.push(`<path d="M ${ex - 7} ${ey} Q ${ex} ${ey + 5} ${ex + 7} ${ey}" ${eyeStroke}/>`);
      break;
    case "happy":
      parts.push(`<path d="M ${ex - 7} ${ey + 2} Q ${ex} ${ey - 6} ${ex + 7} ${ey + 2}" ${eyeStroke}/>`);
      break;
    case "x":
      parts.push(`<path d="M ${ex - 6} ${ey - 6} L ${ex + 6} ${ey + 6} M ${ex + 6} ${ey - 6} L ${ex - 6} ${ey + 6}" ${eyeStroke}/>`);
      break;
  }

  // 喙
  const bx = 41 + q.puff * 8 - q.tuck * 4;
  const by = cy - 4 + q.tuck * 10;
  const open = q.beak * 7;
  const beakS = 1 - q.tuck * 0.35;
  parts.push(
    `<g transform="translate(${r1(bx)} ${r1(by)}) scale(${r1(beakS)})">` +
      `<path d="M -2 -6 L 15 ${r1(-1 - open * 0.3)} L -1 ${r1(2 - open * 0.2)} Z" fill="${C.beak}" stroke="${C.line}" stroke-width="${LW - 0.6}" stroke-linejoin="round"/>` +
      (q.beak > 0.05
        ? `<path d="M -1 ${r1(3 + open * 0.2)} L 11 ${r1(4 + open)} L -2 ${r1(7 + open * 0.4)} Z" fill="${C.beak}" stroke="${C.line}" stroke-width="${LW - 0.6}" stroke-linejoin="round"/>`
        : "") +
      `</g>`,
  );

  // 翅膀：以肩膀为轴转动；展开时变长变宽
  const wingS = 0.86 + q.wingOpen * 0.5;
  const wingAng = q.wing - q.wingOpen * 10;
  const sh = [-10, cy - 10];
  parts.push(
    `<g transform="translate(${sh[0]} ${r1(sh[1])}) rotate(${r1(-wingAng)}) scale(${r1(wingS)} ${r1(0.92 + q.wingOpen * 0.2)})">` +
      `<path d="M 6 -8 Q 18 12 0 30 Q -24 40 -52 30 Q -34 -2 6 -8 Z" fill="${C.wing}" stroke="${C.line}" stroke-width="${r1(LW / wingS)}" stroke-linejoin="round"/>` +
      `<path d="M 2 -1 Q 10 12 -4 23 Q -22 29 -40 25 Q -24 4 2 -1 Z" fill="${C.wingLight}"/>` +
      `</g>`,
  );

  // 整体变换：先压扁拉长（以脚为基准），再前倾（以脚为轴）/歪头（以身体中心为轴），最后上下移动；脚只跟着上下移动和压扁
  const center = cy + 6;
  const squash = `scale(${q.sx} ${q.sy})`;
  const tf = `rotate(${r1(q.lean)} 0 -6) rotate(${r1(q.tilt)} 0 ${r1(center)}) ${squash}`;
  return `<g transform="translate(0 ${r1(q.dy)})"><g transform="${squash}">${feet.join("")}</g><g transform="${tf}">${parts.join("")}</g></g>`;
}

// ---------- 道具和特效（相对鸟的坐标系） ----------

/** 问号，(x, y) 是问号中心 */
const question = (x, y, s = 1) =>
  `<g transform="translate(${r1(x)} ${r1(y)}) scale(${s})">` +
  `<path d="M -7 -8 Q -7 -17 1 -17 Q 9 -17 9 -9 Q 9 -3 2 0 L 2 5" fill="none" stroke="${C.line}" stroke-width="7.5" stroke-linecap="round" stroke-linejoin="round"/>` +
  `<path d="M -7 -8 Q -7 -17 1 -17 Q 9 -17 9 -9 Q 9 -3 2 0 L 2 5" fill="none" stroke="${C.wingLight}" stroke-width="3.6" stroke-linecap="round" stroke-linejoin="round"/>` +
  `<circle cx="2" cy="13" r="3.8" fill="${C.line}"/><circle cx="2" cy="13" r="2" fill="${C.wingLight}"/></g>`;

/** 感叹号 */
const bang = (x, y, s = 1) =>
  `<g transform="translate(${r1(x)} ${r1(y)}) scale(${s})">` +
  `<path d="M 0 -16 L 0 3" stroke="${C.line}" stroke-width="8" stroke-linecap="round"/><path d="M 0 -16 L 0 3" stroke="#f0b34a" stroke-width="4" stroke-linecap="round"/>` +
  `<circle cx="0" cy="12" r="4.2" fill="${C.line}"/><circle cx="0" cy="12" r="2.2" fill="#f0b34a"/></g>`;

/** 睡觉的 z */
const zee = (x, y, s = 1, a = 1) =>
  `<path transform="translate(${r1(x)} ${r1(y)}) scale(${s})" d="M -5 -5 L 5 -5 L -5 5 L 5 5" fill="none" stroke="${C.fx}" stroke-opacity="${a}" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/>`;

/** 头晕的圈圈 */
const dizzy = (x, y, rot = 0) =>
  `<path transform="translate(${x} ${y}) rotate(${rot})" d="M 0 0 m -12 0 a 12 6 0 1 0 24 0 a 10 5 0 1 0 -20 0 a 7 3.5 0 1 0 14 0" fill="none" stroke="${C.line}" stroke-width="3" stroke-linecap="round"/>`;

/** 声波弧线（在听你说话） */
const waves = (x, y, n) => {
  let s = "";
  for (let i = 0; i < n; i++) {
    const r = 7 + i * 6;
    s += `<path d="M ${x + r * 0.5} ${y - r} Q ${x + r * 1.1} ${y} ${x + r * 0.5} ${y + r}" fill="none" stroke="${C.fx}" stroke-width="3" stroke-linecap="round"/>`;
  }
  return s;
};

/** 小键盘，lit 是被啄亮的键 */
const keyboard = (lit = -1) => {
  let s = `<rect x="40" y="-15" width="50" height="15" rx="4" fill="${C.key}" stroke="${C.line}" stroke-width="${LW - 0.6}"/>`;
  for (let i = 0; i < 5; i++) {
    s += `<rect x="${44.5 + i * 8.8}" y="-11.5" width="6.5" height="4" rx="1.2" fill="${i === lit ? C.keyLit : C.keyTop}"/>`;
  }
  s += `<rect x="49" y="-5.5" width="32" height="3.5" rx="1.2" fill="${C.keyTop}"/>`;
  return s;
};

// ---------- 每一行的帧 ----------

const hop = (i) => {
  // 蹦一下：蹲 → 起 → 空中 → 落 → 蹲
  const dy = [0, -6, -14, -17, -13, -6, 0, 0][i];
  const sq = [0.9, 1.06, 1.02, 1, 1.02, 1.05, 0.9, 1][i];
  return bird({ dy, sy: sq, sx: 2 - sq, feet: dy < -10 ? 0.6 : 1, wing: dy < -5 ? -18 : 0, lean: [4, -3, -2, 0, 3, 4, 4, 0][i] });
};

const rows = [
  // 0 idle：呼吸、眨眼、歪头
  [
    bird(),
    bird({ sy: 0.985, eye: "half" }),
    bird({ sy: 0.98, eye: "closed" }),
    bird({ sy: 0.985, eye: "half" }),
    bird({ sy: 1.01, tilt: -4 }),
    bird({ sy: 1.005, tilt: -2 }),
  ],
  // 1 向右蹦
  [0, 1, 2, 3, 4, 5, 6, 7].map(hop),
  // 2 向左蹦（第 1 行镜像，生成时翻转）
  null,
  // 3 打招呼 / 说话：张嘴、抬翅、头冠抖
  [
    bird({ beak: 1, wing: -22, crest: 0.4 }),
    bird({ beak: 0.2, wing: -6, crest: 0.1 }),
    bird({ beak: 0.9, wing: -28, crest: 0.5, eye: "happy" }),
    bird({ beak: 0, wing: 0, crest: 0.2 }),
  ],
  // 4 起跳 / 空中 / 下落 / 落地
  [
    bird({ sy: 0.86, sx: 1.08, lean: 6 }),
    bird({ sy: 1.08, sx: 0.94, wing: -80, wingOpen: 1, feet: 0.7, lean: -4 }),
    bird({ dy: -4, wing: 18, wingOpen: 1, feet: 0, lean: -2 }),
    bird({ dy: -2, wing: -55, wingOpen: 0.9, feet: 0.5, lean: 6 }),
    bird({ sy: 0.9, sx: 1.06, wing: -18, wingOpen: 0.3, lean: 2 }),
  ],
  // 5 出错：吓一跳 → 炸毛 → 晃 → 晕
  [
    bird({ eye: "wide", crest: 1 }),
    bird({ dy: -6, eye: "wide", crest: 1, puff: 0.3, feet: 0.8 }),
    bird({ eye: "wide", crest: 1, puff: 0.6 }),
    bird({ eye: "wide", crest: 1, puff: 0.9 }),
    bird({ eye: "x", crest: 0.8, puff: 1, tilt: -7 }),
    bird({ eye: "x", crest: 0.8, puff: 1, tilt: 7 }),
    bird({ eye: "x", crest: 0.6, puff: 1, tilt: -3 }) + dizzy(-2, -128, 0),
    bird({ eye: "x", crest: 0.5, puff: 1 }) + dizzy(-2, -128, 20),
  ],
  // 6 等待授权：头冠竖起、蹦一下、冒感叹号
  [
    bird({ crest: 1, eye: "wide" }),
    bird({ crest: 1, eye: "wide", sy: 0.94, sx: 1.04 }) + bang(40, -122, 0.8),
    bird({ crest: 1, eye: "wide", dy: -6 }) + bang(40, -128, 1),
    bird({ crest: 1, eye: "wide" }) + bang(40, -124, 1),
    bird({ crest: 1, eye: "half", tilt: -3 }) + bang(40, -124, 1),
    bird({ crest: 0.8, eye: "wide" }) + bang(40, -122, 0.9),
  ],
  // 7 工作中：啄键盘
  [
    keyboard() + bird({ lean: 4 }),
    keyboard() + bird({ lean: 20, wing: -8 }),
    keyboard(2) + bird({ lean: 36, wing: -14, eye: "closed" }),
    keyboard() + bird({ lean: 8, crest: 0.3 }),
    keyboard(4) + bird({ lean: 36, wing: -14, eye: "closed" }),
    keyboard() + bird({ lean: 6 }),
  ],
  // 8 思考：歪头、问号一跳一跳
  [
    bird({ tilt: -10 }) + question(30, -128, 0.9),
    bird({ tilt: -11 }) + question(30, -132, 0.95),
    bird({ tilt: -12, eye: "half" }) + question(30, -134, 1),
    bird({ tilt: -12 }) + question(30, -132, 1),
    bird({ tilt: -11, crest: 0.3 }) + question(30, -130, 0.95),
    bird({ tilt: -10 }) + question(30, -128, 0.9),
  ],
  // 9 睡觉：把头埋起来鼓成一团，z 往上飘
  [0, 1, 2, 3, 4, 5].map((i) => {
    const br = [0, 0.5, 1, 1, 0.5, 0][i];
    const zs = [
      [[34, -96, 0.7, 1]],
      [[36, -102, 0.8, 1]],
      [[38, -108, 0.9, 0.9], [26, -92, 0.6, 1]],
      [[40, -114, 1, 0.7], [28, -98, 0.7, 1]],
      [[30, -104, 0.8, 1]],
      [[32, -100, 0.75, 1]],
    ][i];
    return (
      bird({ eye: "closed", tuck: 1, puff: 0.15 + br * 0.08, sy: 0.92 + br * 0.03, sx: 1.06, wing: -10, wingOpen: 0.15, crest: 0 }) +
      zs.map(([x, y, s, a]) => zee(x, y, s, a)).join("")
    );
  }),
  // 10 在听你说话：微微歪头，声波一圈圈出现
  [0, 1, 2, 3, 4, 5].map((i) => bird({ tilt: -5, crest: 0.4, eye: i === 3 ? "half" : "open" }) + waves(54, -76, [1, 2, 3, 1, 2, 3][i])),
  // 11 坐在窗沿上：肚子贴着边沿（基线），脚收起来，尾巴搭在边沿外轻轻晃，偶尔眨眼、歪头
  [0, 1, 2, 3, 4, 5].map((i) =>
    bird({
      dy: 13,
      feet: 0,
      sx: 1.06,
      sy: [0.9, 0.895, 0.9, 0.9, 0.905, 0.9][i],
      tailDroop: [8, 10, 12, 12, 10, 8][i],
      wing: -4,
      eye: ["open", "open", "half", "closed", "open", "open"][i],
      tilt: [0, 0, 0, 0, -6, -4][i],
    }),
  ),
];
rows[2] = rows[1].map((f) => `<g transform="scale(-1 1)">${f}</g>`);

// ---------- 拼成图集 ----------

let cells = "";
rows.forEach((frames, r) => {
  frames.forEach((f, c) => {
    // 每格单独一个 clipPath id，避免互相引用
    const id = `c${r}_${c}`;
    cells += `<g transform="translate(${c * CW + CW / 2} ${r * CH + BASE_Y})">${f.replaceAll("CLIP", id)}</g>`;
  });
});
const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${CW * COLS}" height="${CH * ROWS}" viewBox="0 0 ${CW * COLS} ${CH * ROWS}">${cells}</svg>`;

const png = new Resvg(svg, { background: "rgba(0,0,0,0)" }).render().asPng();
mkdirSync(outDir, { recursive: true });
writeFileSync(join(outDir, "spritesheet.png"), png);
writeFileSync(
  join(outDir, "pet.json"),
  JSON.stringify(
    {
      id: "perch-bird",
      displayName: "栖栖",
      description: "Perch 的默认形象：一只停在窗沿上的蓝白小鸟。",
      spritesheetPath: "spritesheet.png",
      spriteVersionNumber: 2,
    },
    null,
    2,
  ) + "\n",
);
// 第 9～11 行是自己加的：睡觉、听你说话、坐在窗沿上（rows: 12 是 Perch 的扩展，声明图集有 12 行）
writeFileSync(
  join(outDir, "skin.json"),
  JSON.stringify(
    {
      rows: 12,
      animations: {
        sleep: { row: 9, frames: [0, 1, 2, 3, 4, 5], durations: [520, 480, 600, 600, 480, 520], loop: true },
        listen: { row: 10, frames: [0, 1, 2, 3, 4, 5], durations: [200, 200, 260, 200, 200, 260], loop: true },
        sit: { row: 11, frames: [0, 1, 2, 3, 4, 5], durations: [900, 900, 110, 110, 1100, 900], loop: true },
      },
    },
    null,
    2,
  ) + "\n",
);
console.log(`已生成 ${outDir}（${CW * COLS}×${CH * ROWS}，${png.length} 字节）`);
