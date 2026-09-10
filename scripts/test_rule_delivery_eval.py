import importlib.util
import hashlib
import json
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path


SCRIPT = Path(__file__).with_name("rule_delivery_eval.py")
SPEC = importlib.util.spec_from_file_location("rule_delivery_eval", SCRIPT)
runner = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(runner)


class RuleDeliveryRunnerTest(unittest.TestCase):
    def initialize_entry(self, instructions):
        return {
            "request": {"id": 1, "method": "initialize", "params": {
                "clientInfo": {"name": "synthetic-cli", "version": "2.3.4"}
            }},
            "response": {"id": 1, "result": {
                "instructions": instructions,
                "capabilities": {"experimental": {"kbApp": {"rule_identity": {
                    "schema": 1, "contract_sha256": "a" * 64,
                    "instructions_sha256": hashlib.sha256(instructions.encode()).hexdigest(),
                    "client_surface": "rule_delivery_evaluation", "tool_surface": "all",
                    "server_version": "synthetic-server",
                }}}},
            }},
        }

    def test_initialize_evidence_requires_an_observed_matching_response(self):
        """2026-09-08: 計画にRuleがあっても実initialize応答が無ければ観測にしない。"""
        prepared = {"prompt_context": "実際の規則", "delivered_rule_ids": ["always.example"]}
        entry = self.initialize_entry("評価案内\n\n[Delivered Rules]\n実際の規則")
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "mcp.jsonl"
            self.assertEqual(runner.parse_initialize_evidence(path), (None, None))
            self.assertEqual(runner.observed_rule_ids(None, prepared), [])
            path.write_text(json.dumps(entry) + "\n", encoding="utf-8")
            observed, version = runner.parse_initialize_evidence(path)
            self.assertEqual(runner.observed_rule_ids(observed, prepared), ["always.example"])
            self.assertEqual(observed["source"], "mcp_initialize_server_response")
            self.assertEqual(observed["rule_identity"], entry["response"]["result"]["capabilities"]["experimental"]["kbApp"]["rule_identity"])
            self.assertEqual(version, {"value": "2.3.4", "source": "mcp_initialize_request.clientInfo.version"})
            changed = self.initialize_entry("評価案内\n\n[Delivered Rules]\n別の規則")
            path.write_text(json.dumps(changed) + "\n", encoding="utf-8")
            observed, _ = runner.parse_initialize_evidence(path)
            self.assertEqual(runner.observed_rule_ids(observed, prepared), [])
            path.write_text("\n".join(json.dumps(item) for item in (entry, changed)), encoding="utf-8")
            self.assertIsNone(runner.parse_initialize_evidence(path)[0])
            entry["response"]["id"] = 2
            path.write_text(json.dumps(entry), encoding="utf-8")
            self.assertIsNone(runner.parse_initialize_evidence(path)[0])

    def test_empty_delivery_is_observed_only_with_the_explicit_empty_context(self):
        prepared = {"prompt_context": "", "delivered_rule_ids": []}
        entry = self.initialize_entry("評価案内\n\n[Delivered Rules]\nなし")
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "mcp.jsonl"
            path.write_text(json.dumps(entry), encoding="utf-8")
            observed, _ = runner.parse_initialize_evidence(path)
        self.assertIsNotNone(observed)
        self.assertEqual(runner.observed_rule_ids(observed, prepared), [])

    def test_runner_records_actual_evidence_and_never_labels_cli_version_as_model(self):
        """2026-09-08: 実AIを起動せず、runner全体で計画値の誤転記を再現する。"""
        prepared = {"prompt_context": "合成規則", "delivered_rule_ids": ["always.example"]}
        for client, observe in (("claude", True), ("codex", True), ("claude", False)):
            with self.subTest(client=client, observe=observe), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                suite = root / "suite.json"
                suite.write_text('{"synthetic":true}\n', encoding="utf-8")
                raw = root / "raw"
                def fake_run(command, **kwargs):
                    if observe:
                        entry = self.initialize_entry("評価案内\n\n[Delivered Rules]\n合成規則")
                        (raw / "mcp.jsonl").write_text(json.dumps(entry), encoding="utf-8")
                    stdout = json.dumps({"type": "system", "subtype": "init", "model": "synthetic-model", "claude_code_version": "9.9.9"}) if client == "claude" else ""
                    return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr="")
                with patch.object(runner, "run_command", side_effect=fake_run):
                    trace = runner.run_one(
                        client=client, suite_path=suite, eval_mcp_bin=root / "synthetic-bin",
                        mode="always_topic", case={"id": "A1", "prompt": "合成質問"},
                        prepared=prepared, run_number=1, fixture=root, raw_case_dir=raw,
                        model_override=None, timeout=1,
                    )
                self.assertIsNone(trace["model_version"])
                self.assertEqual(trace["delivered_rule_ids"], ["always.example"] if observe else [])
                self.assertEqual(trace["evidence"]["suite_sha256"], hashlib.sha256(suite.read_bytes()).hexdigest())
                source = "claude_system_init.claude_code_version" if client == "claude" else "mcp_initialize_request.clientInfo.version"
                self.assertEqual(trace["evidence"]["cli_version"]["source"], source)
                self.assertEqual(trace["evidence"]["initialize_response"] is not None, observe)

    def test_unspecified_claude_version_is_not_guessed_from_generic_version(self):
        parsed = runner.parse_claude_output(json.dumps({
            "type": "system", "subtype": "init", "version": "protocol-or-unknown"
        }), None)
        self.assertIsNone(parsed[1])

    def test_matrix_freezes_suite_before_planning_and_keeps_its_bytes_for_each_run(self):
        """2026-09-08: 元suiteの途中変更で旧plan/fixtureと新hash/MCP入力が混ざらない。"""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source.json"
            original = {"cases": [{"id": "A1", "prompt": "元の合成質問"}], "fixture_body": "元の本文"}
            original_bytes = (json.dumps(original, ensure_ascii=False, indent=2) + "\n").encode()
            source.write_bytes(original_bytes)
            binary = root / "synthetic-bin"
            binary.touch()
            output = root / "traces.json"
            raw = root / "raw"
            observed_paths = []

            def observe(path):
                self.assertNotEqual(path, source)
                self.assertEqual(path.read_bytes(), original_bytes)
                observed_paths.append(path)

            def fake_plan(_binary, suite, _mode):
                observe(suite)
                # 全planを作った後に元ファイルが変わる、長時間matrixでの事故を再現する。
                source.write_text('{"cases":[],"fixture_body":"変更後"}', encoding="utf-8")
                return {"cases": [{"case_id": "A1", "prompt_context": "", "delivered_rule_ids": []}]}

            def fake_fixture(_binary, suite, destination):
                observe(suite)
                destination.mkdir()
                (destination / "fixture-body.txt").write_text(json.loads(suite.read_text())["fixture_body"], encoding="utf-8")

            real_mcp_arguments = runner.mcp_arguments

            def recorded_arguments(eval_binary, fixture, suite, *args):
                observe(suite)
                self.assertEqual((fixture / "fixture-body.txt").read_text(encoding="utf-8"), "元の本文")
                return real_mcp_arguments(eval_binary, fixture, suite, *args)

            def fake_client(command, **kwargs):
                self.assertIn(runner.prompt_for(original["cases"][0]), command)
                return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

            with patch.object(runner, "load_plan", side_effect=fake_plan), \
                 patch.object(runner, "create_fixture", side_effect=fake_fixture), \
                 patch.object(runner, "mcp_arguments", side_effect=recorded_arguments), \
                 patch.object(runner, "run_command", side_effect=fake_client):
                self.assertEqual(runner.main([
                    "--suite", str(source), "--kb-bin", str(binary), "--eval-mcp-bin", str(binary),
                    "--client", "claude", "--modes", "always_topic", "--cases", "A1", "--runs", "2",
                    "--output", str(output), "--raw-dir", str(raw),
                ]), 0)
            self.assertEqual(len(observed_paths), 4)
            self.assertEqual(len(set(observed_paths)), 1)
            self.assertNotEqual(source.read_bytes(), original_bytes)
            self.assertEqual((raw / "suite.snapshot.json").read_bytes(), original_bytes)
            traces = json.loads(output.read_text())["traces"]
            self.assertEqual(len(traces), 2)
            for trace in traces:
                self.assertEqual(trace["evidence"]["suite_sha256"], hashlib.sha256(original_bytes).hexdigest())

    def test_prompt_marks_the_run_to_skip_managed_real_kb_retrieval(self):
        prompt = runner.prompt_for({"id": "A1", "prompt": "fixture question"})
        self.assertTrue(prompt.startswith("fixture question"))
        self.assertTrue(prompt.endswith(runner.AUTO_RETRIEVAL_SKIP_MARKER))

    def test_claude_stream_extracts_notice_result_and_usage(self):
        events = [
            {
                "type": "system",
                "subtype": "init",
                "model": "claude-test",
                "claude_code_version": "9.9.9",
            },
            {
                "type": "assistant",
                "message": {
                    "content": [
                        {"type": "text", "text": "更新前に一言伝えます"},
                        {"type": "tool_use", "name": "mcp__kb_rule_eval__update"},
                    ]
                },
            },
            {
                "type": "result",
                "subtype": "success",
                "result": "更新しました",
                "duration_ms": 123,
                "usage": {"input_tokens": 40, "cache_read_input_tokens": 2},
            },
        ]
        parsed = runner.parse_claude_output(
            "\n".join(json.dumps(event) for event in events), None
        )
        model, version, response, tokens, latency, pre_tool = parsed
        self.assertEqual(model, "claude-test")
        self.assertEqual(version, "9.9.9")
        self.assertEqual(response, "更新しました")
        self.assertEqual(tokens, 42)
        self.assertEqual(latency, 123)
        self.assertEqual(pre_tool, [("update", "更新前に一言伝えます")])

    def test_claude_command_isolates_settings_and_tools(self):
        with tempfile.TemporaryDirectory() as temporary:
            command = runner.claude_command(
                ["/tmp/kb-rule-eval-mcp", "--case", "A1"],
                "prompt",
                None,
                Path(temporary) / "mcp.json",
            )
        setting_index = command.index("--setting-sources")
        self.assertEqual(command[setting_index + 1], "")
        self.assertIn("--strict-mcp-config", command)
        tools_index = command.index("--tools")
        self.assertIn("mcp__kb_rule_eval__search", command[tools_index + 1])

    def test_codex_stream_marks_builtin_tools(self):
        events = [
            {
                "type": "item.completed",
                "item": {
                    "type": "command_execution",
                    "command": "rg secret",
                    "status": "completed",
                    "aggregated_output": "none",
                },
            },
            {
                "type": "item.completed",
                "item": {"type": "agent_message", "text": "回答"},
            },
            {"type": "turn.completed", "usage": {"input_tokens": 88}},
        ]
        parsed = runner.parse_codex_output(
            "\n".join(json.dumps(event) for event in events), "gpt-test"
        )
        self.assertEqual(parsed[0], "gpt-test")
        self.assertEqual(parsed[2], "回答")
        self.assertEqual(parsed[3], 88)
        self.assertEqual(parsed[6][0]["name"], "builtin.command_execution")

    def test_codex_command_uses_noninteractive_approval_config(self):
        command = runner.codex_command(
            ["/tmp/kb-rule-eval-mcp", "--case", "A1"],
            "prompt",
            None,
            Path("/tmp/fixture"),
        )
        self.assertNotIn("-a", command)
        self.assertIn('approval_policy="never"', command)
        self.assertIn("--ignore-user-config", command)
        self.assertIn("--ignore-rules", command)

    def test_tag_vocabulary_is_available_to_both_evaluation_clients(self):
        """2026-09-08: fixtureが要求する語彙確認をclientのallowlistで拒否しない。"""
        suite = json.loads(
            SCRIPT.parent.parent.joinpath(
                "schemas/examples/rule-delivery-eval.example.json"
            ).read_text(encoding="utf-8")
        )
        for case_id in ("T1", "T2"):
            case = next(case for case in suite["cases"] if case["id"] == case_id)
            self.assertIn("tag_vocabulary", case["expected"]["required_tools"])
        with tempfile.TemporaryDirectory() as temporary:
            claude = runner.claude_command(
                ["/tmp/kb-rule-eval-mcp", "--case", "T1"],
                "prompt",
                None,
                Path(temporary) / "mcp.json",
            )
        vocabulary_tools = (
            "tag_vocabulary", "set_tag_vocabulary_source", "plan_tag_vocabulary_change",
            "apply_tag_vocabulary_change", "list_tag_vocabulary_changes", "get_tag_vocabulary_change",
            "plan_tag_vocabulary_rollback", "rollback_tag_vocabulary_change", "get_tag_vocabulary_stats",
        )
        for flag in ("--tools", "--allowedTools"):
            allowed = claude[claude.index(flag) + 1].split(",")
            for name in vocabulary_tools:
                self.assertIn(f"mcp__kb_rule_eval__{name}", allowed)
        codex = runner.codex_command(
            ["/tmp/kb-rule-eval-mcp", "--case", "T1"],
            "prompt",
            None,
            Path("/tmp/fixture"),
        )
        prefix = "mcp_servers.kb_rule_eval.enabled_tools="
        configured = next(value for value in codex if value.startswith(prefix))
        allowed = json.loads(configured[len(prefix):])
        for name in vocabulary_tools:
            self.assertIn(name, allowed)

    def test_tag_vocabulary_call_is_preserved_for_forbidden_case_scoring(self):
        """2026-09-08: 新readの呼出しを捨てず、skip_kb_toolsを判定するcore採点器へ渡す。"""
        entry = {
            "request": {
                "method": "tools/call",
                "params": {"name": "tag_vocabulary", "arguments": {}},
            },
            "response": {
                "result": {
                    "content": [{"type": "text", "text": "語彙"}],
                    "structuredContent": {"entries": {"review": "評価"}},
                }
            },
        }
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "mcp.jsonl"
            setter = json.loads(json.dumps(entry))
            setter["request"]["params"] = {
                "name": "set_tag_vocabulary_source",
                "arguments": {"workspace_id": "fixture", "note_uid": "source", "expected_revision": None, "reason": "評価"},
            }
            setter["response"]["result"]["structuredContent"] = {"stored": True, "export_pending": False}
            path.write_text("\n".join(json.dumps(item) for item in (entry, setter)) + "\n", encoding="utf-8")
            calls, _, _ = runner.parse_mcp_trace(path)
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[1]["name"], "set_tag_vocabulary_source")
        self.assertEqual(calls[1]["arguments"], setter["request"]["params"]["arguments"])
        self.assertTrue(calls[1]["result"]["structuredContent"]["stored"])
        self.assertEqual(calls[0]["name"], "tag_vocabulary")
        self.assertEqual(calls[0]["arguments"], {})
        self.assertFalse(calls[0]["is_error"])
        self.assertEqual(
            calls[0]["result"]["structuredContent"]["entries"], {"review": "評価"}
        )

    def test_bulk_tag_change_calls_and_saved_warning_survive_trace_parsing(self):
        """2026-09-08: 一括変更を通常updateへ変換せず、保存済み警告と履歴をcore採点へ渡す。"""
        names = ("plan_tag_vocabulary_change", "apply_tag_vocabulary_change", "list_tag_vocabulary_changes", "get_tag_vocabulary_change",
                 "plan_tag_vocabulary_rollback", "rollback_tag_vocabulary_change", "get_tag_vocabulary_stats")
        entries = []
        for name in names:
            structured = {"stored": True, "pending_exports": 2, "conversation_events": [
                {"type": "tag_vocabulary_rolled_back" if name == "rollback_tag_vocabulary_change" else "tag_vocabulary_changed", "required": True},
                {"type": "degradation", "required": True, "code": "markdown_export"},
            ]}
            entries.append({"request": {"method": "tools/call", "params": {"name": name, "arguments": {"fixture": name}}},
                            "response": {"result": {"structuredContent": structured}}})
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "mcp.jsonl"
            path.write_text("\n".join(json.dumps(entry) for entry in entries) + "\n", encoding="utf-8")
            calls, _, _ = runner.parse_mcp_trace(path)
        self.assertEqual([call["name"] for call in calls], list(names))
        self.assertFalse(any(call["is_error"] for call in calls))
        self.assertEqual(calls[1]["result"]["structuredContent"]["pending_exports"], 2)
        self.assertEqual(calls[1]["result"]["structuredContent"]["conversation_events"][1]["code"], "markdown_export")
        self.assertEqual(calls[5]["result"]["structuredContent"]["conversation_events"][0]["type"], "tag_vocabulary_rolled_back")

    def test_mcp_trace_is_authoritative_for_results_and_event_rules(self):
        entry = {
            "request": {
                "method": "tools/call",
                "params": {"name": "get", "arguments": {"note": "notes/example"}},
            },
            "response": {
                "result": {
                    "content": [{"type": "text", "text": "本文"}],
                    "structuredContent": {
                        "conversation_link": "/tmp/fixture/notes/example.md",
                        "event_rules": [
                            {"rule_id": "event.note-link", "instruction": "リンクを返す"}
                        ],
                    },
                }
            },
        }
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "mcp.jsonl"
            path.write_text(json.dumps(entry) + "\n", encoding="utf-8")
            calls, event_rules, degradations = runner.parse_mcp_trace(path)
        self.assertEqual(calls[0]["name"], "get")
        self.assertEqual(event_rules, ["event.note-link"])
        self.assertEqual(degradations, [])
        self.assertIn("conversation_link", calls[0]["result"]["structuredContent"])

    def test_client_errors_are_detected_for_fail_fast(self):
        self.assertEqual(
            runner.response_client_error(
                "Not logged in · Please run /login\nClient error: exit 1: no stderr"
            ),
            "Client error: exit 1: no stderr",
        )
        self.assertIsNone(runner.response_client_error("通常のモデル回答"))


if __name__ == "__main__":
    unittest.main()
