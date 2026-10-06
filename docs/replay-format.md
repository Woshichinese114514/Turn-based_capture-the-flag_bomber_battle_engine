# 回放格式契约（Replay Format Contract）

> **这是 Rust 引擎与 Web UI 之间唯一的接口契约。双方都必须严格按本文档实现。**
> 任何格式变更都属于**协议变更**：必须升版本号、更新本文档、双方同步。
> 发现接口问题只在本文档或代码注释里提出，**不要擅自改协议**。

- 契约版本：`engine_version = 1`，`rules_version = 2`，`map_gen_version = 1`
  - 版本历史（**只增不改**，改动必须留痕，方便第三方复算旧回放）：
    - `engine_version 1` / `rules_version 1` / `map_gen_version 1`：首个可用版本。
    - `rules_version 2`：虚空从「不可通行」改为「**可进入但立即死亡**」。
      **JSON 结构与字段含义没有任何变化**（地形编码表、事件类型集合、单位/旗/炸弹字段全部照旧），
      变化的是「数据代表的规则」，因此只升 `rules_version`、不升 `engine_version`：
      解析器无需分支，但**分数/结果不可与 version 1 的回放直接比较**（同一局面下 AI 的合法动作集变了）。
      这也是版本号三件套的用途：看 `rules_version` 决定要不要比较结果，看 `engine_version` 决定要不要换解析器。
- 数据结构定义处：`crates/protocol/src/{types,view,replay,result}.rs`（Rust 侧唯一真源）
- 校验工具：`tools/validate_replay.py`（Python，对任意 `.jsonl` 回放做结构校验）
- 手写样例：`samples/demo_2p.jsonl`、`samples/demo_3p.jsonl`（供 Web UI 开发期使用，
  由 `tools/make_demo_replay.py` 生成；真实回放由 `cli run --out <dir>` 产出到 `<dir>/replays/`。
  另有 `samples/malformed.jsonl`（畸形行容错）与 `samples/version_mismatch.jsonl`（版本不匹配警告），
  两者也是 Web UI 的容错测试素材）

---

## 1. 文件格式

回放文件是 **JSONL**（每行一个 JSON 对象，UTF-8，行尾 `\n`）：

```text
第 1 行            {"type":"init",  ...}   恰好一行
第 2..N-1 行        {"type":"frame", ...}   每个全局 tick 一行，tick 从 1 开始逐 1 递增
最后 1 行           {"type":"end",   ...}   恰好一行
```

约定与理由：

- **顺序读取即可播放**：不需要读完整个文件才能出第一帧。
- **每帧是全量快照，不是差分**：Web UI 必须支持 O(1) 跳到任意 tick（进度条拖动、输入 tick 跳转），
  差分格式会强迫 UI 从头重放。
- **地图只在 `init` 出现一次**：地图整局不变，重复放进 frame 会让文件体积翻十倍。
- 解析器应当**容忍未知字段**（引擎将来可能新增可选字段）与**容忍缺失的可选字段**。
- 解析器遇到畸形行应当跳过并给出提示，**不允许崩溃**（见第 6 节容错要求）。

## 2. 命名与坐标约定

- 所有字段名都是 **snake_case**（`carrying_flag`、`respawn_timer`、`hit_units`…）。
- 坐标是**整数网格坐标**：`x` 向右增大，`y` 向下增大，`(0,0)` 在左上角；Web UI 自己乘格子大小。
- 坐标在 JSON 里是**平铺字段**（`"x":3,"y":4`），不是嵌套的 `"pos":{...}`。
- 数组型字段**下标即队伍 ID**：`scores[0]` 是 0 队分数；`kills`/`deaths`/`ai_names` 同理。
- 事件使用内部标签：靠 `"type"` 字段区分（见第 5 节）。
- 单位/旗/炸弹的 ID 在整局内**稳定不变**（单位死亡复活后 ID 不变）。

## 3. `init` 行

```json
{
  "type": "init",
  "engine_version": 1,
  "rules_version": 2,
  "map_gen_version": 1,
  "map": {
    "width": 5,
    "height": 5,
    "map_gen_version": 1,
    "terrain": [3,3,3,1,2, 3,3,3,0,0, 3,3,3,0,1, 0,0,0,0,0, 1,0,4,4,4]
  },
  "teams": [
    {"team_id": 0, "ai_name": "random",      "base_x": 0, "base_y": 0},
    {"team_id": 1, "ai_name": "greedy_flag", "base_x": 2, "base_y": 4}
  ],
  "max_ticks": 300,
  "flag_spawn_interval": 5,
  "center_radius": 4,
  "seed": 1234567890123
}
```

