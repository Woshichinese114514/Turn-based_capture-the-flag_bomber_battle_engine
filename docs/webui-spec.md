# Web UI（回放查看器）实现规范

> 对应项目需求第三部分。**本目录（`webui/`）只属于 Web UI 负责人**：不要修改任何 Rust 代码；
> 发现协议问题只在 `webui/README.md` 或注释里提，不要擅自改 `docs/replay-format.md`。
> 纯前端、无后端、无框架、无渲染库：**自己用 Canvas 2D + 原生 DOM 实现**。

## 1. 技术约束（硬性）

- 纯 HTML/CSS/JS，**不需要构建步骤**，双击 `webui/index.html`（`file://`）就能用。
- **禁止**任何框架（React/Vue/Svelte…）、任何 WebGL/游戏/物理引擎（Three.js/PixiJS/Phaser/Konva…）。
- **禁止 ES module（`<script type="module">`）**：浏览器在 `file://` 下会因为 CORS 拒绝加载模块脚本，
  用户没开 `--allow-file-access-from-files` 就打不开了。用**经典 `<script src="...">`** +
  每个文件一个 IIFE/命名空间对象（例如 `window.QFR.parser`），或者干脆写成一个 `index.html`。
- 禁止 `eval`；禁止用 `innerHTML` 拼接未转义的用户数据（文件名、AI 名字、事件文本都算用户数据）；
  必须用 `textContent` / `createElement`。
- 不要用全局裸变量做状态：状态集中在 `state` 模块对象里。

## 2. 数据来源与解析

- 输入：回放 JSONL（Rust 引擎导出，或 `samples/*.jsonl` 手写样例）。
  字段表、地形编码、12 种事件、容错策略见 **`docs/replay-format.md`**（权威契约）。
- 逐行 `JSON.parse`，按 `type` 区分 `init`/`frame`/`end`；地图只在 `init`。
- 全部加载进内存后按 tick 建索引 → **O(1) 跳转**（进度条拖动、输入 tick 跳转必须即时）。
- 容错（必须，且有可视提示，不能崩）：
  - 空文件 / 全是坏行 → 在界面上给出可读错误，进度条与控制保留可用；
  - 单行坏 JSON → 跳过并计数，界面上提示「跳过 N 行」；
  - 帧字段缺失 → 用默认值兜底（`units`/`flags`/`bombs`/`events` 当空数组，`scores` 当全 0）；
  - `init.engine_version` / `map_gen_version` 与预期不符 → 顶部显示黄色警告，但**继续渲染**；
  - 未知事件 `type` → 日志里显示原始类型名，不报错（向前兼容）；
  - `end` 缺失 → 仍可播放，只显示「未提供最终结果」。
- 回放字段访问**集中**在 parser 模块（或一个 `accessors` 对象）里，便于协议升级时统一改。

## 3. 渲染（Canvas 2D，自己画）

必须画：地图、阵营、中心区域（可选虚线）、单位、旗、炸弹、各队分数、tick/总 tick。

- 地图：墙=深色实心块；虚空=更深的底色（或画成网格外）；空地=浅色网格；
  阵营=队伍色半透明填充+加粗边界。
- 单位：队伍色实心圆/方 + HP 数字或血条；死亡单位默认不画或画半透明灰影；
  携带旗的单位要有额外标记（旗图标叠加）；可选「本回合已攻击」小标记。
- 旗：三角/旗形图标（携带时跟随携带者）。**图标形状要能区分单位/旗/炸弹，不能只靠颜色**。
- 炸弹：圆形，颜色随 `timer` 从黄到红，中间写倒计时数字；可选爆炸范围预览（悬停/选中）。
- 分数：每队一个分数牌（队伍色），领先队伍高亮；tick 显示 `当前/总`。
- 格子尺寸自适应窗口并保持正方形（`cell = floor(min(canvasW/width, canvasH/height))`），
  `resize` 时重算；**坐标换算逻辑必须有注释**（网格坐标 → 像素：`px = x*cell + originX`）。
- 队伍颜色稳定、尽量色盲友好（推荐 Okabe-Ito 调色板）；不同队伍颜色固定不随加载顺序变化。

