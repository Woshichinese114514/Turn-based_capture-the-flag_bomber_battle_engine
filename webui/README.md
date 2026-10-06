# 抢旗人 · 回放查看器（webui）

纯前端、零依赖的回放可视化页面：把引擎导出的 `.jsonl` 回放渲染成可交互的 2D 战场，
并提供逐 tick 的**中文**事件日志、队伍分数与信息面板。

- 技术栈：原生 HTML + CSS + JavaScript（经典 `<script src>` + `window.QFR` 命名空间），Canvas 2D 手绘。
- **没有**框架、打包器、渲染库、后端或 ES module —— 因为要在 `file://` 下双击直接打开，
  而 `<script type="module">` 在 `file://` 会被 CORS 拦截。
- 不使用 `eval`；所有来自回放的文本（AI 名字、非法动作原因等）一律经 `textContent` 写入，绝不拼 `innerHTML`。

---

## 1. 怎么打开

**方式一（最简单）**：双击 `webui/index.html`，浏览器直接打开。

**方式二（带参数，无头验收/自动化用）**：

```bash
# ?replay=<相对或绝对 url>   ?tick=<1 基 tick>   ?err=<人为触发错误，自测用>
chromium --headless --no-sandbox --disable-gpu --allow-file-access-from-files \
  --hide-scrollbars --user-data-dir=$PWD/.tmp/run/udd --virtual-time-budget=5000 \
  --window-size=1440,900 --screenshot=$PWD/webui/screenshots/demo_2p.png \
  "file://$PWD/webui/index.html?replay=../samples/demo_2p.jsonl&tick=12"
```

浏览器直开时若用 `file://` 且需要 `?replay=` 读取本地文件，Chrome/Chromium 需要加
`--allow-file-access-from-files`（页面会给出可读提示，并建议改用「选择文件」按钮）；
Firefox 一般无需额外参数。**页面本身不依赖网络，也不需要任何服务。**

## 2. 怎么加载回放

| 方式 | 操作 |
| --- | --- |
| 文件选择 | 右上角「选择回放文件…」 |
| 拖拽 | 把 `.jsonl` 拖到地图区域（拖入时画布会高亮）；一次拖多个只加载第一个并给出提示 |
| URL 参数 | `index.html?replay=../samples/demo_2p.jsonl&tick=12` |

加载后会立刻渲染目标帧（`?tick=N` 直接画第 N tick，不做补间动画），并更新所有面板。

## 3. 控制与快捷键

- 播放控制：`⏮` 首帧、`−50 / −10 / 上一步`、`播放/暂停`、`下一步 / +10 / +50`、`⏭` 末帧、`重置到首帧`。
- 进度条：拖动即跳转（值 = 帧下标）；右侧显示「帧 i / N　速度 kx」。
- 跳到 tick：输入 1 基 tick 后回车或点「跳转」；越界会夹到边界并弹出提示。
- 速度：`0.5x / 1x / 2x / 4x`（1x ≈ 每秒 2.5 帧）。
- 队伍过滤：事件日志按队伍过滤，下拉项显示「队伍色名（AI 名字）」。
- 画布：点炸弹格子可选中它（显示十字爆炸范围预览）；点别处取消。缩放：`＋ / 自适应 / −`。
- 键盘：`空格` 播放/暂停、`←/→` 上/下一帧、`Home/End` 首/末帧。
  焦点在输入框或下拉框内时不抢按键。

## 4. 回放格式约定（查看器视角）

一行一条 JSON（JSONL），顺序为：`init` → 每 tick 一条 `frame` → `end`。

- 契约版本：`engine_version` / `rules_version` / `map_gen_version` 当前均为 `1`。
  **版本策略：不认识的版本只给黄色警告，继续渲染** —— 旧 UI 必须还能打开新回放，
  不认识的事件类型原样显示类型名，不认识的地形编码按空地渲染（并计入控制台警告）。
