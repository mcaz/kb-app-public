import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("rule_delivery_eval.py")
SPEC = importlib.util.spec_from_file_location("rule_delivery_eval", SCRIPT)
runner = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(runner)


class RuleDeliveryRunnerTest(unittest.TestCase):
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
