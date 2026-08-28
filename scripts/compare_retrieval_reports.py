#!/usr/bin/env python3
"""retrieval-benchmark JSON reportの挙動中立性を突合する。

用途: 派生索引registryのようなsearch挙動を変えないはずの変更で、
baselineと変更後のreport(official / holdout)を比較し、時間値以外の
全case metricsが完全一致することを機械的に証明する。

- 比較対象: 各caseの各strategy(candidate_ids / selected / recall / precision /
  token / spill / depth内訳 / gate判定 / body_requirements など全て)と
  summaries・gate・linked_minus_top3
- 除外: 実行時間のみ(*_elapsed_us / p50_elapsed_us / p95_elapsed_us)。
  レポート規約どおり時間値は効果判定に使わない
- 終了code: 一致=0、差分あり=1(差分は全て標準出力へ列挙)

使い方:
    python3 scripts/compare_retrieval_reports.py BASE.json AFTER.json [--strategy linked_v1]

--strategy を省略すると全strategyを比較する(挙動中立の変更ではこちらが既定で通るべき)。
"""

import argparse
import json
import sys

TIME_KEYS = frozenset(
    {
        "search_elapsed_us",
        "retrieval_elapsed_us",
        "total_elapsed_us",
        "p50_elapsed_us",
        "p95_elapsed_us",
    }
)


def strip_time(value):
    """時間値のkeyだけを再帰的に落とす。"""
    if isinstance(value, dict):
        return {k: strip_time(v) for k, v in value.items() if k not in TIME_KEYS}
    if isinstance(value, list):
        return [strip_time(item) for item in value]
    return value


def diff_values(path, base, after, out):
    if isinstance(base, dict) and isinstance(after, dict):
        for key in sorted(set(base) | set(after)):
            if key not in base:
                out.append(f"{path}.{key}: baselineに無いkeyが増えた")
            elif key not in after:
                out.append(f"{path}.{key}: 変更後にkeyが消えた")
            else:
                diff_values(f"{path}.{key}", base[key], after[key], out)
    elif isinstance(base, list) and isinstance(after, list):
        if len(base) != len(after):
            out.append(f"{path}: 要素数 {len(base)} → {len(after)}")
        for index, (b, a) in enumerate(zip(base, after)):
            diff_values(f"{path}[{index}]", b, a, out)
    elif base != after:
        out.append(f"{path}: {base!r} → {after!r}")


def case_map(section):
    return {case["id"]: case for case in section.get("cases", [])}


def strategy_map(case):
    return {entry["strategy"]: entry for entry in case.get("strategies", [])}


def compare_section(name, base, after, strategies, out):
    base_cases = case_map(base)
    after_cases = case_map(after)
    for case_id in sorted(set(base_cases) | set(after_cases)):
        if case_id not in after_cases:
            out.append(f"{name}.{case_id}: 変更後reportからcaseが消えた")
            continue
        if case_id not in base_cases:
            out.append(f"{name}.{case_id}: baselineに無いcaseが増えた")
            continue
        base_case = strip_time(base_cases[case_id])
        after_case = strip_time(after_cases[case_id])
        for key in ("query", "surface", "required", "relevant", "excluded", "search_degraded", "stable"):
            diff_values(f"{name}.{case_id}.{key}", base_case.get(key), after_case.get(key), out)
        base_strategies = strategy_map(base_case)
        after_strategies = strategy_map(after_case)
        targets = strategies or sorted(set(base_strategies) | set(after_strategies))
        for strategy in targets:
            diff_values(
                f"{name}.{case_id}.{strategy}",
                base_strategies.get(strategy),
                after_strategies.get(strategy),
                out,
            )
    for key in ("summaries", "gate", "linked_minus_top3", "case_count", "search_configuration", "strategy_configurations"):
        base_value = strip_time(base.get(key))
        after_value = strip_time(after.get(key))
        if strategies and key == "summaries":
            base_value = [s for s in (base_value or []) if s.get("strategy") in strategies]
            after_value = [s for s in (after_value or []) if s.get("strategy") in strategies]
        diff_values(f"{name}.{key}", base_value, after_value, out)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base")
    parser.add_argument("after")
    parser.add_argument(
        "--strategy",
        action="append",
        help="比較するstrategy名(複数可)。省略時は全strategy",
    )
    args = parser.parse_args()
    with open(args.base, encoding="utf-8") as handle:
        base = json.load(handle)
    with open(args.after, encoding="utf-8") as handle:
        after = json.load(handle)

    diffs = []
    for key in ("schema_version", "core_version", "fixture_note_count"):
        diff_values(key, base.get(key), after.get(key), diffs)
    for section in ("controls", "challenges"):
        compare_section(section, base.get(section, {}), after.get(section, {}), args.strategy, diffs)

    if diffs:
        print(f"NG: {len(diffs)}件の差分(時間値除外後)")
        for line in diffs:
            print(f"  {line}")
        return 1
    scope = ", ".join(args.strategy) if args.strategy else "全strategy"
    print(f"OK: 時間値を除く全case metricsが完全一致({scope})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