- `init.map.terrain` 行优先，`idx = y * width + x`；编码 `0=空地 1=墙 2=虚空 3+team_id=阵营格`。
- `frame` 是**全量快照**：`units/flags/bombs/events/scores` 都可能缺失或为 `null`，
  缺失时按空数组/0 处理；`flags[].x/y` 在被携带时等于携带者坐标；`bombs[].timer` 只会出现 `2/1`。
- `end.winner` 为 `null` 表示平局；缺 `end` 只提示「最终结果未知」，其余照常播放。
- 实体 ID 整局稳定，因此日志里的「单位 5」全程指向同一个单位。

容错策略（都有可视提示，绝不白屏或卡死控件）：

| 情况 | 行为 |
| --- | --- |
| 单行坏 JSON | 跳过该行 + 顶部提示「已跳过 N 行……（行号）」，其余照常 |
| 未知 `type` 行 | 忽略并计数提示（可能是新协议字段） |
| 缺 `init` | 用 15×15 空地图兜底渲染 + 黄色警告（`malformed.jsonl` 走的就是这条路） |
| `terrain` 长度 ≠ `width*height` | 警告 + 用空地补齐/截断 |
| 文件里 `init`/`frame`/`end` 一条可用数据都没有 | 顶部红色错误横幅 + 保留兜底画面与全部控件 |
| `?replay=` 打不开（路径错/权限） | 红色错误横幅说明原因与解决办法，控件仍可用 |

## 5. 队伍颜色与命名（一致性约定）

队伍配色用 **Okabe–Ito 色盲友好调色板**（按 `team_id` 固定取色，与加载顺序无关）：

| team_id | 颜色 | 面板/日志里的名字 |
| --- | --- | --- |
| 0 | `#0072B2` 蓝 | 蓝队 |
| 1 | `#D55E00` 橙 | 橙队 |
| 2 | `#009E73` 绿 | 绿队 |
| 3 | `#CC79A7` 品红 | 品红队 |

**名字只描述颜色，不表示阵营含义**。早期版本用过「红队/蓝队」，画面上却是蓝打橙，
日志与画面语义不符；现在统一成颜色名，日志、分数牌、过滤器与地图配色一致。

## 6. 无头验收钩子（自动化/回归用）

页面在**首帧渲染完成后**同步设置：

| 全局变量 | 含义 |
| --- | --- |
| `window.__QFR_READY__` | `true` 表示已完成首帧渲染 |
| `window.__QFR_STATUS__` | `{ file, ticks, totalTicks, teams, currentTick, currentIndex, speed, playing, warnings[], errors[], badLines, map:{width,height}, rendered, error }` |
| `window.__QFR_ERROR__` | 加载失败时的错误文本（成功时不设置） |

辅助调试入口：`QFR.app.refreshStatus()`（重绘并回写状态）、`QFR.app.loadText(text, name)`
（直接喂一段回放文本）、`QFR.app.enableTestError(msg)`（人为触发错误横幅）。

## 7. 自测结论（chromium 无头，2025 实测）

环境：Chromium 153（`/usr/bin/chromium`）。本机裸跑 `--headless` 需要
`--no-sandbox --user-data-dir=<可写目录>` 且 `HOME` 指向可写目录，否则报
`Failed to create headless user data directory container`。

验证命令（截图与断言分开跑）：

```bash
# ① 真实截图（即 docs/webui-spec.md §7 的命令）
chromium --headless --no-sandbox --disable-gpu --allow-file-access-from-files --hide-scrollbars \
  --user-data-dir=$PWD/.tmp/run/udd --virtual-time-budget=5000 --window-size=1440,900 \
  --screenshot=$PWD/webui/screenshots/demo_2p.png \
  "file://$PWD/webui/index.html?replay=../samples/demo_2p.jsonl&tick=12"

# ② 断言钩子 + DOM 计数 + console 错误收集（自带 CDP 客户端，Node 22+，无 npm 依赖）
node webui/.selfcheck/cdp_check.js '../samples/demo_2p.jsonl' 12 webui/screenshots/demo_2p.png
```

