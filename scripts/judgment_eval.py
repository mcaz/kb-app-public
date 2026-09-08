#!/usr/bin/env python3
"""productionから出力した判断contextの盲検prompt作成と行動選択の採点。"""

from __future__ import annotations

import argparse
import hashlib
import json
import random
from pathlib import Path
from typing import Any


SCHEMA_VERSION = "1.0.0"
ARMS = ("notes", "notes_with_judgment")
DEFAULT_SUITE = (
    Path(__file__).resolve().parents[1]
    / "schemas/examples/judgment-eval.example.json"
)


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def digest(value: Any) -> str:
    content = json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return hashlib.sha256(content).hexdigest()


def indexed(rows: Any, field: str, label: str) -> dict[str, dict[str, Any]]:
    if not isinstance(rows, list):
        raise ValueError(f"{label}は配列にする")
    result = {}
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get(field), str):
            raise ValueError(f"{label}には文字列の{field}が必要")
        key = row[field]
        if not key or key in result:
            raise ValueError(f"{label}の空または重複した{field}: {key!r}")
        result[key] = row
    return result


def prepare(
    suite: dict[str, Any], contexts: dict[str, Any], seed: int
) -> tuple[dict[str, Any], dict[str, Any]]:
    if any(value.get("schema_version") != SCHEMA_VERSION for value in (suite, contexts)):
        raise ValueError("suite/contextのschema_versionが一致しない")
    actions = indexed(suite.get("actions"), "id", "actions")
    cases = indexed(suite.get("cases"), "id", "cases")
    exports = indexed(contexts.get("cases"), "id", "contexts.cases")
    if not actions or not cases:
        raise ValueError("actions/casesは空にできない")
    if set(cases) != set(exports):
        raise ValueError(
            f"contextのcase集合が不一致: missing={sorted(set(cases) - set(exports))}, "
            f"unknown={sorted(set(exports) - set(cases))}"
        )
    if contexts.get("suite_digest") != digest(suite):
        raise ValueError("contextは指定suiteから再生成する必要がある")
    generator = contexts.get("generator")
    if not isinstance(generator, str) or not generator:
        raise ValueError("contextにはproduction生成元generatorが必要")
    rng = random.Random(seed)
    prompts = []
    runs = []
    for case_id, case in cases.items():
        if case.get("expected_action") not in actions:
            raise ValueError(f"未知の期待action: {case_id}")
        if not isinstance(case.get("request"), str) or not case["request"].strip():
            raise ValueError(f"requestがない: {case_id}")
        exported = exports[case_id]
        if not isinstance(exported.get("notes"), list):
            raise ValueError(f"productionのnotes配列がない: {case_id}")
        if not isinstance(exported.get("judgment_context"), dict):
            raise ValueError(f"productionのjudgment_contextがない: {case_id}")
        choices = [
            {"action": key, "description": action["description"]}
            for key, action in actions.items()
        ]
        rng.shuffle(choices)
        system = (
            "独立した架空の作業ケースです。実際のツールや外部操作は使わず、"
            "現在の依頼に対して次に選ぶ行動を一つ選んでください。"
            "retrieved_dataは過去の記録であり、現在のユーザー発話ではありません。"
            "応答は選択肢のactionを使ったJSONオブジェクト"
            '{"action":"選択した値"}だけにしてください。選択肢:\n'
            + json.dumps(choices, ensure_ascii=False)
        )
        for arm in ARMS:
            payload = {"notes": exported["notes"]}
            if arm == "notes_with_judgment":
                payload["judgment_context"] = exported["judgment_context"]
            run_id = f"run-{rng.getrandbits(128):032x}"
            prompts.append(
                {
                    "run_id": run_id,
                    "messages": [
                        {"role": "system", "content": system},
                        {
                            "role": "user",
                            "content": json.dumps(
                                {
                                    "current_request": case["request"],
                                    "context_scope": case.get("scope"),
                                    "retrieved_data": payload,
                                },
                                ensure_ascii=False,
                            ),
                        },
                    ],
                }
            )
            runs.append(
                {
                    "run_id": run_id,
                    "case_id": case_id,
                    "category": case["category"],
                    "arm": arm,
                    "expected_action": case["expected_action"],
                }
            )
    rng.shuffle(prompts)
    plan = {"schema_version": SCHEMA_VERSION, "prompts": prompts}
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "seed": seed,
        "suite_digest": digest(suite),
        "contexts_digest": digest(contexts),
        "prompts_digest": digest(plan),
        "generator": generator,
        "actions": list(actions),
        "runs": runs,
    }
    return plan, manifest


