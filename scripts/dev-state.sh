#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/dev-state.sh [options]

Emit a kb-app development Context Pack as JSON.

Options:
  --repo PATH             Git worktree (default: current directory)
  --mode MODE             summary, detail, or full (default: summary)
  --id ID                 Knowledge ID required by detail mode
  --checks PATH           Override .kb-dev/checks.json
  --handoff PATH          Override .kb-dev/handoff.json
  --knowledge-file PATH   Adapter input used by tests/future MCP implementations
  --max-knowledge N       Maximum summary items (default: 5)
  --now ISO8601           Fixed clock for deterministic contract tests
  -h, --help              Show this help

The command is read-only. It never commits, pushes, or creates GitHub issues.
EOF
}

repo="."
mode="summary"
detail_id=""
checks_path=""
handoff_path=""
knowledge_path=""
max_knowledge="5"
fixed_now=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      repo=${2:?--repo requires a path}
      shift 2
      ;;
    --mode)
      mode=${2:?--mode requires a value}
      shift 2
      ;;
    --id)
      detail_id=${2:?--id requires a value}
      shift 2
      ;;
    --checks)
      checks_path=${2:?--checks requires a path}
      shift 2
      ;;
    --handoff)
      handoff_path=${2:?--handoff requires a path}
      shift 2
      ;;
    --knowledge-file)
      knowledge_path=${2:?--knowledge-file requires a path}
      shift 2
      ;;
    --max-knowledge)
      max_knowledge=${2:?--max-knowledge requires a value}
      shift 2
      ;;
    --now)
      fixed_now=${2:?--now requires a value}
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$mode" in
  summary|detail|full) ;;
  *)
    echo "--mode must be summary, detail, or full" >&2
    exit 2
    ;;
esac

if [ "$mode" = "detail" ] && [ -z "$detail_id" ]; then
  echo "--id is required in detail mode" >&2
  exit 2
fi

case "$max_knowledge" in
  ''|*[!0-9]*)
    echo "--max-knowledge must be a non-negative integer" >&2
    exit 2
    ;;
esac

python3 - "$repo" "$mode" "$detail_id" "$checks_path" "$handoff_path" \
  "$knowledge_path" "$max_knowledge" "$fixed_now" <<'PY'
from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import pathlib
import re
import stat
import subprocess
import sys
from typing import Any


SCHEMA_VERSION = "1.0.0"
STAMP_PREFIX = "sha256:"
HEX_RE = re.compile(r"^[0-9a-f]{40,64}$")
STAMP_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
FRESHNESS_RANK = {
    "valid": 0,
    "needs_review": 1,
    "unknown": 2,
    "superseded": 3,
}


def fail(message: str) -> None:
    raise SystemExit(message)


def parse_time(value: str) -> dt.datetime:
    if value.endswith("Z"):
        value = value[:-1] + "+00:00"
    parsed = dt.datetime.fromisoformat(value)
    if parsed.tzinfo is None:
        fail(f"timestamp must include a timezone: {value}")
    return parsed.astimezone(dt.timezone.utc)