| # | 样例 | 结果 | 截图 |
| --- | --- | --- | --- |
| 1 | `demo_2p.jsonl`（2 队 24 tick，12 种事件全覆盖） | ✅ `ready=true`，`tick=12`，地图/阵营区/中心虚线圆/单位+HP 点/旗/事件日志（3 行）/分数牌全部可见，`rendered=6`，无 console 错误 | `screenshots/demo_2p.png` |
| 1b | 同上末帧 `&tick=24` | ✅ 10 条事件、末帧显示「最终结果：蓝队 获胜」与击杀/死亡，`rendered=6` | `screenshots/demo_2p_result.png` |
| 2 | `malformed.jsonl`（坏行 + 无可用 `init`） | ✅ 不崩溃：提示「已跳过 1 行……行号：2」+ 黄色警告 2 条，15×15 空地图兜底渲染，控件可用 | `screenshots/malformed_jsonl.png` |
| 3 | `version_mismatch.jsonl`（`engine_version=99`） | ✅ 黄色版本警告 2 条，渲染继续（`rendered=6`，`tick=5`） | `screenshots/version_mismatch.png` |
| 4 | `demo_3p.jsonl`（3 队） | ✅ `teams=3`、3 张分数牌、`rendered=9`，三处阵营区各自配色正确 | `screenshots/demo_3p.png` |

补充边界用例（同一次自测中验证）：

- `?err=...`：错误横幅与 `__QFR_ERROR__` 正常置位，控件仍可点。
- 空文件（`/dev/null`）：判定为 fatal，红色错误横幅「文件里没有任何可用的回放数据」+ 兜底画面，不白屏。
- 不存在的路径：红色错误横幅并给出 `--allow-file-access-from-files` 或「选择文件」的解决建议。
- **交互链路**（`webui/.selfcheck/interact_demo.js`，20 项断言全部通过）：下一步按钮、`←/→/Home/End`
  键盘、tick 输入跳转与越界夹取提示、速度切换、队伍过滤（行数 10 → 5）、播放自动推进、暂停不漂移、
  重置、进度条跳转，均为真实点击/键盘事件驱动。

## 8. 目录结构

```
webui/
├── index.html              # 唯一页面（经典 script 顺序：core→parser→state→render→log→controls→main）
├── css/style.css           # 暗色主题与响应式布局（≤900px 侧栏下移，≤620px 压缩页头页脚）
├── js/core.js              # 常量（版本/地形编码）、队伍配色与命名、工具、URL 参数、就绪钩子
├── js/parser.js            # JSONL 解析、容错归一化、版本校验、访问器（二分查 tick）
├── js/state.js             # 唯一状态源 + 事件订阅（goto/step/播放/速度/过滤/选中炸弹）
├── js/render.js            # Canvas 2D 渲染（地形/中心区/旗/炸弹/事件高亮/单位/HUD）与坐标换算
├── js/log.js               # 12 种事件 → 中文描述、事件日志、信息面板、分数牌、最终结果
├── js/controls.js          # 文件选择/拖拽、按钮、进度条、速度、过滤、键盘、画布点选
├── js/main.js              # 装配、渲染调度（rAF 合并）、播放循环、加载流程、就绪钩子
├── .selfcheck/cdp_check.js # 无头验收小工具（CDP 断言 + 截图，非页面运行时依赖）
├── .selfcheck/interact_demo.js # 交互回归脚本（由 cdp_check.js --script= 注入执行）
├── screenshots/            # 第 7 节表格里的 5 张截图
└── README.md
```

## 9. 已知限制

1. **`file://` + `?replay=` 受浏览器权限限制**：Chromium 需要 `--allow-file-access-from-files`；
   日常使用请直接双击后用「选择文件」或拖拽，这两条路径没有任何限制。
