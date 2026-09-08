"""2026-09-06: 新規会話の計測がresumeや権限変更を暗黙に持ち込まない。"""

import contextlib
import importlib.util
import io
from pathlib import Path
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "start_observation_claude", Path(__file__).with_name("start-observation-claude.py")
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
SESSION = "0d1777c0-eecb-4f45-a4cf-b1196d3859f3"


class LauncherTests(unittest.TestCase):
    def launch(self, args=(), env=None):
        return MODULE.build_launch(MODULE.parser().parse_args(args), env or {}, SESSION, 1000)

    def test_fresh_id_is_bound_without_fabricating_host_session(self):
        command, env = self.launch()
        self.assertEqual(command, ["claude", "--session-id", SESSION])
        self.assertEqual(env["KB_APP_OBSERVATION_SESSION_ID"], SESSION)
        self.assertEqual(env["KB_APP_OBSERVATION_SESSION_STARTED_AT_MS"], "1000")
        self.assertEqual(env["KB_APP_OBSERVATION_PURPOSE"], "normal")
        self.assertNotIn("CLAUDE_CODE_SESSION_ID", env)

    def test_existing_configuration_and_host_evidence_are_preserved(self):
        inherited = {"CLAUDE_CODE_SESSION_ID": "parent", "KB_APP_HARVEST": "off", "PATH": "fixture"}
        _, env = self.launch(env=inherited)
        for key, value in inherited.items():
            self.assertEqual(env[key], value)
        self.assertEqual(len(inherited), 3)

    def test_diagnostic_and_unknown_purpose_are_not_promoted_to_normal(self):
        for purpose in ["diagnostic", "unrecognized"]:
            _, env = self.launch(env={"KB_APP_OBSERVATION_PURPOSE": purpose})
            self.assertEqual(env["KB_APP_OBSERVATION_PURPOSE"], purpose)
        _, env = self.launch(["--diagnostic"])
        self.assertEqual(env["KB_APP_OBSERVATION_PURPOSE"], "diagnostic")

    def test_resume_and_permission_overrides_are_not_forwarded(self):
        for args in [["--resume", "old"], ["-c"], ["--fork-session"], ["--from-pr", "1"],
                     ["--session-id", SESSION], ["--dangerously-skip-permissions"],
                     ["--settings", "{}"], ["--mod", "model"]]:
            with self.subTest(args=args), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    MODULE.parser().parse_args(args)

    def test_prompt_is_an_argument_and_cannot_become_cli_options(self):
        command, _ = self.launch(["--model", "model", "--name", "name", "--", "--resume old"])
        self.assertEqual(command[-2:], ["--", "--resume old"])
        self.assertEqual(command[3:7], ["--model", "model", "--name", "name"])

    def test_each_main_launch_uses_a_new_uuid_and_no_model_in_tests(self):
        with mock.patch.object(MODULE.os, "execvpe") as execute:
            MODULE.main([])
            MODULE.main([])
        calls = execute.call_args_list
        self.assertNotEqual(calls[0].args[1][2], calls[1].args[1][2])
        for call in calls:
            self.assertEqual(call.args[1][2], call.args[2]["KB_APP_OBSERVATION_SESSION_ID"])

    def test_launch_failure_does_not_print_environment_or_prompt(self):
        for error, code in [(FileNotFoundError("SECRET"), 127), (OSError("SECRET"), 1)]:
            output = io.StringIO()
            with mock.patch.object(MODULE.os, "execvpe", side_effect=error), contextlib.redirect_stderr(output):
                self.assertEqual(MODULE.main(["PRIVATE_PROMPT"]), code)
            self.assertNotIn("SECRET", output.getvalue())
            self.assertNotIn("PRIVATE_PROMPT", output.getvalue())


if __name__ == "__main__":
    unittest.main()
