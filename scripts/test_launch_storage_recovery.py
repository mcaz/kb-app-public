"""2026-09-07: 実Vault・設定・processに触れず、復旧起動の停止条件を合成データで守る。"""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "launch_storage_recovery", Path(__file__).with_name("launch-storage-recovery.py")
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

FIXTURE_LEGACY_EXE = Path("/fixture/old-checkout/target/release/kb")


def launch_args(*extra):
    return ["--manifest", "/fixture/manifest.json", "--legacy-executable", str(FIXTURE_LEGACY_EXE), *extra]


def process(pid, executable=None, started="Mon Sep 7 19:00:00 2026"):
    return MODULE.Process(pid, 10, os.getuid(), started, executable or str(MODULE.INSTALLED_EXE))


def manifest():
    return {
        "format_version": 1,
        "source_commit": "a" * 40,
        "source_tree": "b" * 40,
        "candidate_executable_sha256": "c" * 64,
        "candidate_bundle_sha256": "d" * 64,
        "installed_executable_sha256": "e" * 64,
        "desktop_config_sha256": "f" * 64,
        "autostart_plist_sha256": "0" * 64,
    }


class FakeSystem:
    def __init__(self, inventories):
        self.inventories = list(inventories)
        self.terminated = []
        self.quits = 0
        self.commands = []

    def processes(self):
        if len(self.inventories) > 1:
            return self.inventories.pop(0)
        return self.inventories[0]

    def quit_gui(self):
        self.quits += 1

    def terminate(self, target):
        self.terminated.append(target)

    def sleep(self, _seconds):
        pass

    def command(self, command):
        self.commands.append(command)
        return ""


class ConfigTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.path = self.root / "claude_desktop_config.json"
        self.config = {
            "mcpServers": {
                "kb-app": {"command": str(FIXTURE_LEGACY_EXE), "args": ["mcp", "--vault", "private"]},
                "other-service": {"command": "unrelated", "env": {"SECRET": "preserve-me"}},
            },
            "preferences": {"locale": "ja", "nested": [True, None]},
        }
        self.original = json.dumps(self.config).encode()
        self.path.write_bytes(self.original)

    def tearDown(self):
        self.temp.cleanup()

    def test_only_the_legacy_entry_is_removed_and_exact_backup_is_private(self):
        updated_hash = MODULE.disable_legacy_registration(self.path, MODULE.digest(self.original), FIXTURE_LEGACY_EXE)
        expected = json.loads(self.original)
        del expected["mcpServers"]["kb-app"]
        self.assertEqual(json.loads(self.path.read_bytes()), expected)
        self.assertEqual(MODULE.digest(self.path.read_bytes()), updated_hash)
        backup = list(self.root.glob("*.bak"))
        self.assertEqual(len(backup), 1)
        self.assertEqual(backup[0].read_bytes(), self.original)
        self.assertEqual(stat.S_IMODE(backup[0].stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(self.path.stat().st_mode), 0o600)

    def test_original_hash_mismatch_never_creates_a_backup_or_replaces_config(self):
        with self.assertRaisesRegex(MODULE.LaunchError, "desktop_config_changed"):
            MODULE.disable_legacy_registration(self.path, "0" * 64, FIXTURE_LEGACY_EXE)
        self.assertEqual(self.path.read_bytes(), self.original)
        self.assertEqual(list(self.root.glob("*.bak")), [])

    def test_concurrent_change_before_rename_is_preserved_and_backup_remains(self):
        with mock.patch.object(MODULE, "read_regular", side_effect=[self.original, b"changed"]):
            with mock.patch.object(MODULE.os, "replace") as replace:
                with self.assertRaisesRegex(MODULE.LaunchError, "desktop_config_changed"):
                    MODULE.disable_legacy_registration(self.path, MODULE.digest(self.original), FIXTURE_LEGACY_EXE)
                replace.assert_not_called()
        self.assertEqual(self.path.read_bytes(), self.original)
        self.assertEqual(list(self.root.glob("*.bak"))[0].read_bytes(), self.original)
        self.assertEqual(list(self.root.glob(".kb-recovery-*")), [])

    def test_unknown_registration_and_duplicate_json_are_not_rewritten(self):
        self.config["mcpServers"]["kb-app"]["command"] = "different"
        with self.assertRaisesRegex(MODULE.LaunchError, "legacy_registration_changed"):
            MODULE.without_legacy_registration(json.dumps(self.config), FIXTURE_LEGACY_EXE)
        with self.assertRaisesRegex(MODULE.LaunchError, "duplicate_json_key"):
            MODULE.without_legacy_registration('{"mcpServers":{},"mcpServers":{}}', FIXTURE_LEGACY_EXE)

    def test_symlink_config_is_not_followed(self):
        link = self.root / "link.json"
        link.symlink_to(self.path)
        with self.assertRaisesRegex(MODULE.LaunchError, "symbolic_link_refused"):
            MODULE.disable_legacy_registration(link, MODULE.digest(self.original), FIXTURE_LEGACY_EXE)
        self.assertEqual(self.path.read_bytes(), self.original)

    def test_existing_backup_is_not_overwritten(self):
        backup = self.path.with_name(self.path.name + ".pre-storage-recovery-" + MODULE.digest(self.original)[:12] + ".bak")
        backup.write_bytes(b"different")
        with self.assertRaisesRegex(MODULE.LaunchError, "desktop_backup_conflict"):
            MODULE.disable_legacy_registration(self.path, MODULE.digest(self.original), FIXTURE_LEGACY_EXE)
        self.assertEqual(backup.read_bytes(), b"different")
        self.assertEqual(self.path.read_bytes(), self.original)

    def test_exact_previously_disabled_config_can_resume_without_rewriting(self):
        expected_hash = MODULE.digest(self.original)
        updated_hash = MODULE.disable_legacy_registration(self.path, expected_hash, FIXTURE_LEGACY_EXE)
        updated = self.path.read_bytes()
        modified = self.path.stat().st_mtime_ns
        self.assertEqual(MODULE.approved_desktop_config_hash(self.path, expected_hash, FIXTURE_LEGACY_EXE), updated_hash)
        self.assertEqual(MODULE.disable_legacy_registration(self.path, expected_hash, FIXTURE_LEGACY_EXE), updated_hash)
        self.assertEqual(self.path.read_bytes(), updated)
        self.assertEqual(self.path.stat().st_mtime_ns, modified)
        self.assertEqual(len(list(self.root.glob("*.bak"))), 1)

    def test_resume_never_accepts_an_unrelated_change_after_disabling(self):
        expected_hash = MODULE.digest(self.original)
        MODULE.disable_legacy_registration(self.path, expected_hash, FIXTURE_LEGACY_EXE)
        changed = json.loads(self.path.read_bytes())
        changed["preferences"]["locale"] = "en"
        self.path.write_text(json.dumps(changed))
        before = self.path.read_bytes()
        with self.assertRaisesRegex(MODULE.LaunchError, "desktop_config_changed"):
            MODULE.disable_legacy_registration(self.path, expected_hash, FIXTURE_LEGACY_EXE)
        self.assertEqual(self.path.read_bytes(), before)

    def test_resume_requires_the_original_private_backup(self):
        expected_hash = MODULE.digest(self.original)
        MODULE.disable_legacy_registration(self.path, expected_hash, FIXTURE_LEGACY_EXE)
        backup = MODULE.desktop_backup_path(self.path, expected_hash)
        backup.chmod(0o644)
        with self.assertRaisesRegex(MODULE.LaunchError, "desktop_backup_conflict"):
            MODULE.approved_desktop_config_hash(self.path, expected_hash, FIXTURE_LEGACY_EXE)
        backup.chmod(0o600)
        backup.write_bytes(b"unrelated private backup")
        with self.assertRaisesRegex(MODULE.LaunchError, "desktop_backup_conflict"):
            MODULE.approved_desktop_config_hash(self.path, expected_hash, FIXTURE_LEGACY_EXE)
        backup.unlink()
        with self.assertRaisesRegex(MODULE.LaunchError, "desktop_config_changed"):
            MODULE.approved_desktop_config_hash(self.path, expected_hash, FIXTURE_LEGACY_EXE)


class ProcessTests(unittest.TestCase):
    def test_parser_preserves_spaces_in_executable_and_requires_complete_rows(self):
        rows = MODULE.parse_processes(
            " 123  10 501 Mon Sep  7 19:00:00 2026 /Applications/Some App.app/Contents/MacOS/App\n"
        )
        self.assertEqual(rows[0].executable, "/Applications/Some App.app/Contents/MacOS/App")
        self.assertEqual(rows[0].started_at, "Mon Sep 7 19:00:00 2026")
        with self.assertRaisesRegex(MODULE.LaunchError, "process_inventory_unparseable"):
            MODULE.parse_processes("unknown output")

    def test_gui_quits_first_then_only_known_kb_targets_receive_term(self):
        gui, mcp = process(100), process(101)
        legacy = process(102, str(FIXTURE_LEGACY_EXE))
        parent = process(10, "/Applications/Codex.app/Contents/MacOS/Codex")
        system = FakeSystem([[gui, mcp, legacy, parent], [mcp, legacy, parent], [parent]])
        self.assertEqual(MODULE.quiesce(system, FIXTURE_LEGACY_EXE), 2)
        self.assertEqual(system.quits, 1)
        self.assertEqual(system.terminated, [mcp, legacy])

    def test_unknown_kb_or_tunnel_or_running_desktop_stops_before_quit(self):
        paths = [
            "/somewhere/target/release/kb",
            "/somewhere/tunnel-client",
            "/Applications/Claude.app/Contents/MacOS/Claude",
            str(MODULE.CANDIDATE_EXE),
        ]
        for path in paths:
            with self.subTest(path=path):
                system = FakeSystem([[process(110, path)]])
                with self.assertRaises(MODULE.LaunchError):
                    MODULE.quiesce(system, FIXTURE_LEGACY_EXE)
                self.assertEqual(system.quits, 0)
                self.assertEqual(system.terminated, [])

    def test_survivors_are_not_force_killed_or_repeatedly_signaled(self):
        old = process(100)
        system = FakeSystem([[old]])
        with self.assertRaisesRegex(MODULE.LaunchError, "kb_process_survived"):
            MODULE.quiesce(system, FIXTURE_LEGACY_EXE)
        self.assertEqual(system.terminated, [old])

    def test_restart_during_quiet_window_stops_without_killing_new_process(self):
        old, restarted = process(100), process(101)
        system = FakeSystem([[old], [old], [], [restarted]])
        with self.assertRaisesRegex(MODULE.LaunchError, "kb_process_restarted"):
            MODULE.quiesce(system, FIXTURE_LEGACY_EXE)
        self.assertEqual(system.terminated, [old])

    def test_pid_reuse_is_rejected_without_sending_signal(self):
        old = process(100)
        reused = process(100, started="Mon Sep 7 19:01:00 2026")
        system = MODULE.TerminalSystem()
        with mock.patch.object(system, "processes", return_value=[reused]), mock.patch.object(MODULE.os, "kill") as kill:
            with self.assertRaisesRegex(MODULE.LaunchError, "process_identity_changed"):
                system.terminate(old)
            kill.assert_not_called()

    def test_terminal_process_command_does_not_request_arguments_or_environment(self):
        system = MODULE.TerminalSystem()
        with mock.patch.object(system, "command", return_value="") as command:
            self.assertEqual(system.processes(), [])
        self.assertEqual(command.call_args.args[0], ["/bin/ps", "-ww", "-axo", "pid=,ppid=,uid=,lstart=,comm="])

    def test_candidate_is_opened_only_with_the_fixed_recovery_flag(self):
        candidate = process(200, str(MODULE.CANDIDATE_EXE))
        system = FakeSystem([[], [candidate], [candidate]])
        MODULE.launch_candidate(system, FIXTURE_LEGACY_EXE)
        self.assertEqual(system.commands, [["/usr/bin/open", "-n", "-a", str(MODULE.CANDIDATE_APP), "--args", "--storage-recovery"]])

    def test_restart_after_candidate_launch_is_not_accepted(self):
        candidate, restarted = process(200, str(MODULE.CANDIDATE_EXE)), process(100)
        system = FakeSystem([[], [candidate], [candidate, restarted], [restarted]])
        with self.assertRaisesRegex(MODULE.LaunchError, "kb_process_restarted_after_launch"):
            MODULE.launch_candidate(system, FIXTURE_LEGACY_EXE)
        self.assertEqual(system.terminated, [candidate])

    def test_second_candidate_during_acceptance_is_stopped_and_rejected(self):
        first, second = process(200, str(MODULE.CANDIDATE_EXE)), process(201, str(MODULE.CANDIDATE_EXE))
        system = FakeSystem([[], [first], [first, second], []])
        with self.assertRaisesRegex(MODULE.LaunchError, "multiple_recovery_processes"):
            MODULE.launch_candidate(system, FIXTURE_LEGACY_EXE)
        self.assertEqual(set(system.terminated), {first, second})

    def test_failed_acceptance_reports_a_recovery_process_that_survives_term(self):
        candidate, restarted = process(200, str(MODULE.CANDIDATE_EXE)), process(100)
        system = FakeSystem([[], [candidate], [candidate, restarted]])
        with self.assertRaisesRegex(MODULE.LaunchError, "kb_process_restarted_after_launch_and_recovery_stop_failed"):
            MODULE.launch_candidate(system, FIXTURE_LEGACY_EXE)
        self.assertEqual(system.terminated, [candidate])

    def test_candidate_is_stopped_when_forbidden_process_appears_in_same_snapshot(self):
        candidate = process(200, str(MODULE.CANDIDATE_EXE))
        for path in ["/Applications/Claude.app/Contents/MacOS/Claude", "/somewhere/tunnel-client", "/unknown/kb"]:
            for during_acceptance in [False, True]:
                with self.subTest(path=path, during_acceptance=during_acceptance):
                    forbidden = process(201, path)
                    observations = [[], [candidate]] if during_acceptance else [[]]
                    system = FakeSystem(observations + [[candidate, forbidden], [forbidden]])
                    with self.assertRaises(MODULE.LaunchError):
                        MODULE.launch_candidate(system, FIXTURE_LEGACY_EXE)
                    self.assertEqual(system.terminated, [candidate])

    def test_launch_command_failure_still_captures_and_stops_started_candidate(self):
        candidate = process(200, str(MODULE.CANDIDATE_EXE))
        system = FakeSystem([[], [candidate], []])
        with mock.patch.object(system, "command", side_effect=MODULE.LaunchError("system_command_failed")):
            with self.assertRaisesRegex(MODULE.LaunchError, "system_command_failed$"):
                MODULE.launch_candidate(system, FIXTURE_LEGACY_EXE)
        self.assertEqual(system.terminated, [candidate])

    def test_failed_launch_and_failed_inventory_reports_unconfirmed_status(self):
        system = FakeSystem([[]])
        with mock.patch.object(system, "command", side_effect=MODULE.LaunchError("system_command_failed")), mock.patch.object(system, "processes", side_effect=[[], MODULE.LaunchError("system_command_failed")]):
            with self.assertRaisesRegex(MODULE.LaunchError, "system_command_failed_and_recovery_status_unconfirmed"):
                MODULE.launch_candidate(system, FIXTURE_LEGACY_EXE)
        self.assertEqual(system.terminated, [])


class ManifestTests(unittest.TestCase):
    # 2026-09-08: 公開版では旧実行ファイルやmanifestを個人のcheckoutから推測しない。
    def test_missing_ambiguous_or_invalid_paths_stop_before_reads_and_system_actions(self):
        invalid = [
            [],
            ["--check-only"],
            ["--manifest", "/fixture/manifest.json"],
            ["--legacy-executable", str(FIXTURE_LEGACY_EXE)],
            ["--manifest", "relative.json", "--legacy-executable", str(FIXTURE_LEGACY_EXE)],
            ["--manifest", "/fixture/manifest.json", "--legacy-executable", "target/release/kb"],
            ["--manifest", "/fixture/manifest.json", "--legacy-executable", "/fixture/../kb"],
            ["--manifest", "/fixture/manifest.json", "--legacy-executable", "/bin/sh"],
            ["--manifest", "/fixture/manifest.json", "--legacy-executable", str(MODULE.INSTALLED_EXE)],
            ["--manifest", "/fixture/manifest.json", "--legacy-executable", str(MODULE.CANDIDATE_EXE)],
            launch_args("--manifest", "/another/manifest.json"),
            launch_args("--check-only", "--check-only"),
            launch_args("--unknown", "PRIVATE_PATH"),
        ]
        for args in invalid:
            with self.subTest(args=args):
                output = io.StringIO()
                with mock.patch.object(MODULE, "read_regular") as read, mock.patch.object(MODULE, "TerminalSystem") as system, contextlib.redirect_stderr(output):
                    self.assertEqual(MODULE.main(args), 1)
                    read.assert_not_called()
                    system.assert_not_called()
                self.assertNotIn("PRIVATE_PATH", output.getvalue())

    def test_explicit_paths_are_forwarded_without_guessing_a_different_legacy_target(self):
        explicit = Path("/fixture/another-checkout/target/release/kb")
        fake = FakeSystem([[]])
        args = ["--manifest", "/fixture/approved.json", "--legacy-executable", str(explicit)]
        with mock.patch.object(MODULE, "read_regular", return_value=json.dumps(manifest())) as read, mock.patch.object(MODULE, "TerminalSystem", return_value=fake), mock.patch.object(MODULE, "approved_desktop_config_hash", return_value="f" * 64) as approve, mock.patch.object(MODULE, "verify_sources") as verify, mock.patch.object(MODULE, "checked_inventory") as inventory, mock.patch.object(MODULE, "disable_legacy_registration", return_value="1" * 64) as disable, mock.patch.object(MODULE, "quiesce", return_value=2) as stop, mock.patch.object(MODULE, "launch_candidate") as launch, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(MODULE.main(args), 0)
            read.assert_called_once_with(Path("/fixture/approved.json"))
            approve.assert_called_once_with(MODULE.DESKTOP_CONFIG, "f" * 64, explicit)
            disable.assert_called_once_with(MODULE.DESKTOP_CONFIG, "f" * 64, explicit)
            inventory.assert_called_once_with(fake, explicit)
            stop.assert_called_once_with(fake, explicit)
            launch.assert_called_once_with(fake, explicit)
            self.assertEqual(verify.call_args_list, [mock.call(fake, manifest(), "f" * 64), mock.call(fake, manifest(), "1" * 64)])

    def test_config_and_process_checks_accept_only_the_explicit_legacy_path(self):
        explicit = Path("/fixture/different-checkout/target/release/kb")
        original = json.dumps({"mcpServers": {"kb-app": {"command": str(FIXTURE_LEGACY_EXE)}}})
        with self.assertRaisesRegex(MODULE.LaunchError, "legacy_registration_changed"):
            MODULE.without_legacy_registration(original, explicit)
        system = FakeSystem([[process(102, str(FIXTURE_LEGACY_EXE))]])
        with self.assertRaisesRegex(MODULE.LaunchError, "unrecognized_kb_process"):
            MODULE.quiesce(system, explicit)
        self.assertEqual(system.quits, 0)
        self.assertEqual(system.terminated, [])

    def test_placeholder_manifest_stops_before_any_system_or_config_action(self):
        pending = manifest()
        pending["candidate_executable_sha256"] = "TODO_VERIFIED_EXECUTABLE_SHA256"
        output = io.StringIO()
        with mock.patch.object(MODULE, "read_regular", return_value=json.dumps(pending)), mock.patch.object(MODULE, "TerminalSystem") as system, contextlib.redirect_stderr(output):
            self.assertEqual(MODULE.main(launch_args()), 1)
            system.assert_not_called()
        self.assertIn("manifest_not_finalized", output.getvalue())

    def test_check_only_never_changes_config_or_stops_processes(self):
        with mock.patch.object(MODULE, "read_regular", return_value=json.dumps(manifest())), mock.patch.object(MODULE, "approved_desktop_config_hash", return_value="f" * 64), mock.patch.object(MODULE, "verify_sources"), mock.patch.object(MODULE, "checked_inventory"), mock.patch.object(MODULE, "disable_legacy_registration") as disable, mock.patch.object(MODULE, "quiesce") as stop, mock.patch.object(MODULE, "launch_candidate") as launch, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(MODULE.main(launch_args("--check-only")), 0)
            disable.assert_not_called()
            stop.assert_not_called()
            launch.assert_not_called()

    def test_arbitrary_executable_or_manifest_arguments_are_rejected(self):
        with mock.patch.object(MODULE, "read_regular") as read, contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(MODULE.main(["--manifest", "/other/file"]), 1)
            self.assertEqual(MODULE.main(["/other/application"]), 1)
            read.assert_not_called()

    def test_system_failures_never_print_configuration_or_raw_details(self):
        output = io.StringIO()
        with mock.patch.object(MODULE, "read_regular", side_effect=OSError("SECRET_CONFIG")), contextlib.redirect_stderr(output):
            self.assertEqual(MODULE.main(launch_args()), 1)
        self.assertNotIn("SECRET_CONFIG", output.getvalue())

    def test_bundle_digest_includes_assets_and_rejects_symlinks(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name).resolve()
            asset = root / "asset.js"
            asset.write_text("first")
            before = MODULE.bundle_digest(root)
            asset.write_text("second")
            self.assertNotEqual(MODULE.bundle_digest(root), before)
            (root / "linked.js").symlink_to(asset)
            with self.assertRaisesRegex(MODULE.LaunchError, "candidate_symlink_refused"):
                MODULE.bundle_digest(root)


if __name__ == "__main__":
    unittest.main()
