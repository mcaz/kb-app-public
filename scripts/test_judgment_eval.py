"""採点器の欠損検出と、期待値を隠した対照比較を検査する。"""

import copy
import json
import tempfile
import unittest
from pathlib import Path

import judgment_eval as runner


class JudgmentEvaluationTests(unittest.TestCase):
    def setUp(self):
        self.suite = runner.load_json(runner.DEFAULT_SUITE)
        # Python側では順位づけを再実装せず、export契約だけを模した入力を使う。
        self.contexts = {
            "schema_version": runner.SCHEMA_VERSION,
            "generator": "test_fixture_not_production",
            "suite_digest": runner.digest(self.suite),
            "cases": [
                {
                    "id": case["id"],
                    "notes": [{"id": note, "document": "合成ノート"} for note in case["note_ids"]],
                    "judgment_context": {"entries": [], "context_scope": case["scope"]},
                }
                for case in self.suite["cases"]
            ],
        }
        self.plan, self.manifest = runner.prepare(self.suite, self.contexts, 146)

    def responses(self):
        return {
            "schema_version": runner.SCHEMA_VERSION,
            "prompts_digest": self.manifest["prompts_digest"],
            "responses": [
                {"run_id": run["run_id"], "action": run["expected_action"]}
                for run in self.manifest["runs"]
            ],
        }

    def test_prompt_pairs_only_differ_by_production_context(self):
        prompts = {item["run_id"]: item for item in self.plan["prompts"]}
        for case in self.suite["cases"]:
            pair = [row for row in self.manifest["runs"] if row["case_id"] == case["id"]]
            original, structured = [prompts[row["run_id"]]["messages"] for row in pair]
            self.assertEqual(original[0], structured[0])
            original_payload = json.loads(original[1]["content"])
            structured_payload = json.loads(structured[1]["content"])
            structured_payload["retrieved_data"].pop("judgment_context")
            self.assertEqual(original_payload, structured_payload)

    def test_prompts_do_not_include_grading_labels(self):
        payload = json.dumps(self.plan)
        for field in ("expected_action", "category", "case_id", "notes_with_judgment"):
            self.assertNotIn(field, payload)
        self.assertEqual(len(self.plan["prompts"]), 20)
        self.assertEqual(len({row["run_id"] for row in self.plan["prompts"]}), 20)

    def test_seed_is_reproducible_and_changes_blinded_order(self):
        self.assertEqual(runner.prepare(self.suite, self.contexts, 146)[0], self.plan)
        self.assertNotEqual(runner.prepare(self.suite, self.contexts, 147)[0], self.plan)

    def test_stale_or_missing_context_is_not_prepared(self):
        for mutation in ("digest", "missing", "unknown", "duplicate"):
            with self.subTest(mutation=mutation):
                contexts = copy.deepcopy(self.contexts)
                if mutation == "digest":
                    contexts["suite_digest"] = "stale"
                elif mutation == "missing":
                    contexts["cases"].pop()
                elif mutation == "unknown":
                    contexts["cases"][0]["id"] = "unknown"
                else:
                    contexts["cases"].append(contexts["cases"][0])
                with self.assertRaises(ValueError):
                    runner.prepare(self.suite, contexts, 146)

    def test_scores_declared_action_not_reason_or_citations(self):
        responses = self.responses()
        responses["responses"][0].update(
            action="execute_update",
            reason="present_commandが正しい。決定ノートを引用した。",
            citations=["manual"],
        )
        report = runner.score(self.manifest, responses)
        self.assertEqual(report["status"], "valid")
        self.assertEqual(report["measurement"], "declared_next_action_only")
        self.assertEqual(report["metrics"]["notes"]["correct"], 9)
        self.assertEqual(report["metrics"]["notes_with_judgment"]["correct"], 10)
        self.assertFalse(report["cases"][0]["correct"])

    def test_incomplete_or_mismatched_results_have_no_quality_score(self):
        for mutation in ("missing", "unknown", "duplicate", "action", "digest"):
            with self.subTest(mutation=mutation):
                responses = self.responses()
                if mutation == "missing":
                    responses["responses"].pop()
                elif mutation == "unknown":
                    responses["responses"][0]["run_id"] = "unknown"
                elif mutation == "duplicate":
                    responses["responses"].append(responses["responses"][0])
                elif mutation == "action":
                    responses["responses"][0]["action"] = "unknown"
                else:
                    responses["prompts_digest"] = "wrong-plan"
                report = runner.score(self.manifest, responses)
                self.assertEqual(report["status"], "invalid")
                self.assertIsNone(report["metrics"])
                self.assertTrue(report["errors"])

    def test_cli_writes_separate_prompts_and_manifest_and_rejects_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            contexts = root / "contexts.json"
            runner.write_json(contexts, self.contexts)
            output = root / "prepared"
            self.assertEqual(
                runner.main(["--dry-run", "--contexts", str(contexts), "--output", str(output)]),
                0,
            )
            self.assertEqual(runner.load_json(output / "prompts.json"), self.plan)
            self.assertEqual(runner.load_json(output / "manifest.json"), self.manifest)
            with self.assertRaises(FileExistsError):
                runner.write_json(output / "prompts.json", {})

    def test_cli_invalid_responses_emit_report_and_nonzero_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            responses = self.responses()
            responses["responses"].pop()
            runner.write_json(root / "responses.json", responses)
            runner.write_json(root / "manifest.json", self.manifest)
            result = runner.main([
                "--responses", str(root / "responses.json"),
                "--manifest", str(root / "manifest.json"),
                "--output", str(root / "report.json"),
            ])
            self.assertEqual(result, 2)
            self.assertEqual(runner.load_json(root / "report.json")["status"], "invalid")

    def test_manifest_without_paired_comparison_is_rejected(self):
        manifest = copy.deepcopy(self.manifest)
        manifest["runs"].pop()
        with self.assertRaises(ValueError):
            runner.score(manifest, self.responses())


if __name__ == "__main__":
    unittest.main()