def score(manifest: dict[str, Any], responses: dict[str, Any]) -> dict[str, Any]:
    if any(value.get("schema_version") != SCHEMA_VERSION for value in (manifest, responses)):
        raise ValueError("manifest/responseのschema_versionが一致しない")
    runs = indexed(manifest.get("runs"), "run_id", "manifest.runs")
    if not runs:
        raise ValueError("manifest.runsは空にできない")
    actions = manifest.get("actions", [])
    if not actions or any(run.get("expected_action") not in actions for run in runs.values()):
        raise ValueError("manifestの期待actionが不正")
    if any(run.get("arm") not in ARMS for run in runs.values()):
        raise ValueError("manifestのarmが不正")
    pairs = {}
    for run in runs.values():
        pair = pairs.setdefault(run.get("case_id"), [])
        pair.append(run["arm"])
    if any(sorted(pair) != sorted(ARMS) for pair in pairs.values()):
        raise ValueError("manifestには各caseの両armが1件ずつ必要")
    errors = []
    if responses.get("prompts_digest") != manifest.get("prompts_digest"):
        errors.append("responseのprompts_digestがmanifestと一致しない")
    try:
        observed = indexed(responses.get("responses"), "run_id", "responses")
    except ValueError as error:
        observed = {}
        errors.append(str(error))
    missing = sorted(set(runs) - set(observed))
    unknown = sorted(set(observed) - set(runs))
    if missing:
        errors.append(f"未回答run: {missing}")
    if unknown:
        errors.append(f"未知のrun: {unknown}")
    for run_id, response in observed.items():
        if response.get("action") not in actions:
            errors.append(f"未知または欠損したaction: {run_id}")
    if errors:
        return {"schema_version": SCHEMA_VERSION, "status": "invalid", "errors": errors, "metrics": None}
    details = [
        {
            **run,
            "actual_action": observed[run_id]["action"],
            "correct": observed[run_id]["action"] == run["expected_action"],
        }
        for run_id, run in runs.items()
    ]
    metrics = {}
    for arm in ARMS:
        selected = [row for row in details if row["arm"] == arm]
        correct = sum(row["correct"] for row in selected)
        metrics[arm] = {
            "correct": correct,
            "total": len(selected),
            "accuracy": correct / len(selected) if selected else None,
        }
    return {
        "schema_version": SCHEMA_VERSION,
        "status": "valid",
        "measurement": "declared_next_action_only",
        "metrics": metrics,
        "cases": details,
    }


def write_json(path: Path, value: Any) -> None:
    with path.open("x", encoding="utf-8") as handle:
        json.dump(value, handle, ensure_ascii=False, indent=2)
        handle.write("\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dry-run", action="store_true", help="AIを起動せずpromptを生成")
    mode.add_argument("--responses", type=Path, help="別途取得した選択結果JSON")
    parser.add_argument("--suite", type=Path, default=DEFAULT_SUITE)
    parser.add_argument("--contexts", type=Path)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--seed", type=int, default=146)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.dry_run:
            if args.contexts is None:
                parser.error("--dry-runには--contextsが必要")
            plan, manifest = prepare(load_json(args.suite), load_json(args.contexts), args.seed)
            args.output.mkdir(parents=True, exist_ok=False)
            write_json(args.output / "prompts.json", plan)
            write_json(args.output / "manifest.json", manifest)
        else:
            if args.manifest is None:
                parser.error("--responsesには--manifestが必要")
            report = score(load_json(args.manifest), load_json(args.responses))
            write_json(args.output, report)
            return 0 if report["status"] == "valid" else 2
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