def format_time(value: dt.datetime) -> str:
    return value.astimezone(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def git(root: pathlib.Path, *args: str, check: bool = True) -> bytes:
    proc = subprocess.run(
        ["git", "-C", str(root), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and proc.returncode != 0:
        fail(proc.stderr.decode("utf-8", "replace").strip() or "git command failed")
    return proc.stdout


def git_text(root: pathlib.Path, *args: str, check: bool = True) -> str:
    return git(root, *args, check=check).decode("utf-8", "surrogateescape").strip()


def add_hash_record(hasher: Any, label: bytes, value: bytes) -> None:
    hasher.update(len(label).to_bytes(8, "big"))
    hasher.update(label)
    hasher.update(len(value).to_bytes(8, "big"))
    hasher.update(value)


def add_path_state(hasher: Any, root: pathlib.Path, rel_bytes: bytes) -> None:
    rel = os.fsdecode(rel_bytes)
    path = root / rel
    add_hash_record(hasher, b"path", rel_bytes)
    try:
        info = path.lstat()
    except FileNotFoundError:
        add_hash_record(hasher, b"kind", b"missing")
        return

    add_hash_record(hasher, b"mode", oct(stat.S_IMODE(info.st_mode)).encode())
    if path.is_symlink():
        add_hash_record(hasher, b"kind", b"symlink")
        add_hash_record(hasher, b"content", os.fsencode(os.readlink(path)))
    elif path.is_file():
        add_hash_record(hasher, b"kind", b"file")
        with path.open("rb") as handle:
            content_hasher = hashlib.sha256()
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                content_hasher.update(chunk)
        add_hash_record(hasher, b"content-sha256", content_hasher.digest())
    else:
        add_hash_record(hasher, b"kind", b"other")


def worktree_stamp(root: pathlib.Path, head: str | None) -> str:
    """Hash HEAD, index entries, and all current tracked/untracked file bytes."""
    hasher = hashlib.sha256()
    add_hash_record(hasher, b"format", b"kb-dev-worktree-v1")
    add_hash_record(hasher, b"head", (head or "unborn").encode())

    index = git(root, "ls-files", "--stage", "-z")
    add_hash_record(hasher, b"index", index)

    tracked = sorted(set(filter(None, git(root, "ls-files", "-z").split(b"\0"))))
    untracked = sorted(
        set(filter(None, git(root, "ls-files", "--others", "--exclude-standard", "-z").split(b"\0")))
    )
    for rel in tracked:
        add_hash_record(hasher, b"tracked", rel)
        add_path_state(hasher, root, rel)
    for rel in untracked:
        add_hash_record(hasher, b"untracked", rel)
        add_path_state(hasher, root, rel)
    return STAMP_PREFIX + hasher.hexdigest()


def status_counts(root: pathlib.Path) -> dict[str, int]:
    fields = git(root, "status", "--porcelain=v1", "-z", "--untracked-files=all").split(b"\0")
    counts = {"staged": 0, "unstaged": 0, "untracked": 0}
    index = 0
    while index < len(fields):
        entry = fields[index]
        index += 1
        if not entry:
            continue
        if entry.startswith(b"??"):
            counts["untracked"] += 1
            continue
        if len(entry) < 2:
            continue
        x, y = chr(entry[0]), chr(entry[1])
        if x not in (" ", "?"):
            counts["staged"] += 1
        if y not in (" ", "?"):
            counts["unstaged"] += 1
        if x in ("R", "C") or y in ("R", "C"):
            index += 1
    return counts


def load_json(path: pathlib.Path) -> Any:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def is_string_list(value: Any, *, nonempty: bool = False) -> bool:
    return (
        isinstance(value, list)
        and (not nonempty or bool(value))
        and all(isinstance(item, str) and bool(item) for item in value)
    )


def valid_checks(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    if set(value) != {"schema_version", "recorded_for", "recorded_at", "scope", "commands"}:
        return False
    recorded_for = value.get("recorded_for")
    if not isinstance(recorded_for, dict) or set(recorded_for) != {
        "branch",
        "base_commit",
        "state_stamp",
    }:
        return False
    branch = recorded_for.get("branch")
    base = recorded_for.get("base_commit")
    if branch is not None and (not isinstance(branch, str) or not branch):
        return False
    if base is not None and (not isinstance(base, str) or HEX_RE.fullmatch(base) is None):
        return False
    if not isinstance(recorded_for.get("state_stamp"), str) or STAMP_RE.fullmatch(
        recorded_for["state_stamp"]
    ) is None:
        return False
    if value.get("schema_version") != SCHEMA_VERSION or value.get("scope") not in {
        "full",
        "partial",
        "unknown",
    }:
        return False
    try:
        parse_time(value.get("recorded_at", ""))
    except (ValueError, TypeError, SystemExit):
        return False
    commands = value.get("commands")
    if not isinstance(commands, list) or not commands:
        return False
    allowed = {"command", "exit_code", "started_at", "finished_at", "duration_ms"}
    for command in commands:
        if not isinstance(command, dict) or not {"command", "exit_code"}.issubset(command):
            return False
        if not set(command).issubset(allowed):
            return False
        if not isinstance(command["command"], str) or not command["command"]:
            return False
        if isinstance(command["exit_code"], bool) or not isinstance(command["exit_code"], int):
            return False
        if command["exit_code"] < 0:
            return False
        for field in ("started_at", "finished_at"):
            if field in command:
                try:
                    parse_time(command[field])
                except (ValueError, TypeError, SystemExit):
                    return False
        if "duration_ms" in command and (
            isinstance(command["duration_ms"], bool)
            or not isinstance(command["duration_ms"], int)
            or command["duration_ms"] < 0
        ):
            return False
    return True


def valid_handoff(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    required = {
        "schema_version",
        "task",
        "next",
        "discarded",
        "open_questions",
        "branch",
        "base_commit",
        "created_at",
        "updated_at",
        "expires_at",
    }
    if not required.issubset(value) or not set(value).issubset(required | {"promotion"}):
        return False
    if value.get("schema_version") != SCHEMA_VERSION:
        return False
    if not isinstance(value.get("task"), str) or not value["task"]:
        return False
    if not is_string_list(value.get("next"), nonempty=True):
        return False
    if not is_string_list(value.get("discarded")) or not is_string_list(value.get("open_questions")):
        return False
    branch, base = value.get("branch"), value.get("base_commit")
    if branch is not None and (not isinstance(branch, str) or not branch):
        return False
    if base is not None and (not isinstance(base, str) or HEX_RE.fullmatch(base) is None):
        return False
    try:
        for field in ("created_at", "updated_at", "expires_at"):
            parse_time(value[field])
    except (ValueError, TypeError, SystemExit):
        return False
    promotion = value.get("promotion")
    if promotion is not None:
        if not isinstance(promotion, dict) or promotion.get("provider") != "github":
            return False
        if (
            not isinstance(promotion.get("issue_url"), str)
            or re.fullmatch(r"https://github\.com/[^/]+/[^/]+/issues/[1-9][0-9]*", promotion["issue_url"])
            is None
        ):
            return False
        if not set(promotion).issubset({"provider", "issue_url", "issue_number"}):
            return False
        if "issue_number" in promotion and (
            isinstance(promotion["issue_number"], bool)
            or not isinstance(promotion["issue_number"], int)
            or promotion["issue_number"] < 1
        ):
            return False
    return True


def warning(code: str, message: str, severity: str = "warning") -> dict[str, str]:
    return {
        "id": f"warning:{code}",
        "code": code,
        "severity": severity,
        "message": message,
    }


def checks_context(
    path: pathlib.Path,
    branch: str | None,
    head: str | None,
    stamp: str,
    warnings: list[dict[str, str]],
) -> dict[str, Any]:
    empty = {
        "validity": "unknown",
        "scope": "unknown",
        "result": "unknown",
        "recorded_scope": "unknown",
        "recorded_result": "unknown",
        "commands": [],
    }
    if not path.exists():
        warnings.append(warning("checks_missing", "No validation evidence is recorded; validation is unknown.", "info"))
        return empty
    try:
        value = load_json(path)
    except (OSError, json.JSONDecodeError):
        warnings.append(warning("checks_invalid", "Validation evidence is unreadable; validation is unknown.", "error"))
        return empty
    if not valid_checks(value):
        warnings.append(warning("checks_invalid", "Validation evidence violates checks.schema.json; validation is unknown.", "error"))
        return empty

    recorded = value["recorded_for"]
    recorded_result = "passed" if all(item["exit_code"] == 0 for item in value["commands"]) else "failed"
    same_state = (
        recorded["state_stamp"] == stamp
        and recorded["branch"] == branch
        and recorded["base_commit"] == head
    )
    result = {
        "validity": "valid" if same_state else "stale",
        "scope": value["scope"] if same_state else "unknown",
        "result": recorded_result if same_state else "unknown",
        "recorded_scope": value["scope"],
        "recorded_result": recorded_result,
        "recorded_at": value["recorded_at"],
        "recorded_state_stamp": recorded["state_stamp"],
        "commands": value["commands"],
    }
    if not same_state:
        warnings.append(warning("checks_stale", "The worktree differs from the state that was validated; validation is unknown."))
    elif value["scope"] == "partial":
        warnings.append(warning("checks_partial", "Only a partial validation scope is current.", "info"))
    elif value["scope"] == "unknown":
        warnings.append(warning("checks_scope_unknown", "The validation scope was not classified.", "warning"))
    return result


def handoff_context(
    path: pathlib.Path,
    branch: str | None,
    head: str | None,
    now: dt.datetime,
    warnings: list[dict[str, str]],
) -> dict[str, Any]:
    if not path.exists():
        return {"status": "missing", "source": "none"}
    try:
        value = load_json(path)
    except (OSError, json.JSONDecodeError):
        warnings.append(warning("handoff_invalid", "The local handoff is unreadable and was not adopted.", "error"))
        return {"status": "invalid", "source": "local"}
    if not valid_handoff(value):
        warnings.append(warning("handoff_invalid", "The local handoff violates handoff.schema.json and was not adopted.", "error"))
        return {"status": "invalid", "source": "local"}

    base = {
        "source": "local",
        "source_branch": value["branch"],
        "source_base_commit": value["base_commit"],
    }
    if value["branch"] != branch:
        warnings.append(warning("orphaned_handoff", "The handoff belongs to another branch and was not adopted."))
        return {"status": "orphaned_handoff", **base}
    if parse_time(value["expires_at"]) <= now:
        warnings.append(warning("handoff_expired", "The handoff TTL expired and its task was not adopted."))
        return {"status": "expired", **base}
    if value["base_commit"] != head:
        warnings.append(warning("handoff_stale_base", "The handoff base commit differs from HEAD and was not adopted."))
        return {"status": "stale_base", **base}

    adopted = {
        "status": "adopted",
        **base,
        "task": value["task"],
        "next": value["next"],
        "discarded": value["discarded"],
        "open_questions": value["open_questions"],
        "updated_at": value["updated_at"],
        "expires_at": value["expires_at"],
    }
    if "promotion" in value:
        adopted["promotion"] = value["promotion"]
    return adopted


def markdown_title(path: pathlib.Path, content: str) -> str:
    for line in content.splitlines():
        if line.startswith("# "):
            return line[2:].strip()
    return path.stem.replace("-", " ")


def markdown_summary(content: str) -> str:
    paragraphs: list[str] = []
    current: list[str] = []
    in_fence = False
    for raw in content.splitlines():
        line = raw.strip()
        if line.startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence or line.startswith("#") or line.startswith("|") or line.startswith("-"):
            if current:
                paragraphs.append(" ".join(current))
                current = []
            continue
        if not line:
            if current:
                paragraphs.append(" ".join(current))
                current = []
            if paragraphs:
                break
            continue
        current.append(line.lstrip("> "))
    if current:
        paragraphs.append(" ".join(current))
    summary = paragraphs[0] if paragraphs else ""
    summary = re.sub(r"\[([^]]+)]\([^)]+\)", r"\1", summary)
    return summary[:400]


def local_knowledge(root: pathlib.Path) -> list[dict[str, Any]]:
    paths: list[tuple[pathlib.Path, str]] = []
    contract = root / "docs" / "contract.md"
    if contract.is_file():
        paths.append((contract, "contract"))
    for kind, pattern in (("adr", "docs/adr/*.md"), ("pitfall", "docs/pitfalls/*.md")):
        for path in sorted(root.glob(pattern)):
            if path.is_file():
                paths.append((path, kind))
    items = []
    for path, kind in paths:
        content = path.read_text(encoding="utf-8")
        rel = path.relative_to(root).as_posix()
        items.append(
            {
                "id": rel,
                "kind": kind,
                "title": markdown_title(path, content),
                "summary": markdown_summary(content),
                "freshness": "valid",
                "content": content,
            }
        )
    return items


def adapted_knowledge(path: pathlib.Path, now: dt.datetime) -> list[dict[str, Any]]:
    value = load_json(path)
    raw_items = value.get("items") if isinstance(value, dict) else None
    if not isinstance(raw_items, list):
        fail("--knowledge-file must contain an object with an items array")
    items: list[dict[str, Any]] = []
    for raw in raw_items:
        if not isinstance(raw, dict):
            fail("--knowledge-file items must be objects")
        item = {
            "id": str(raw.get("id", "")),
            "kind": raw.get("kind", "pitfall"),
            "title": str(raw.get("title", "")),
            "summary": str(raw.get("summary", "")),
            "freshness": raw.get("freshness", "unknown"),
        }
        if not item["id"] or not item["title"]:
            fail("--knowledge-file items require non-empty id and title")
        if item["kind"] not in {"contract", "adr", "pitfall"}:
            item["kind"] = "pitfall"
        if item["freshness"] not in FRESHNESS_RANK:
            item["freshness"] = "unknown"
        if raw.get("superseded_by"):
            item["freshness"] = "superseded"
            item["superseded_by"] = str(raw["superseded_by"])
        derived = raw.get("derived_from")
        if isinstance(derived, dict):
            resolver_ok = derived.get("resolver_available") is True
            fingerprint_ok = (
                isinstance(derived.get("fingerprint"), str)
                and derived.get("fingerprint") == derived.get("source_fingerprint")
            )
            if not resolver_ok or not fingerprint_ok:
                item["freshness"] = "unknown"
        review_after = raw.get("review_after")
        if isinstance(review_after, str) and item["freshness"] == "valid":
            try:
                if parse_time(review_after) <= now:
                    item["freshness"] = "needs_review"
            except (ValueError, SystemExit):
                item["freshness"] = "unknown"
        if isinstance(raw.get("content"), str):
            item["content"] = raw["content"]
        items.append(item)
    return items


def knowledge_context(
    items: list[dict[str, Any]],
    mode: str,
    detail_id: str,
    max_items: int,
    warnings: list[dict[str, str]],
) -> dict[str, Any]:
    items.sort(key=lambda item: (FRESHNESS_RANK[item["freshness"]], item["id"]))
    for item in items:
        if item["freshness"] != "valid":
            warnings.append(
                warning(
                    f"knowledge_{item['freshness']}_{hashlib.sha256(item['id'].encode()).hexdigest()[:8]}",
                    f"Knowledge {item['id']} is {item['freshness']}.",
                    "info" if item["freshness"] == "superseded" else "warning",
                )
            )

    selected: list[dict[str, Any]]
    if mode == "detail":
        selected = [item for item in items if item["id"] == detail_id]
        if not selected:
            warnings.append(warning("knowledge_detail_not_found", f"Knowledge ID {detail_id} was not found."))
    elif mode == "full":
        selected = items
    else:
        selected = items[:max_items]

    output = []
    for source in selected:
        item = {key: source[key] for key in ("id", "kind", "title", "summary", "freshness")}
        if "superseded_by" in source:
            item["superseded_by"] = source["superseded_by"]
        if mode in {"detail", "full"} and "content" in source:
            item["content"] = source["content"]
        output.append(item)
    omitted = max(0, len(items) - len(selected)) if mode == "summary" else 0
    return {"items": output, "omitted_count": omitted}


def main() -> None:
    requested_root = pathlib.Path(sys.argv[1]).expanduser().resolve()
    mode = sys.argv[2]
    detail_id = sys.argv[3]
    checks_arg = sys.argv[4]
    handoff_arg = sys.argv[5]
    knowledge_arg = sys.argv[6]
    max_knowledge = int(sys.argv[7])
    fixed_now = sys.argv[8]

    root_text = git_text(requested_root, "rev-parse", "--show-toplevel")
    root = pathlib.Path(root_text).resolve()
    branch = git_text(root, "symbolic-ref", "--quiet", "--short", "HEAD", check=False) or None
    head = git_text(root, "rev-parse", "--verify", "HEAD", check=False) or None
    if head is not None and HEX_RE.fullmatch(head) is None:
        fail("Git returned an invalid HEAD object ID")
    now = parse_time(fixed_now) if fixed_now else dt.datetime.now(dt.timezone.utc)
    stamp = worktree_stamp(root, head)
    changes = status_counts(root)
    warnings: list[dict[str, str]] = []

    checks_file = pathlib.Path(checks_arg).expanduser() if checks_arg else root / ".kb-dev" / "checks.json"
    handoff_file = pathlib.Path(handoff_arg).expanduser() if handoff_arg else root / ".kb-dev" / "handoff.json"
    checks = checks_context(checks_file, branch, head, stamp, warnings)
    handoff = handoff_context(handoff_file, branch, head, now, warnings)

    if knowledge_arg:
        knowledge_items = adapted_knowledge(pathlib.Path(knowledge_arg).expanduser(), now)
    else:
        knowledge_items = local_knowledge(root)
    knowledge = knowledge_context(knowledge_items, mode, detail_id, max_knowledge, warnings)

    result: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "producer": {"kind": "script", "name": "dev-state.sh", "version": SCHEMA_VERSION},
        "generated_at": format_time(now),
        "mode": mode,
        "repository": {
            "root": str(root),
            "branch": branch,
            "head": head,
            "dirty": any(changes.values()),
            "state_stamp": stamp,
            "changes": changes,
        },
        "checks": checks,
        "handoff": handoff,
        "freshness_warnings": warnings,
        "knowledge": knowledge,
    }
    if mode == "detail":
        result["detail_id"] = detail_id
    json.dump(result, sys.stdout, ensure_ascii=False, indent=2, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
PY