2. **不支持动画补间**：跳转/步进直接画目标帧（规范要求）。出拳、爆炸等只有静态高亮叠层，
   不做逐帧插值；爆炸的“十字”按曼哈顿距离绘制，不逐格做视线遮挡回放（墙阻挡只在炸弹倒计时预览里体现）。
3. **只读查看器**：不修改回放，不做导出/编辑；大地图在极小窗口下格子最小 8px，细节会看不清（可放大）。
4. **超长回放**：解析与渲染都是 O(帧数)，事件日志每 tick 重建 DOM；10 万帧级别的回放会占用较多内存，
   建议按需截取片段。
5. `?tick=N` 若该 tick 不存在（回放有断号），会跳到**不超过 N 的最近一帧**；信息面板同时显示
   「帧序号」与「tick 值」，避免误读进度。
6. 协议字段以 `docs/replay-format.md` 为准；本 UI 只读不校验写。已确认 3 个正常样例与文档一致，
   未发现需要引擎侧修改的协议问题（唯一可疑者是 `samples/malformed.jsonl` 第 1 行 `init` 被截断，
   属于样例本身有意构造的坏数据）。

## 10. 验收阶段的修复与截图复现（Lead）

Web UI 冻结后，Lead 在验收时用自有的 CDP 工具复核「版本不匹配要有醒目提示」这一条，
发现一个**只在特定布局时序下才暴露**的样式缺陷，已修复：

- **现象**：`webui/css/style.css` 的 `body` 是 `display:grid; height:100vh; overflow:hidden`，
  但 `grid-template-rows` 只声明了 4 行（`auto auto minmax(0,1fr) auto`），而 `body` 的直接
  子元素有 6 个（`header` / `noticeBar` / `errorBanner` / `warningBar` / `main` / `footer`）。
  被标记 `hidden` 的横幅不占格子，于是「谁落在第 3 行（`1fr`）」会随横幅显隐而变：只要有一条
  可见横幅，它就会掉进 `1fr` 那一行，被压成 **19px**（其内容实际 86px），同时 `main` 被挤到
  隐式 `auto` 行。外部表现就是「窗口偏矮时，版本不匹配警告看不见」。
- **证据**：修复前 `1440×900` 的无头截图里，警告条 `#warningBar` 的
  `getBoundingClientRect().height = 19`、`ul` 高 41px，整张图的警示色 `#ffe9b0` 像素数为 **0**；
  修复后同一 URL 为 `height = 86`、警示色像素 **1527**。
- **修复**：`grid-template-rows` 改为与 6 个子元素逐行对应的
  `auto auto auto auto minmax(0, 1fr) auto`，横幅恒 `auto`、`main` 恒 `1fr`，布局不再随横幅显隐抖动。
- **截图复现**：`webui/screenshots/` 下 5 张图已用 Lead 自有的 `tools/shot.js`（CDP 截图 +
  `window.__QFR_STATUS__` 断言）在修复后重新生成，命令示例：

  ```bash
  node tools/shot.js --url "file://$PWD/webui/index.html?replay=../samples/version_mismatch.jsonl&tick=5" \
       --out webui/screenshots/version_mismatch.png --fixed-viewport --width 1440 --height 900 \
       --require-status --require-warnings
  ```

  该工具与 `webui/.selfcheck/cdp_check.js` 相互独立（前者属于验收工具链，后者是开发期自测脚本）：
  两者都走 CDP，但 `tools/shot.js` 额外断言「告警横幅布局可见」并对截图做像素级复核。

> 坑位备注：`chromium --headless --screenshot=...` 这条 CLI 路径在本机会**整块丢掉**回放加载后
> 由 JS 插入 DOM 的告警横幅（同一 URL 警示色像素 0，CDP 路径 1500+；加长 `--virtual-time-budget`
> 与 `--run-all-compositor-stages-before-draw` 均无效）。所以「横幅可见」只能用 CDP 截图取证，
> 这一点已写进 `tools/acceptance.sh` 关卡 5 的注释。