## 4. 交互

- 文件选择器 + 拖拽文件加载（拖入非 `.jsonl` 给提示）；可选：多文件/目录拖拽。
- 加载后自动显示第一帧，并显示文件名/总 tick/地图尺寸/队伍数/AI 名字。
- 控制：前进一帧、后退一帧、+10、+50、跳到结尾、输入 tick 跳转、进度条拖动、
  播放/暂停、速度（0.5x/1x/2x/4x）、重置到首帧；键盘：空格播放暂停、左右箭头步进、
  Home/End 首尾。
- 自动播放用 `setInterval`/`requestAnimationFrame` 驱动，**不阻塞主线程**；
  跳转时**不播中间动画**，直接画目标帧。
- 状态机清晰：`{ currentTick, playing, speed }`，所有改变状态的入口都走同一组函数。

## 5. 事件日志与信息面板

- 事件日志：人类可读中文文本（例如「红队单位 3 攻击 蓝队单位 5，造成 1 点伤害」），
  **禁止直接贴 JSON**；可按队伍过滤；只在 tick 变化时更新 DOM（不要每帧重建）。
- 信息面板：当前 tick/总 tick、各队分数、AI 名字、存活单位数/场上旗数/炸弹数；
  结束时显示最终结果（赢家 / 各队得分 / 击杀 / 死亡）。
- 可选加分：事件动画（攻击连线、爆炸扩散、得分闪光），但**绝不能阻塞步进/跳转**。

## 6. 布局与代码组织

- 布局：顶部（标题/文件名/tick/分数/播放控制），中间 Canvas，右侧（事件日志+信息面板），
  底部（进度条/速度/跳转）；响应式，暗色主题优先；窗口极小也要能用（控制与进度不消失）。
- 代码组织：`parser` / `state` / `renderer` / `controls` / `eventLog` / `main`，
  每个文件顶部写文件级注释（职责 + 依赖方向）；模块间通过明确接口通信，不互相改内部状态。
- 性能：24~300 tick、几十个实体流畅；步进/跳转即时。

## 7. 无头验证钩子（**必须实现**，Lead 用它做交付验收）

Lead 会用 chromium 无头模式截图验证，因此页面必须支持：

1. `webui/index.html?replay=<url>`：加载并显示指定回放（相对路径即可，例如
   `?replay=../samples/demo_2p.jsonl`）。加载失败要在页面上显示错误文本，不要静默。
2. `?tick=<N>`：加载后跳转到第 N 个 tick（可选但强烈建议）。
3. 就绪标志：首帧渲染完成后设置 `window.__QFR_READY__ = true`（并附
   `window.__QFR_STATUS__ = { file, ticks, teams, errors }`），供无头脚本判断。
4. 加载失败时设置 `window.__QFR_ERROR__ = "..."`。

验证命令示例（Lead 会跑）：

```bash
chromium --headless --disable-gpu --allow-file-access-from-files --hide-scrollbars \
  --virtual-time-budget=4000 --window-size=1440,900 \
  --screenshot=/tmp/webui.png \
  "file:///home/zqh/项目/抢旗人/webui/index.html?replay=../samples/demo_2p.jsonl&tick=12"
```

## 8. 交付与自测

- `webui/index.html`（+ 若干 `.js`/`.css`，若拆文件）、`webui/README.md`
  （如何打开、如何加载回放、支持的回放格式与容错、截图）。
- 自测清单（在 README 里记录结果）：正常回放能从头看到尾；步进/后退/跳转/自动播放正常；
  分数/单位/旗/炸弹显示正确；事件日志文本正确；窗口缩放自适应；
  `samples/malformed.jsonl` 不崩溃且有错误提示；`samples/version_mismatch.jsonl` 有版本警告但能渲染；
  `samples/demo_3p.jsonl`（3 队）正常显示。
- 条件允许时给解析器写几个断言（可以是一个不依赖测试框架的 `webui/tests/parser.test.html`
  或 `webui/selfcheck.js`，用无头模式跑；没有也不强求，但要留下手动验证记录）。
