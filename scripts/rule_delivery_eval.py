#!/usr/bin/env python3
"""Run the fixed Rule Delivery Matrix against Claude Code or Codex.

The runner creates a disposable kb-app Vault for every case/run, exposes only
the evaluation MCP server, records both client and server JSONL, and emits the
client-neutral trace schema consumed by `kb eval rule-delivery-score`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Iterable


SCHEMA_VERSION = "1.0.0"
MODES = ("semantic_only", "rules_top_k", "always_topic", "always_topic_event")
KB_TOOLS = (
    "search", "get", "attach", "recent", "tag_vocabulary", "set_tag_vocabulary_source",
    "plan_tag_vocabulary_change", "apply_tag_vocabulary_change",
    "list_tag_vocabulary_changes", "get_tag_vocabulary_change",
    "plan_tag_vocabulary_rollback", "rollback_tag_vocabulary_change", "get_tag_vocabulary_stats",
    "propose", "update", "remove"
)
ONE_PIXEL_PNG_BASE64 = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
)
AUTO_RETRIEVAL_SKIP_MARKER = (
    "<task-notification>rule-delivery-eval-isolated-fixture</task-notification>"
)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    repo = Path(__file__).resolve().parents[1]
    executable_suffix = ".exe" if os.name == "nt" else ""
    parser = argparse.ArgumentParser(
        description="Run and normalize the fixed 20-case Rule Delivery evaluation."
    )
    parser.add_argument(
        "--client",
        action="append",
        choices=("claude", "codex"),
        help="Client to run; repeat for both (default: both).",
    )
    parser.add_argument(
        "--suite",
        type=Path,
        default=repo / "schemas/examples/rule-delivery-eval.example.json",
    )
    parser.add_argument(
        "--kb-bin", type=Path, default=repo / f"target/debug/kb{executable_suffix}"
    )
    parser.add_argument(
        "--eval-mcp-bin",
        type=Path,
        default=repo / f"target/debug/kb-rule-eval-mcp{executable_suffix}",
    )
    parser.add_argument("--modes", default=",".join(MODES))
    parser.add_argument("--cases", default="all")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--model")
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--output", type=Path, default=Path("rule-delivery-traces.json"))
    parser.add_argument("--raw-dir", type=Path)
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Validate and print the matrix without launching an AI client.",
    )
    return parser.parse_args(argv)


def csv_values(raw: str, allowed: Iterable[str], label: str) -> list[str]:
    allowed_values = tuple(allowed)
    if raw == "all":
        return list(allowed_values)
    values = [part.strip() for part in raw.split(",") if part.strip()]
    unknown = sorted(set(values) - set(allowed_values))
    if unknown:
        raise ValueError(f"unknown {label}: {', '.join(unknown)}")
    if not values:
        raise ValueError(f"{label} must not be empty")
    return values


def run_command(
    command: list[str], *, cwd: Path | None = None, timeout: int = 60
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=cwd,
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )


def text_output(value: str | bytes | None) -> str:
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value or ""


def require_success(result: subprocess.CompletedProcess[str], description: str) -> str:
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise RuntimeError(f"{description} failed ({result.returncode}): {detail}")
    return result.stdout


def load_suite(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as handle:
        suite = json.load(handle)
    return suite


def load_plan(kb_bin: Path, suite: Path, mode: str) -> dict[str, Any]:
    result = run_command(
        [
            str(kb_bin),
            "eval",
            "rule-delivery-plan",
            "--suite",
            str(suite),
            "--mode",
            mode.replace("_", "-"),
            "--format",
            "json",
        ]
    )
    return json.loads(require_success(result, f"plan {mode}"))


def create_fixture(kb_bin: Path, suite: Path, output: Path) -> None:
    result = run_command(
        [
            str(kb_bin),
            "eval",
            "rule-delivery-fixture",
            "--suite",
            str(suite),
            "--output",
            str(output),
        ],
        timeout=120,
    )
    require_success(result, "fixture creation")


def mcp_arguments(
    eval_mcp_bin: Path,
    fixture: Path,
    suite: Path,
    mode: str,
    case_id: str,
    trace_path: Path,
    client: str,
) -> list[str]:
    return [
        str(eval_mcp_bin),
        "--vault",
        str(fixture),
        "--suite",
        str(suite),
        "--mode",
        mode.replace("_", "-"),
        "--case",
        case_id,
        "--trace",
        str(trace_path),
        "--client",
        f"rule-delivery-eval/{client}",
    ]


def prompt_for(case: dict[str, Any]) -> str:
    prompt = str(case["prompt"])
    if case["id"] == "E4":
        prompt += (
            "\n\n評価用に受領した sample.png の内容(Base64)は次です。"
            "添付ツールへはこの内容をそのまま渡してください。\n"
            f"{ONE_PIXEL_PNG_BASE64}"
        )
    return f"{prompt}\n\n{AUTO_RETRIEVAL_SKIP_MARKER}"


def claude_command(
    mcp_args: list[str], prompt: str, model: str | None, config_path: Path
) -> list[str]:
    config = {
        "mcpServers": {
            "kb_rule_eval": {
                "type": "stdio",
                "command": mcp_args[0],
                "args": mcp_args[1:],
            }
        }
    }
    config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
    tools = ",".join(f"mcp__kb_rule_eval__{name}" for name in KB_TOOLS)
    command = [
        "claude",
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        str(config_path),
        "--tools",
        tools,
        "--allowedTools",
        tools,
        "--permission-mode",
        "dontAsk",
    ]
    if model:
        command.extend(["--model", model])
    command.append(prompt)
    return command


def toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def codex_command(
    mcp_args: list[str], prompt: str, model: str | None, fixture: Path
) -> list[str]:
    server = "mcp_servers.kb_rule_eval"
    enabled_tools = json.dumps(list(KB_TOOLS), ensure_ascii=False)
    command = [
        "codex",
        "exec",
        "--json",
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
        "-s",
        "workspace-write",
        "-C",
        str(fixture),
        "-c",
        'approval_policy="never"',
        "-c",
        f"{server}.command={toml_string(mcp_args[0])}",
        "-c",
        f"{server}.args={json.dumps(mcp_args[1:], ensure_ascii=False)}",
        "-c",
        f"{server}.required=true",
        "-c",
        f"{server}.enabled_tools={enabled_tools}",
        "-c",
        f'{server}.default_tools_approval_mode="approve"',
    ]
    if model:
        command.extend(["--model", model])
    command.append(prompt)
    return command


def json_lines(text: str) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    for line in text.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            events.append(value)
    return events


def input_token_total(usage: Any) -> int | None:
    if not isinstance(usage, dict):
        return None
    values = [
        int(value)
        for key, value in usage.items()
        if key.endswith("input_tokens") and isinstance(value, (int, float))
    ]
    return sum(values) if values else None


def parse_claude_output(
    stdout: str, fallback_model: str | None
) -> tuple[str, str | None, str, int | None, int | None, list[tuple[str, str | None]]]:
    model = fallback_model or "claude-default"
    version: str | None = None
    response = ""
    latency: int | None = None
    input_tokens: int | None = None
    pre_tool: list[tuple[str, str | None]] = []
    pending_text: list[str] = []
    all_text: list[str] = []
    errors: list[str] = []
    for event in json_lines(stdout):
        event_type = event.get("type")
        if event_type == "system" and event.get("subtype") == "init":
            model = str(event.get("model") or model)
            raw_version = event.get("claude_code_version")
            version = str(raw_version) if raw_version else version
        elif event_type == "assistant":
            message = event.get("message") or {}
            if message.get("model"):
                model = str(message["model"])
            for block in message.get("content") or []:
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "text":
                    value = str(block.get("text") or "")
                    pending_text.append(value)
                    all_text.append(value)
                elif block.get("type") == "tool_use":
                    name = short_tool_name(str(block.get("name") or ""))
                    notice = "\n".join(pending_text).strip() or None
                    pre_tool.append((name, notice))
                    pending_text.clear()
        elif event_type == "result":
            if event.get("result") is not None:
                response = str(event.get("result") or "")
            if isinstance(event.get("duration_ms"), (int, float)):
                latency = int(event["duration_ms"])
            input_tokens = input_token_total(event.get("usage")) or input_tokens
            error_values = event.get("errors") or []
            errors.extend(str(value) for value in error_values)
        elif event_type == "error":
            errors.append(str(event.get("error") or event.get("message") or event))
    if not response:
        response = "\n".join(all_text).strip()
    if errors:
        response = "\n".join(part for part in [response, "Client error: " + "; ".join(errors)] if part)
    return model, version, response, input_tokens, latency, pre_tool


def parse_codex_output(
    stdout: str, fallback_model: str | None
) -> tuple[str, str | None, str, int | None, int | None, list[tuple[str, str | None]], list[dict[str, Any]]]:
    model = fallback_model or "codex-default"
    response_parts: list[str] = []
    input_tokens: int | None = None
    latency: int | None = None
    pre_tool: list[tuple[str, str | None]] = []
    builtin_calls: list[dict[str, Any]] = []
    pending_text: list[str] = []
    errors: list[str] = []
    for event in json_lines(stdout):
        event_type = str(event.get("type") or "")
        item = event.get("item") if isinstance(event.get("item"), dict) else {}
        item_type = str(item.get("type") or "")
        if event_type == "item.completed" and item_type == "agent_message":
            value = str(item.get("text") or item.get("message") or "")
            if value:
                response_parts.append(value)
                pending_text.append(value)
        elif event_type in ("item.started", "item.completed") and item_type == "mcp_tool_call":
            name = short_tool_name(str(item.get("tool") or item.get("name") or ""))
            if event_type == "item.started":
                notice = "\n".join(pending_text).strip() or None
                pre_tool.append((name, notice))
                pending_text.clear()
        elif event_type == "item.completed" and item_type in {
            "command_execution",
            "file_change",
            "web_search",
        }:
            builtin_calls.append(
                {
                    "name": f"builtin.{item_type}",
                    "pre_tool_text": "\n".join(pending_text).strip() or None,
                    "arguments": item,
                    "result": item.get("aggregated_output") or item.get("output") or {},
                    "is_error": str(item.get("status") or "").lower() in {"failed", "error"},
                }
            )
            pending_text.clear()
        elif event_type == "turn.completed":
            usage = event.get("usage") or {}
            input_tokens = input_token_total(usage) or input_tokens
        elif event_type in {"turn.failed", "error"}:
            errors.append(str(event.get("error") or event.get("message") or event))
    response = response_parts[-1] if response_parts else ""
    if errors:
        response = "\n".join(part for part in [response, "Client error: " + "; ".join(errors)] if part)
    return model, None, response, input_tokens, latency, pre_tool, builtin_calls


def short_tool_name(name: str) -> str:
    if "__" in name:
        return name.rsplit("__", 1)[-1]
    if "." in name:
        return name.rsplit(".", 1)[-1]
    return name


def nested_strings(value: Any, key: str) -> list[str]:
    found: list[str] = []
    if isinstance(value, dict):
        if isinstance(value.get(key), str):
            found.append(value[key])
        for child in value.values():
            found.extend(nested_strings(child, key))
    elif isinstance(value, list):
        for child in value:
            found.extend(nested_strings(child, key))
    return found


def parse_mcp_trace(path: Path) -> tuple[list[dict[str, Any]], list[str], list[str]]:
    calls: list[dict[str, Any]] = []
    event_rules: list[str] = []
    degradations: list[str] = []
    if not path.exists():
        return calls, event_rules, degradations
    for entry in json_lines(path.read_text(encoding="utf-8")):
        request = entry.get("request") or {}
        if request.get("method") != "tools/call":
            continue
        params = request.get("params") or {}
        response = entry.get("response") or {}
        result = response.get("result") or {}
        calls.append(
            {
                "name": str(params.get("name") or "unknown"),
                "pre_tool_text": None,
                "arguments": params.get("arguments") or {},
                "result": result,
                "is_error": bool(response.get("error") or result.get("isError")),
            }
        )
        event_rules.extend(nested_strings(result.get("structuredContent"), "rule_id"))
        degradations.extend(
            nested_strings(result.get("structuredContent"), "eval_injected_degradations")
        )
        injected = (
            result.get("structuredContent", {}).get("eval_injected_degradations", [])
            if isinstance(result.get("structuredContent"), dict)
            else []
        )
        if isinstance(injected, list):
            degradations.extend(str(value) for value in injected)
    return calls, unique_ordered(event_rules), unique_ordered(degradations)


def unique_ordered(values: Iterable[str]) -> list[str]:
    return list(dict.fromkeys(value for value in values if value))


def parse_initialize_evidence(
    path: Path,
) -> tuple[dict[str, Any] | None, dict[str, str] | None]:
    """サーバーが生成した応答の観測。stdout完了・host受信・モデル遵守は未確認。"""
    observations: list[dict[str, Any]] = []
    versions: list[dict[str, str]] = []
    if not path.exists():
        return None, None
    def object_value(value: Any) -> dict[str, Any]:
        return value if isinstance(value, dict) else {}

    for entry in json_lines(path.read_text(encoding="utf-8")):
        request = object_value(entry.get("request"))
        if request.get("method") != "initialize":
            continue
        response = object_value(entry.get("response"))
        result = object_value(response.get("result"))
        if response.get("error") or request.get("id") is None or response.get("id") != request["id"]:
            continue
        instructions = result.get("instructions")
        if not isinstance(instructions, str):
            continue
        capabilities = object_value(result.get("capabilities"))
        experimental = object_value(capabilities.get("experimental"))
        identity = object_value(experimental.get("kbApp")).get("rule_identity")
        observations.append({
            "source": "mcp_initialize_server_response",
            "instructions": instructions,
            "rule_identity": identity if isinstance(identity, dict) else None,
        })
        client_info = object_value(object_value(request.get("params")).get("clientInfo"))
        version = client_info.get("version")
        if isinstance(version, str) and version:
            versions.append({"value": version, "source": "mcp_initialize_request.clientInfo.version"})
    # 再接続で異なる応答を観測した場合、都合のよい1件を配信証拠に選ばない。
    observation = observations[0] if observations and all(item == observations[0] for item in observations) else None
    version = versions[0] if versions and all(item == versions[0] for item in versions) else None
    return observation, version


def observed_rule_ids(
    initialize: dict[str, Any] | None, prepared: dict[str, Any]
) -> list[str]:
    if initialize is None:
        return []
    parts = initialize["instructions"].split("\n\n[Delivered Rules]\n")
    expected_context = prepared.get("prompt_context") or "なし"
    if len(parts) != 2 or parts[1] != expected_context:
        return []
    # IDは実応答の規則本文と計画の完全一致から導出する。計画だけでは観測扱いにしない。
    return list(prepared.get("delivered_rule_ids") or [])


def response_client_error(response: str) -> str | None:
    return next(
        (line.strip() for line in response.splitlines() if line.strip().startswith("Client error:")),
        None,
    )


def attach_pre_tool_text(
    calls: list[dict[str, Any]], pre_tool: list[tuple[str, str | None]]
) -> None:
    cursor = 0
    for call in calls:
        for index in range(cursor, len(pre_tool)):
            name, notice = pre_tool[index]
            if name == call["name"]:
                call["pre_tool_text"] = notice
                cursor = index + 1
                break


def os_label() -> str:
    return f"{platform.system().lower()}-{platform.release()}"


def run_one(
    *,
    client: str,
    suite_path: Path,
    eval_mcp_bin: Path,
    mode: str,
    case: dict[str, Any],
    prepared: dict[str, Any],
    run_number: int,
    fixture: Path,
    raw_case_dir: Path,
    model_override: str | None,
    timeout: int,
) -> dict[str, Any]:
    suite_sha256 = hashlib.sha256(suite_path.read_bytes()).hexdigest()
    raw_case_dir.mkdir(parents=True, exist_ok=False)
    mcp_trace = raw_case_dir / "mcp.jsonl"
    mcp_args = mcp_arguments(
        eval_mcp_bin, fixture, suite_path, mode, case["id"], mcp_trace, client
    )
    prompt = prompt_for(case)
    if client == "claude":
        command = claude_command(mcp_args, prompt, model_override, raw_case_dir / "mcp.json")
    else:
        command = codex_command(mcp_args, prompt, model_override, fixture)
    (raw_case_dir / "command.json").write_text(
        json.dumps(command, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    started = time.monotonic()
    try:
        result = run_command(command, cwd=fixture, timeout=timeout)
        client_error = None
    except subprocess.TimeoutExpired as error:
        result = subprocess.CompletedProcess(
            command,
            124,
            stdout=text_output(error.stdout),
            stderr=text_output(error.stderr),
        )
        client_error = f"Client error: timeout after {timeout}s"
    wall_latency = int((time.monotonic() - started) * 1000)
    (raw_case_dir / "client.stdout.jsonl").write_text(result.stdout, encoding="utf-8")
    (raw_case_dir / "client.stderr.txt").write_text(result.stderr, encoding="utf-8")

    builtin_calls: list[dict[str, Any]] = []
    if client == "claude":
        parsed = parse_claude_output(result.stdout, model_override)
        model, version, response, input_tokens, client_latency, pre_tool = parsed
    else:
        parsed = parse_codex_output(result.stdout, model_override)
        model, version, response, input_tokens, client_latency, pre_tool, builtin_calls = parsed
    if result.returncode != 0 and not client_error:
        detail = result.stderr.strip().splitlines()[-1] if result.stderr.strip() else "no stderr"
        client_error = f"Client error: exit {result.returncode}: {detail}"
    if client_error:
        response = "\n".join(part for part in [response, client_error] if part)

    tool_calls, event_rule_ids, injected = parse_mcp_trace(mcp_trace)
    initialize, cli_version = parse_initialize_evidence(mcp_trace)
    if client == "claude" and version:
        cli_version = {"value": version, "source": "claude_system_init.claude_code_version"}
    attach_pre_tool_text(tool_calls, pre_tool)
    tool_calls.extend(builtin_calls)
    degraded_codes = unique_ordered(list(prepared.get("degraded_codes") or []) + injected)
    return {
        "case_id": case["id"],
        "mode": mode,
        "client_surface": client,
        "model": model,
        "model_version": None,
        "evidence": {
            "suite_sha256": suite_sha256,
            "cli_version": cli_version,
            "initialize_response": initialize,
        },
        "os": os_label(),
        "run": run_number,
        "delivered_rule_ids": observed_rule_ids(initialize, prepared),
        "event_rule_ids": event_rule_ids,
        "degraded_codes": degraded_codes,
        "tool_calls": tool_calls,
        "response": response,
        "input_tokens": input_tokens,
        "latency_ms": client_latency or wall_latency,
    }


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    clients = args.client or ["claude", "codex"]
    if args.runs < 1:
        raise ValueError("--runs must be at least 1")
    suite_path = args.suite.resolve()
    kb_bin = args.kb_bin.resolve()
    eval_mcp_bin = args.eval_mcp_bin.resolve()
    for path, label in ((suite_path, "suite"), (kb_bin, "kb binary"), (eval_mcp_bin, "eval MCP binary")):
        if not path.is_file():
            raise FileNotFoundError(f"{label} not found: {path}")
    # 長いmatrix中に元suiteが編集されても、plan・fixture・MCP・hashを同じ入力へ固定する。
    suite_bytes = suite_path.read_bytes()
    with tempfile.TemporaryDirectory(prefix="kb-rule-eval-") as temporary:
        fixed_suite = Path(temporary) / "suite.snapshot.json"
        fixed_suite.write_bytes(suite_bytes)
        return run_matrix(args, clients, kb_bin, eval_mcp_bin, fixed_suite, Path(temporary))


def run_matrix(
    args: argparse.Namespace,
    clients: list[str],
    kb_bin: Path,
    eval_mcp_bin: Path,
    suite_path: Path,
    temporary: Path,
) -> int:
    suite = load_suite(suite_path)
    case_map = {case["id"]: case for case in suite.get("cases", [])}
    modes = csv_values(args.modes, MODES, "mode")
    case_ids = csv_values(args.cases, case_map, "case")
    plans = {mode: load_plan(kb_bin, suite_path, mode) for mode in modes}
    prepared = {
        (mode, case["case_id"]): case
        for mode, plan in plans.items()
        for case in plan["cases"]
    }
    planned_runs = len(clients) * len(modes) * len(case_ids) * args.runs
    if args.dry_run:
        print(
            json.dumps(
                {
                    "clients": clients,
                    "modes": modes,
                    "cases": case_ids,
                    "runs_each": args.runs,
                    "total_runs": planned_runs,
                },
                ensure_ascii=False,
                indent=2,
            )
        )
        return 0

    output = args.output.resolve()
    raw_dir = (args.raw_dir or output.with_suffix("").with_name(output.stem + "-raw")).resolve()
    if output.exists():
        raise ValueError(f"output already exists: {output}")
    if raw_dir.exists() and any(raw_dir.iterdir()):
        raise ValueError(f"raw directory is not empty: {raw_dir}")
    output.parent.mkdir(parents=True, exist_ok=True)
    raw_dir.mkdir(parents=True, exist_ok=True)
    # 一時実行先を破棄した後にも、trace digestの元バイト列で再採点できるよう残す。
    with (raw_dir / "suite.snapshot.json").open("xb") as saved_suite:
        saved_suite.write(suite_path.read_bytes())
    traces: list[dict[str, Any]] = []
    fixture_template = Path(temporary) / "fixture-template"
    create_fixture(kb_bin, suite_path, fixture_template)
    completed = 0
    for client in clients:
        for mode in modes:
            for case_id in case_ids:
                for run_number in range(1, args.runs + 1):
                    completed += 1
                    label = f"{client}/{mode}/{case_id}/run-{run_number}"
                    print(f"[{completed}/{planned_runs}] {label}", file=sys.stderr, flush=True)
                    fixture = Path(temporary) / f"fixture-{completed}"
                    shutil.copytree(fixture_template, fixture)
                    raw_case_dir = raw_dir / client / mode / case_id / f"run-{run_number}"
                    trace = run_one(
                        client=client,
                        suite_path=suite_path,
                        eval_mcp_bin=eval_mcp_bin,
                        mode=mode,
                        case=case_map[case_id],
                        prepared=prepared[(mode, case_id)],
                        run_number=run_number,
                        fixture=fixture,
                        raw_case_dir=raw_case_dir,
                        model_override=args.model,
                        timeout=args.timeout,
                    )
                    traces.append(trace)
                    output.write_text(
                        json.dumps(
                            {"schema_version": SCHEMA_VERSION, "traces": traces},
                            ensure_ascii=False,
                            indent=2,
                        )
                        + "\n",
                        encoding="utf-8",
                    )
                    client_error = response_client_error(str(trace.get("response") or ""))
                    if client_error:
                        raise RuntimeError(f"{label} failed before valid inference: {client_error}")
    print(output)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (FileNotFoundError, RuntimeError, ValueError) as error:
        print(f"rule_delivery_eval: {error}", file=sys.stderr)
        raise SystemExit(2)