字段说明：

| 字段 | 类型 | 说明 |
|---|---|---|
| `type` | `"init"` | 行类型标签 |
| `engine_version` | u32 | 引擎版本（模拟核心实现） |
| `rules_version` | u32 | 规则版本（伤害/复活/得分等数值与判定） |
| `map_gen_version` | u32 | 地图生成算法版本（同种子不同版本会产出不同地图） |
| `map.width` / `map.height` | u16 | 地图尺寸（列数 / 行数） |
| `map.map_gen_version` | u32 | 与顶层 `map_gen_version` 一致（冗余，便于只解析 `map` 的消费者） |
| `map.terrain` | `u8[]` | **行优先**地形数组，长度必须等于 `width*height`，`idx = y*width + x` |
| `teams` | `TeamInfo[]` | 每队一条，下标即队伍 ID |
| `teams[].team_id` | u8 | 队伍 ID（0 起） |
| `teams[].ai_name` | string | AI 名字（缺失按 `""` 处理，UI 显示「未知」） |
| `teams[].base_x` / `base_y` | i32 | 该队 **3×3 阵营区左上角**坐标（不是中心） |
| `max_ticks` | u32 | 本局最大全局回合数 |
| `flag_spawn_interval` | u32 | 每隔多少回合检查一次补旗 |
| `center_radius` | u8 | 中心区域半径 R：旗只刷在「到中心曼哈顿距离 ≤ R」的格子 |
| `seed` | u64 | 本局地图种子（可选；`end.seed` 为准，缺失回退 0） |

### 3.1 地形编码（重要）

`map.terrain` 用**小整数**编码，而不是对象数组（25×25 地图有 625 格，对象数组会让 init 行膨胀十几倍）：

| 编码 | 含义 | 渲染建议 |
|---|---|---|
| `0` | `Empty` 空地 | 浅色/网格底 |
| `1` | `Wall` 墙 | 深色实心块 |
| `2` | `Void` 虚空 | 最深色/背景色 |
| `3 + team_id` | `TeamBase(team_id)` 阵营格 | 队伍色半透明填充 + 加粗边界 |

因此：2 队地图出现 `3`、`4`；3 队地图出现 `3`、`4`、`5`。**阵营归属可直接由编码反推**，
不需要额外查表。地形语义（判定规则）：

- `Empty`：可通行、可放炸弹、旗可以刷/掉在这里。
- `Wall`：阻挡移动、**阻挡视线**、**阻挡炸弹十字爆炸的继续传播**。
- `Void`：**可进入，但进入即死**（`rules_version >= 2`，见 `docs/rules.md` §1/§9）。不可放置任何东西、
  旗不会掉在这里、不会在这里复活；**不阻挡视线**、**不阻挡爆炸**。
  UI 表现建议：画成「深色深渊」而不是「实心墙」，这样观众能从画面区分「走不进去的墙」和「走进去会死的虚空」。
- `TeamBase`：出生点/复活点/得分区；敌方单位不能进入；禁止放炸弹；格内单位免疫爆炸伤害。

> Web UI 侧注意：地形编码超出 `0..=6` 时按「未知地形」兜底（按 `Empty` 渲染 + 控制台警告），不要抛异常。

## 4. `frame` 行

```json
{
  "type": "frame",
  "tick": 12,
  "scores": [1, 0],
  "units": [
    {"id": 0, "team": 0, "x": 1, "y": 1, "hp": 3, "alive": true,  "respawn_timer": 0, "carrying_flag": null, "attacked_this_turn": false},
    {"id": 3, "team": 1, "x": 2, "y": 2, "hp": 2, "alive": true,  "respawn_timer": 0, "carrying_flag": 0,    "attacked_this_turn": true},
    {"id": 5, "team": 1, "x": 4, "y": 4, "hp": 0, "alive": false, "respawn_timer": 7, "carrying_flag": null, "attacked_this_turn": false}
  ],
  "flags": [
    {"id": 0, "x": 2, "y": 2, "carrier": 3},
    {"id": 1, "x": 0, "y": 0, "carrier": null}
  ],
  "bombs": [
    {"id": 0, "x": 0, "y": 1, "team": 0, "timer": 2, "radius": 2}
  ],
  "events": [
    {"type": "unit_moved", "unit": 3, "from_x": 2, "from_y": 1, "to_x": 2, "to_y": 2}
  ]
}
```

