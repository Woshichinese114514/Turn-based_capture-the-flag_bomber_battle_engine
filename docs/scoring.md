# 评分体系设计（log scale，每 500 分 ≈ 10 倍实力）

> 该文档是 `crates/scoring/**` 的设计说明。实现时把这里的推导写进代码注释，
> 让读代码的人不用翻文档也能明白「为什么分数是这样算的」。

## 1. 需求回顾（来自项目需求）

1. 输出是**一个实数分数**（可正可负）。
2. **对数计数（log scale）**：分数每高约 **500** 分，实力大约强 **10 倍**；
   差 1000 分 ≈ 100 倍。
3. **不同队伍数的对局分数体系不同**：2 队与 3 队的分数不可直接比较。
4. **实力强度以 2 队对局（1v1）的分数为准**；3 队对局是另一套体系，不做跨体系换算。
5. 算法自行完善，可以基于胜率/得分/击杀等指标，但必须通过对数映射落到上述分数体系。

## 2. 从原始指标到「实力」再到「分数」

记某 AI 的原始指标（都归一到 `[0,1]`）：

- `win_rate`：胜场/参赛局数（平局不算胜）。
- `avg_score_share`：每局「本队得分 / 全场总得分」（总分为 0 时取 0.5）。
- `avg_kill_ratio`：`kills / (kills + deaths)`（分母 0 时取 0.5）。

**第一步：综合实力 `strength`（0..1，0.5 表示「平均水准」）**

```text
strength_raw = (w_win * win_rate + w_score * avg_score_share + w_kill * avg_kill_ratio)
               / (w_win + w_score + w_kill)              // 权重归一，保证结果仍在 [0,1]
strength     = clamp(strength_raw, min_strength, max_strength)   // 默认 [0.02, 0.98]
```

另外对 `win_rate` 做**小样本收缩**（避免「打了 3 局赢 3 局 → 无限强」）：

```text
win_rate_shrunk = (wins + 0.5 * prior) / (matches + prior)      // prior = win_rate_prior_matches
```

**第二步：strength → odds → rating（这是满足「每 500 分 ≈ 10 倍」的关键）**

把 `strength` 解释为「该 AI 面对平均水平对手的期望胜率」：

```text
odds   = strength / (1 - strength)          // 0.5 -> 1（势均力敌）；0.9 -> 9（强 9 倍）
rating = baseline_rating + points_per_decade * log10(odds)
       = 1000 + 500 * log10(strength / (1 - strength))
```

验证语义：

| strength | odds | rating |
|---|---|---|
| 0.5 | 1 | 1000（基准：平均水平） |
| 0.9090909… | 10 | 1500（比平均强 10 倍） |
| 0.9900990… | 100 | 2000（强 100 倍） |
| 0.09 | 1/10 | 500（弱 10 倍） |

因为 `log10` 的底数是 10 且系数是 500，所以 **rating 每 +500 ⇔ odds ×10 ⇔ 实力强 10 倍**，
这正是需求要的语义。分数可以小于 0（strength 很小时），符合「可正可负」。

## 3. 为什么 2 队与 3 队必须分成两套体系

`strength` 的参照系是「**平均水平的对手**」，而平均的含义随队伍数变化：

- 2 队对局里，平均对手 = 1 个对手，0.5 胜率就是「和对手五五开」。
- 3 队对局里，平均对手 = 2 个对手，0.5 胜率意味着「每局有 50% 概率赢过另外两队之和」，
  同样是 0.5 但对应的博弈强度完全不同；而且 3 队局的平局概率显著更高（并列最高分）。

因此本项目**不做跨队伍数换算**：

- `ScoringReport.teams` 明确标注该分数属于 2 队体系还是 3 队体系；
- 2 队体系的分数是「实力基准」（可跨批次比较，前提是 `rules_version`、`map_gen_version` 相同）；
- 3 队体系的分数**只能在同一 `teams` 值内比较**；
- `summary.json` 同时记录权重与 `teams`，便于事后核对。

### 3.1 读 `summary.json` 时的一个易混点：`matches` 是「出场次数」

`by_ai_name[].matches` 统计的是**出场次数**（每个队伍每局计 1 次），不是对局数：

- 12 局「random vs random」（2 队、两个槽位同名）→ `by_ai_name` 里 `random.matches = 24`，
  而 `summary.total_matches = 12`；
- 3 队各用不同 AI 的 1000 局 → 每个名字 `matches = 1000`。

之所以按出场次数计：胜负平与「该名字每次出场」一一对应（W + D + L = matches），
胜率才是有意义的分母；对局数口径由 `total_matches` 单独给出，按槽位口径则看 `by_slot`
（`by_slot[].matches` 就等于对局数）。两者不要混用。

## 4. 胜/平/负判定

默认 `HighestScoreRule`（与 `sim::WinCondition::HighestScore` 一致）：

```text
最高分唯一且等于本队分数 -> Win
本队分数等于最高分且最高分被多队并列 -> Draw
否则 -> Loss
```

`MatchResult.winner` 已经给出了 `Option<TeamId>` 的结果；`OutcomeRule` 存在的意义是
**允许替换判定口径**并且让「胜/平/负」的分类集中在一处（将来接 ELO 时，可以把平局按 0.5 分算，
或者引入「击杀差」等二级指标）。评分聚合必须通过 `OutcomeRule` 得到 W/D/L，不得在别处重复实现。

## 5. 扩展点（未来可能加入 ELO / TrueSkill）

* `OutcomeRule` trait：替换胜负分类口径。
* `compute_strength` / `compute_rating` 是两个纯函数：把「综合指标 → 实力」与「实力 → 分数」
  分开，将来要换 ELO，只需要用 `expected_score` 替代 `strength`、用 `K` 因子更新替代 `log10` 映射。
* `ScoringConfig` 里带 `teams`：未来可以按队伍数选择不同的 `baseline_rating`
  （例如 3 队体系基准 1200），但**不允许**把两者混在同一张表里比较。
* 不引入外部评分库（保持依赖面最小）；算法必须能用固定输入写死期望值的单元测试锁住。
