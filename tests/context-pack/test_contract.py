#!/usr/bin/env python3
"""Dependency-free contract tests for the development Context Pack P0."""

from __future__ import annotations

import datetime as dt
import json
import pathlib
import re
import subprocess
import tempfile
import unittest
from typing import Any
from urllib.parse import urlparse


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "dev-state.sh"
SCHEMAS = ROOT / "schemas"
EXAMPLES = SCHEMAS / "examples"
FIXTURES = pathlib.Path(__file__).resolve().parent / "fixtures"
NOW = "2026-08-12T00:00:00Z"
FUTURE = "2026-08-13T00:00:00Z"


def read_json(path: pathlib.Path) -> Any:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def json_type_matches(value: Any, expected: str) -> bool:
    if expected == "null":
        return value is None
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "string":
        return isinstance(value, str)
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    return True


def resolve_ref(root_schema: dict[str, Any], ref: str) -> dict[str, Any]:
    if not ref.startswith("#/"):
        raise AssertionError(f"contract validator only supports local refs: {ref}")
    value: Any = root_schema
    for component in ref[2:].split("/"):
        value = value[component.replace("~1", "/").replace("~0", "~")]
    return value


def schema_errors(
    value: Any,
    schema: dict[str, Any],
    *,
    root_schema: dict[str, Any] | None = None,
    path: str = "$",
) -> list[str]:
    """Validate the JSON Schema keywords used by this repository, without PyPI."""
    root_schema = root_schema or schema
    if "$ref" in schema:
        return schema_errors(value, resolve_ref(root_schema, schema["$ref"]), root_schema=root_schema, path=path)

    errors: list[str] = []
    expected_type = schema.get("type")
    if expected_type is not None:
        expected_types = [expected_type] if isinstance(expected_type, str) else expected_type
        if not any(json_type_matches(value, item) for item in expected_types):
            return [f"{path}: expected {expected_types}, got {type(value).__name__}"]

    if "const" in schema and value != schema["const"]:
        errors.append(f"{path}: expected const {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{path}: {value!r} is not in {schema['enum']!r}")

    if isinstance(value, dict):
        required = schema.get("required", [])
        for key in required:
            if key not in value:
                errors.append(f"{path}: missing required property {key}")
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            for key in value:
                if key not in properties:
                    errors.append(f"{path}: unexpected property {key}")
        for key, child_schema in properties.items():
            if key in value:
                errors.extend(
                    schema_errors(
                        value[key],
                        child_schema,
                        root_schema=root_schema,
                        path=f"{path}.{key}",
                    )
                )

    if isinstance(value, list):
        if len(value) < schema.get("minItems", 0):
            errors.append(f"{path}: too few items")
        if "maxItems" in schema and len(value) > schema["maxItems"]:
            errors.append(f"{path}: too many items")
        if isinstance(schema.get("items"), dict):
            for index, item in enumerate(value):
                errors.extend(
                    schema_errors(
                        item,
                        schema["items"],
                        root_schema=root_schema,
                        path=f"{path}[{index}]",
                    )
                )

    if isinstance(value, str):
        if len(value) < schema.get("minLength", 0):
            errors.append(f"{path}: string is too short")
        pattern = schema.get("pattern")
        if pattern and re.search(pattern, value) is None:
            errors.append(f"{path}: does not match {pattern}")
        if schema.get("format") == "date-time":
            try:
                parsed = value[:-1] + "+00:00" if value.endswith("Z") else value
                result = dt.datetime.fromisoformat(parsed)
                if result.tzinfo is None:
                    raise ValueError("timezone missing")
            except ValueError:
                errors.append(f"{path}: invalid date-time")
        if schema.get("format") == "uri" and not urlparse(value).scheme:
            errors.append(f"{path}: invalid URI")

    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if "minimum" in schema and value < schema["minimum"]:
            errors.append(f"{path}: below minimum")

    for child in schema.get("allOf", []):
        errors.extend(schema_errors(value, child, root_schema=root_schema, path=path))
    condition = schema.get("if")
    if isinstance(condition, dict):
        if not schema_errors(value, condition, root_schema=root_schema, path=path):
            then = schema.get("then")
            if isinstance(then, dict):
                errors.extend(schema_errors(value, then, root_schema=root_schema, path=path))
    return errors


def assert_schema(test: unittest.TestCase, value: Any, schema_name: str) -> None:
    schema = read_json(SCHEMAS / schema_name)
    errors = schema_errors(value, schema)
    test.assertEqual(errors, [], "\n".join(errors))