### 4.1 单位视图 `UnitView`

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | u32 | 单位 ID，整局稳定 |
| `team` | u8 | 所属队伍 |
| `x` / `y` | i32 | 所在格；死亡时保留死亡位置（可选画灰色虚影） |
| `hp` | u8 | 当前 HP，`0..=3` |
| `alive` | bool | 是否存活；死亡单位不阻挡移动 |
| `respawn_timer` | u32 | 复活倒计时，`0` 表示未在倒计时 |
| `carrying_flag` | u32 或 `null` | 携带的旗 ID |
| `attacked_this_turn` | bool | 本回合是否已攻击（每回合最多 1 次） |

### 4.2 旗视图 `FlagView`

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | u32 | 旗 ID |
| `x` / `y` | i32 | 旗所在格；**被携带时等于携带者所在格**（引擎保证一致，UI 无需自己换算） |
| `carrier` | u32 或 `null` | 携带者单位 ID；在地面上为 `null` |

### 4.3 炸弹视图 `BombView`

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | u32 | 炸弹 ID |
| `x` / `y` | i32 | 放置位置（炸弹不移动） |
| `team` | u8 | 放置者队伍（炸弹有友伤，不能只按敌我上色） |
| `timer` | u8 | 剩余 tick 数；放置当回合为 `2`，之后每 tick 减 1，减到 `0` 的那个 tick 结算爆炸 |
| `radius` | u8 | 十字爆炸半径（当前为 2，曼哈顿距离 ≤ radius 且不被墙阻挡） |

> `timer` 在回放中只会出现 `2`、`1`：值变成 `0` 的那个 tick 直接爆炸，炸弹不再出现在该帧里。
> UI 可以据此把颜色从黄渐变到红。

## 5. 事件

事件是**可选使用的动画素材**：实体视图已经是本 tick 结算后的最终状态，
即使完全忽略 `events` 也能正确渲染。事件用于播放动画（移动、攻击线、爆炸扩散、得分闪光）
与写事件日志。

所有事件共用一个 `"type"` 字段，取值为 snake_case 字符串：

| `type` | 附加字段 | 含义 | 建议 UI 表现 |
|---|---|---|---|
| `unit_moved` | `unit`,`from_x`,`from_y`,`to_x`,`to_y` | 单位移动一格成功 | 起点→终点位移补间 |
| `unit_attacked` | `attacker`,`target`,`damage` | 攻击命中，造成 `damage` 点伤害 | 攻击者→目标连线/闪光 |
| `unit_died` | `unit`,`team`,`by`（u32 或 null） | 单位死亡 | 灰化/墓碑，`by` 为伤害来源（可能是自己的炸弹） |
| `unit_respawned` | `unit`,`team`,`x`,`y` | 在己方阵营满血复活 | 出生圈闪光 |
| `flag_picked` | `unit`,`flag` | 拾旗成功 | 旗跟随单位 |
| `flag_dropped` | `flag`,`x`,`y` | 携带者死亡，旗掉落到该格 | 旗落下 |
| `flag_spawned` | `flag`,`x`,`y` | 中心区域刷新新旗 | 中心闪光 |
| `score` | `team`,`unit`,`flag`,`new_score` | 得分（旗消失，队伍 +1） | 阵营闪光 + 分数跳动 |
| `bomb_placed` | `unit`,`bomb`,`x`,`y`,`timer` | 放置炸弹 | 炸弹落下 |
| `bomb_exploded` | `bomb`,`x`,`y`,`radius`,`hit_units` | 炸弹爆炸，`hit_units` 为实际被扣血的单位 ID 数组 | 十字扩散动画 |
| `move_conflict` | `x`,`y`,`units`（u32 数组） | 多个单位同 tick 想进同一格，全部留在原地但消耗 AP | 冲突格爆闪告警 |
| `illegal_action` | `unit`,`action`,`reason` | AI 提交了非法指令，引擎按等待处理 | 日志告警（便于调试 AI） |

事件字段一律是**平铺标量或数组**，没有嵌套对象；坐标以 `from_x/from_y/to_x/to_y` 形式展开。

**事件文本建议**（Web UI 事件日志必须人类可读，禁止直接打印 JSON）：

