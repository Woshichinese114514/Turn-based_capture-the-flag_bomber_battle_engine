#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""版本号三件套一致性检查（Lead 验收用）。

设计意图
========
回放契约规定「引擎版本 / 规则版本 / 地图生成版本」必须同时出现在三处：
  1. manifest.json（一次批量运行的全局声明）
  2. summary.json（评分汇总，供人阅读与存档）
  3. 每份回放的 init 行（Web UI 解析时唯一可见的版本来源）

任何一处漏写、写死旧值、或与另外两处不一致，都会让 Web UI 按错误的策略去解析
同一批数据；而这种错误在单元测试里看不出来（各 crate 各自自洽），只有在
「跨文件」层面才暴露。所以这里做的是跨文件比对，而不是要求某个 crate 单测覆盖。

判定规则（刻意严格）
====================
- manifest 三件套必须齐全且为正整数；
- summary 三件套必须齐全，且与 manifest 完全相等（同一次运行不该出现两套版本）；
- 每份回放 init 的三件套必须与 manifest 完全相等；
- 同时顺带检查 matches.jsonl 的 map_gen_version 与 manifest 一致（它也是协议的一部分，
  下游常按它过滤可比性的对局）。

用法
====
    python3 tools/check_versions.py <out_dir> [<out_dir> ...]

退出码 0 表示全部一致；1 表示存在不一致（全部差异打印出来后退出）。
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

VERSION_KEYS = ("engine_version", "rules_version", "map_gen_version")


def _load_json(path: Path) -> dict:
    """读 JSON 文件；失败时抛出带文件名的异常，便于定位是哪一批产物坏了。"""
    try:
        with path.open("r", encoding="utf-8") as fh:
            data = json.load(fh)
    except FileNotFoundError:
        raise SystemExit(f"缺少文件：{path}")
    except json.JSONDecodeError as exc:
        raise SystemExit(f"{path} 不是合法 JSON：{exc}")
    if not isinstance(data, dict):
        raise SystemExit(f"{path} 顶层不是 JSON 对象")
    return data


def _versions_of(obj: dict) -> dict:
    """从任意一层对象里抽出三件套（缺字段记为 None，便于报出「漏写」而不是 TypeError）。"""
    return {key: obj.get(key) for key in VERSION_KEYS}


def check_dir(out_dir: Path) -> list[str]:
    """检查一个输出目录，返回人类可读的问题列表（空列表 = 通过）。"""
    problems: list[str] = []

    manifest = _load_json(out_dir / "manifest.json")
    summary = _load_json(out_dir / "summary.json")

    man = _versions_of(manifest)
    for key, value in man.items():
        # 版本号必须是正整数：0 或字符串都说明序列化路径出了问题，
        # 而且 Web UI 侧的「版本不匹配」判断会被这种值污染。
        if not isinstance(value, int) or value <= 0:
            problems.append(f"manifest.json 的 {key}={value!r} 不是正整数")
    listed = manifest.get("versions")
    if isinstance(listed, dict):
        for key in VERSION_KEYS:
            if key in listed and listed[key] != man.get(key):
                problems.append(
                    f"manifest.json 内 versions.{key}={listed[key]} 与顶层 {key}={man.get(key)} 不一致"
                )

    summ = _versions_of(summary)
    if summ != man:
        problems.append(f"summary.json 三件套 {summ} 与 manifest.json {man} 不一致")

    # matches.jsonl：逐行解析比按行 split 更抗格式漂移（例如中间出现空行）。
    matches_path = out_dir / "matches.jsonl"
    if not matches_path.exists():
        problems.append(f"缺少 {matches_path}")
    else:
        with matches_path.open("r", encoding="utf-8") as fh:
            for lineno, line in enumerate(fh, start=1):
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                except json.JSONDecodeError as exc:
                    problems.append(f"matches.jsonl:{lineno} 不是合法 JSON：{exc}")
                    continue
                got = row.get("map_gen_version")
                if got != man.get("map_gen_version"):
                    problems.append(
                        f"matches.jsonl:{lineno} map_gen_version={got!r} 与 manifest 的 {man.get('map_gen_version')!r} 不一致"
                    )

    replays = sorted((out_dir / "replays").glob("*.jsonl"))
    if not replays:
        problems.append(f"{out_dir}/replays 下没有回放文件")
    for path in replays:
        with path.open("r", encoding="utf-8") as fh:
            first = fh.readline().strip()
        try:
            init = json.loads(first)
        except json.JSONDecodeError as exc:
            problems.append(f"{path.name}: 首行不是合法 JSON：{exc}")
            continue
        if init.get("type") != "init":
            problems.append(f"{path.name}: 首行 type={init.get('type')!r}，应为 init")
            continue
        got = _versions_of(init)
        if got != man:
            problems.append(f"{path.name}: init 三件套 {got} 与 manifest {man} 不一致")

    return problems


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__)
        return 2
    failed = False
    for raw in argv[1:]:
        out_dir = Path(raw)
        print(f"== 检查 {out_dir}")
        problems = check_dir(out_dir)
        if not problems:
            print("   OK：manifest / summary / matches / replays 版本号完全一致")
            continue
        failed = True
        for item in problems:
            print(f"   [不一致] {item}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