class ContextPackContractTests(unittest.TestCase):
    def make_repo(self) -> pathlib.Path:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = pathlib.Path(temp.name)
        subprocess.run(["git", "init", "-q", "-b", "main", str(root)], check=True)
        subprocess.run(["git", "-C", str(root), "config", "user.name", "Contract Test"], check=True)
        subprocess.run(["git", "-C", str(root), "config", "user.email", "contract@example.invalid"], check=True)
        (root / ".gitignore").write_text(".kb-dev/checks.json\n.kb-dev/handoff.json\n", encoding="utf-8")
        (root / "tracked.txt").write_text("base\n", encoding="utf-8")
        (root / "docs" / "adr").mkdir(parents=True)
        (root / "docs" / "contract.md").write_text("# Contract\n\nCore rules.\n", encoding="utf-8")
        (root / "docs" / "adr" / "0001.md").write_text("# First ADR\n\nKeep facts in Git.\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(root), "add", "."], check=True)
        subprocess.run(["git", "-C", str(root), "commit", "-qm", "fixture"], check=True)
        return root

    def run_context(self, root: pathlib.Path, *args: str) -> dict[str, Any]:
        output = subprocess.run(
            ["bash", str(SCRIPT), "--repo", str(root), "--now", NOW, *args],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        return json.loads(output.stdout)

    def write_checks(self, root: pathlib.Path, *, scope: str = "full", exit_code: int = 0) -> None:
        initial = self.run_context(root)
        target = root / ".kb-dev" / "checks.json"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(
            json.dumps(
                {
                    "schema_version": "1.0.0",
                    "recorded_for": {
                        "branch": initial["repository"]["branch"],
                        "base_commit": initial["repository"]["head"],
                        "state_stamp": initial["repository"]["state_stamp"],
                    },
                    "recorded_at": NOW,
                    "scope": scope,
                    "commands": [{"command": "contract-check", "exit_code": exit_code}],
                }
            ),
            encoding="utf-8",
        )

    def write_handoff(self, root: pathlib.Path, *, branch: str = "main") -> None:
        initial = self.run_context(root)
        target = root / ".kb-dev" / "handoff.json"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(
            json.dumps(
                {
                    "schema_version": "1.0.0",
                    "task": "Continue Context Pack P0",
                    "next": ["Run contract tests"],
                    "discarded": ["Do not copy Git facts to KB"],
                    "open_questions": ["MCP command name"],
                    "branch": branch,
                    "base_commit": initial["repository"]["head"],
                    "created_at": NOW,
                    "updated_at": NOW,
                    "expires_at": FUTURE,
                }
            ),
            encoding="utf-8",
        )

    def test_schema_and_example_documents_are_valid_json(self) -> None:
        for path in sorted(SCHEMAS.glob("*.schema.json")):
            schema = read_json(path)
            self.assertEqual(schema["$schema"], "https://json-schema.org/draft/2020-12/schema")
        assert_schema(self, read_json(EXAMPLES / "checks.example.json"), "checks.schema.json")
        assert_schema(self, read_json(EXAMPLES / "handoff.example.json"), "handoff.schema.json")
        assert_schema(
            self,
            read_json(EXAMPLES / "context-pack.summary.example.json"),
            "context-pack.schema.json",
        )

    def test_t01_fresh_agents_reconstruct_the_same_context(self) -> None:
        root = self.make_repo()
        self.write_checks(root)
        self.write_handoff(root)
        first = self.run_context(root)
        second = self.run_context(root)
        self.assertEqual(first, second)
        self.assertEqual(first["handoff"]["status"], "adopted")
        self.assertEqual(first["handoff"]["task"], "Continue Context Pack P0")
        self.assertEqual(first["checks"]["result"], "passed")
        self.assertEqual(first["checks"]["scope"], "full")
        assert_schema(self, first, "context-pack.schema.json")

    def test_t02_one_byte_tracked_edit_makes_checks_stale(self) -> None:
        root = self.make_repo()
        self.write_checks(root)
        before = self.run_context(root)
        (root / "tracked.txt").write_text("base!\n", encoding="utf-8")
        after = self.run_context(root)
        self.assertNotEqual(before["repository"]["state_stamp"], after["repository"]["state_stamp"])
        self.assertEqual(after["checks"]["validity"], "stale")
        self.assertEqual(after["checks"]["result"], "unknown")

    def test_t03_untracked_test_file_makes_checks_stale(self) -> None:
        root = self.make_repo()
        self.write_checks(root)
        (root / "new-test.txt").write_text("new test\n", encoding="utf-8")
        context = self.run_context(root)
        self.assertEqual(context["checks"]["validity"], "stale")
        self.assertEqual(context["repository"]["changes"]["untracked"], 1)

    def test_t04_same_machine_handoff_needs_no_copy(self) -> None:
        root = self.make_repo()
        self.write_handoff(root)
        claude_view = self.run_context(root)
        codex_view = self.run_context(root)
        self.assertEqual(claude_view["handoff"], codex_view["handoff"])
        self.assertEqual(codex_view["handoff"]["next"], ["Run contract tests"])

    def test_t05_promoted_issue_fixture_supports_cross_device_handoff(self) -> None:
        fixture = read_json(FIXTURES / "mcp-summary.json")
        assert_schema(self, fixture, "context-pack.schema.json")
        self.assertEqual(fixture["handoff"]["source"], "promoted_issue")
        self.assertEqual(fixture["handoff"]["promotion"]["provider"], "github")

    def test_t06_superseded_knowledge_is_downranked(self) -> None:
        root = self.make_repo()
        context = self.run_context(root, "--knowledge-file", str(FIXTURES / "knowledge.json"))
        ids = [item["id"] for item in context["knowledge"]["items"]]
        self.assertLess(ids.index("adr/current"), ids.index("adr/old"))
        old = next(item for item in context["knowledge"]["items"] if item["id"] == "adr/old")
        self.assertEqual(old["freshness"], "superseded")
        self.assertEqual(old["superseded_by"], "adr/current")

    def test_t07_and_t10_unresolved_derived_knowledge_is_unknown(self) -> None:
        root = self.make_repo()
        context = self.run_context(root, "--knowledge-file", str(FIXTURES / "knowledge.json"))
        item = next(
            item for item in context["knowledge"]["items"] if item["id"] == "pitfall/unresolved"
        )
        self.assertEqual(item["freshness"], "unknown")
        warning_codes = {item["code"] for item in context["freshness_warnings"]}
        self.assertTrue(any(code.startswith("knowledge_unknown_") for code in warning_codes))

    def test_t08_unknown_tag_guard_remains_in_the_core_contract(self) -> None:
        contract = (ROOT / "docs" / "contract.md").read_text(encoding="utf-8")
        implementation = (ROOT / "crates" / "kb-core" / "src" / "tags.rs").read_text(encoding="utf-8")
        self.assertIn("語彙外の新語は拒否", contract)
        self.assertIn("fn unknown_tag_is_rejected_with_suggestions", implementation)

    def test_t11_context_inspection_has_no_repository_side_effects(self) -> None:
        root = self.make_repo()
        before_status = subprocess.run(
            ["git", "-C", str(root), "status", "--porcelain=v1"],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        ).stdout
        before_head = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        ).stdout
        self.run_context(root)
        after_status = subprocess.run(
            ["git", "-C", str(root), "status", "--porcelain=v1"],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        ).stdout
        after_head = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        ).stdout
        self.assertEqual(before_status, after_status)
        self.assertEqual(before_head, after_head)
        self.assertFalse((root / ".kb-dev").exists())

    def test_t12_script_and_future_mcp_share_one_schema(self) -> None:
        root = self.make_repo()
        script_value = self.run_context(root)
        mcp_value = read_json(FIXTURES / "mcp-summary.json")
        assert_schema(self, script_value, "context-pack.schema.json")
        assert_schema(self, mcp_value, "context-pack.schema.json")
        self.assertEqual(set(script_value), set(mcp_value))
        self.assertEqual(script_value["schema_version"], mcp_value["schema_version"])

    def test_t13_partial_checks_are_never_reported_as_full(self) -> None:
        root = self.make_repo()
        self.write_checks(root, scope="partial")
        context = self.run_context(root)
        self.assertEqual(context["checks"]["scope"], "partial")
        self.assertNotEqual(context["checks"]["scope"], "full")
        self.assertIn("checks_partial", {item["code"] for item in context["freshness_warnings"]})

    def test_t14_wrong_branch_handoff_is_not_adopted(self) -> None:
        root = self.make_repo()
        self.write_handoff(root, branch="other-branch")
        context = self.run_context(root)
        self.assertEqual(context["handoff"]["status"], "orphaned_handoff")
        self.assertNotIn("task", context["handoff"])
        self.assertNotIn("next", context["handoff"])

    def test_detail_and_full_modes_are_explicit(self) -> None:
        root = self.make_repo()
        detail = self.run_context(root, "--mode", "detail", "--id", "docs/contract.md")
        self.assertEqual(detail["detail_id"], "docs/contract.md")
        self.assertEqual(len(detail["knowledge"]["items"]), 1)
        self.assertIn("content", detail["knowledge"]["items"][0])
        full = self.run_context(root, "--mode", "full")
        self.assertEqual(len(full["knowledge"]["items"]), 2)
        self.assertTrue(all("content" in item for item in full["knowledge"]["items"]))

    def test_acceptance_catalog_keeps_all_agreed_cases_visible(self) -> None:
        catalog = read_json(FIXTURES / "acceptance-cases.json")
        self.assertEqual([item["id"] for item in catalog["cases"]], [f"T{i}" for i in range(1, 15)])
        owner = {item["id"]: item["owner"] for item in catalog["cases"]}
        self.assertEqual(owner["T9"], "future-note-revision")


if __name__ == "__main__":
    unittest.main(verbosity=2)