```text
tick 12 · 蓝队单位 3 从 (2,1) 移动到 (2,2)
tick 12 · 红队单位 0 攻击 蓝队单位 3，造成 1 点伤害
tick 12 · 蓝队单位 3 拾取旗 #0
tick 12 · 炸弹 #0 爆炸（半径 2），命中单位 [0,3]
tick 12 · 移动冲突：单位 [1,4] 同时想进入 (5,5)，双方留在原地（消耗 1 AP）
```

## 6. `end` 行

```json
{
  "type": "end",
  "match_index": 0,
  "seed": 1234567890123,
  "map_gen_version": 1,
  "ticks": 300,
  "scores": [2, 1],
  "kills": [4, 3],
  "deaths": [3, 4],
  "winner": 0,
  "ai_names": ["random", "greedy_flag"]
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `match_index` | u32 | 局索引（0 起，与 `replays/match_00000.jsonl` 对应） |
| `seed` | u64 | 本局地图种子（复现同一张图：种子 + `map_gen_version`） |
| `map_gen_version` | u32 | 地图生成版本 |
| `ticks` | u32 | 实际跑完的全局回合数 |
| `scores` | i32[] | 各队最终得分，下标即队伍 |
| `kills` | u32[] | 各队击杀 |
| `deaths` | u32[] | 各队死亡 |
| `winner` | u8 或 `null` | 赢家队伍 ID；**`null` 表示平局**（并列最高分即平局） |
| `ai_names` | string[] | 各队 AI 名字，下标即队伍 |

`matches.jsonl` 的每一行就是**同一个结构**（不含 `type` 字段，因为整文件都是结果行）。

## 7. 版本号与容错

- 三个版本号在 `manifest.json`、`summary.json`、回放 `init` 行**三处都必须存在**。
- Web UI 解析策略：
  - `map_gen_version` 不是已知版本（当前为 `1`）→ **显示警告横幅**，仍尽量渲染（地形编码按本表解释）。
  - `rules_version` 高于已知版本（当前为 `2`）→ 同样只警告不拒绝：UI 只画数据，不重算规则，
    所以「规则更新」对渲染不构成障碍，但要在信息面板里注明「规则版本较新，结果解释可能有出入」。
  - `engine_version` / `rules_version` 高于 UI 已知版本 → 警告「回放可能包含本 UI 不认识的规则/事件」，
    跳过不认识的事件类型，继续渲染。
  - `map.terrain` 长度与 `width*height` 不符 → 警告 + 用 `Empty` 补齐缺失部分（不要崩溃）。
- 畸形行（JSON 解析失败）→ 跳过该行，累计错误计数并在界面上提示，例如
  「已跳过 3 行无法解析的数据（文件可能有损坏）」。

## 8. 最小可用示例

一份可被 `tools/validate_replay.py` 通过的最小回放（2 队、2 tick、5×5 地图）：

> **注意：这是「格式示例」，不是引擎的真实产物。** 真实地图受 `mapgen` 约束：边长至少 8
> （要放下 3 个互不重叠的 3×3 阵营区），且阵营区左上角遵循
> `docs/internal-api.md` 第 2 节的公式（2 队为 `(1,1)` 与 `(width-4,height-4)`）。
> 因此下面这份 5×5、`base=(0,0)/(2,4)` 的数据**只用于演示字段形状与解析容错**，
> 引擎永远不会生成它；写解析器时不要把它当成合法性判据。

```text
{"type":"init","engine_version":1,"rules_version":2,"map_gen_version":1,"map":{"width":5,"height":5,"map_gen_version":1,"terrain":[3,3,3,1,2,3,3,3,0,0,3,3,3,0,1,0,0,0,0,0,1,0,4,4,4]},"teams":[{"team_id":0,"ai_name":"random","base_x":0,"base_y":0},{"team_id":1,"ai_name":"defender","base_x":2,"base_y":4}],"max_ticks":300,"flag_spawn_interval":5,"center_radius":4,"seed":42}
{"type":"frame","tick":1,"scores":[0,0],"units":[],"flags":[],"bombs":[],"events":[]}
{"type":"frame","tick":2,"scores":[1,0],"units":[],"flags":[],"bombs":[],"events":[{"type":"score","team":0,"unit":0,"flag":0,"new_score":1}]}
{"type":"end","match_index":0,"seed":42,"map_gen_version":1,"ticks":2,"scores":[1,0],"kills":[0,0],"deaths":[0,0],"winner":0,"ai_names":["random","defender"]}
```
