# 渲染器插件

Perch 自己只会画精灵图集。其他形式的形象（骨骼动画、Live2D、程序生成的……）由**渲染器插件**来画：

- 插件放在 `%APPDATA%\Perch\plugins\<文件夹>\`，提供一个或几个有名字的渲染器。
- 皮肤在 `skin.json` 里写 `"renderer": "<名字>"` 选用。只有选了这个皮肤，Perch 才会去加载对应的插件。
- 程序负责移动、物理、拖拽、气泡和鼠标穿透判定；渲染器只负责把当前状态画出来，并告诉程序角色在画面里的位置。

完整的可运行示例在 [examples/renderer-plugin](../examples/renderer-plugin)：一个程序画的团子，用到了下面所有接口。

> 插件代码在桌宠窗口里运行，能做到桌宠窗口能做的所有事。只安装你信任的插件。

## 安装和试用示例

1. 把 `examples/renderer-plugin/plugin/` 复制成 `%APPDATA%\Perch\plugins\blob\`。
2. 把 `examples/renderer-plugin/skin/` 复制成 `%APPDATA%\Perch\skins\blob\`。
3. 在托盘「皮肤」菜单里选「团子（插件示例）」。

找不到插件、或者插件加载出错时，桌宠会改用占位形象，错误信息写在桌宠窗口的控制台里。插件装好后重新选一次皮肤即可，不用重启。

## 插件文件夹

```
plugins/blob/
├─ plugin.json
└─ index.js        入口，单个 ES 模块文件
```

`plugin.json`：

```json
{
  "id": "perch-blob-demo",
  "name": "团子渲染器（示例）",
  "version": "0.1.0",
  "apiVersion": 1,
  "renderers": ["blob"],
  "main": "index.js"
}
```

| 字段 | 说明 |
|---|---|
| `id` | 插件标识 |
| `name`、`version` | 可选，显示和日志用 |
| `apiVersion` | 插件接口版本，目前是 `1`。和程序要求的不一致时不会加载 |
| `renderers` | 这个插件提供的渲染器名字 |
| `main` | 可选，入口文件，默认 `index.js`，不能指到插件文件夹外面 |

入口文件会被当作一个 ES 模块执行，**不能再 `import` 别的文件**：依赖要用打包工具（esbuild、Vite、Rollup 等）打进同一个文件。

## 入口函数

入口文件默认导出一个函数，程序加载时调用它，传入 `api`：

```js
export default function setup(api) {
  api.registerRenderer("blob", () => new BlobRenderer(api));
}
```

`api` 里有：

| 名字 | 说明 |
|---|---|
| `apiVersion` | 当前接口版本 |
| `registerRenderer(name, create)` | 登记渲染器，`create()` 每次返回一个新的渲染器对象 |
| `anims` | 程序会要求播放的状态名列表 |
| `LiveMask` | 从画面实时读鼠标穿透掩码的工具类，见下文 |
| `packMask(w, h, alphaAt, downsample)` | 自己有像素数据时，用它打包成掩码 |

## 渲染器对象

TypeScript 定义在 [src/pet/renderer/Renderer.ts](../src/pet/renderer/Renderer.ts)。

| 方法 | 说明 |
|---|---|
| `mount(host)` | 自己建画布（2D、WebGL 都行）放进 `host`，铺满它。画布大小按 `window.innerWidth/innerHeight × devicePixelRatio` |
| `load(src)` | 加载皮肤，返回位置信息（见下）。换皮肤时会再次调用，换完继续播当前状态 |
| `play(name)` | 播放某个状态。同一个名字重复调用不要重新开始。没有对应动画的状态，自己挑一个相近的 |
| `onHit(cb)` | 登记回调，画面变化时用它上报鼠标穿透信息 |
| `resize()` | 窗口尺寸变了（用户改了"大小"设置） |
| `destroy()` | 停止动画、释放资源、移除画布 |

### 读皮肤文件

`load(src)` 拿到的 `src`：

- `src.info`：`{ id, name, pet, overrides }`，`pet` 是 `pet.json` 的内容，`overrides` 是 `skin.json` 的内容。
- `src.readFile(path)`：读皮肤文件夹里的文件，返回 `ArrayBuffer`。`path` 相对皮肤文件夹，不能跳出去。

皮肤文件夹里除了 `pet.json`（至少要有 `displayName`）和 `skin.json`（写 `renderer`），其余文件怎么放由插件自己规定。

### 位置信息

`load` 返回 `{ layout }`：

| 字段 | 说明 |
|---|---|
| `cellW`、`cellH` | 画面的基准尺寸（逻辑像素）。窗口按它乘以"大小"设置定尺寸 |
| `footRatio` | 脚底在画面里的高度比例（0 顶 ~ 1 底）。程序把这一线贴在任务栏或窗口顶边上 |
| `centerRatio` | 身体水平中心的比例 |
| `topRatio` | 头顶的高度比例，对话气泡贴着它放 |
| `canSit` | 能不能坐在窗沿上（播 `sit` 时脚底线就是窗口顶边） |

骨骼动画可以拿默认姿势的包围盒来算这些比例。

### 鼠标穿透

程序每隔几十毫秒检查光标是否落在角色不透明的部分上，在的话才接收鼠标，否则让点击穿透到后面的窗口。渲染器有两种方式提供判定依据：

**实时读画面**（骨骼动画这类用这个）：`load` 只返回 `{ layout }`，每画完一帧调用 `LiveMask`：

```js
this.live = new api.LiveMask(80); // 最多每 80ms 读一次画面
// 每画完一帧：
const mask = this.live.sample(canvas);
if (mask) this.hitCb({ mask });
```

`sample` 会节流，画面没变时返回 `null`。换动画时可以传 `sample(canvas, true)` 立刻读一次。WebGL 画布要在画完的同一个任务里调用（比如 `requestAnimationFrame` 回调里画完就读），或者建上下文时开 `preserveDrawingBuffer`。换皮肤、改尺寸后调用 `live.reset()`。

**预先算好每帧**（帧数固定的才适合）：`load` 返回 `{ layout, frameMasks: { maskW, maskH, masks } }`，`masks[i]` 是第 `i` 帧的位图；换帧时上报 `this.hitCb({ frame: i })`。

掩码的格式：`w × h` 的位图，不透明度 ≥ 40/255 的算实心，行优先、低位在前打包成字节数组。宽高不必和画布一致，程序按比例对应，48 格左右宽就够了。

## 授权

插件和 Perch 是分开分发的，插件自己的许可证和依赖的许可证由插件作者负责。比如基于 Spine 运行库的插件，要遵守 Spine 运行库许可证，并且不能把它的代码放进 Perch 主仓库。
