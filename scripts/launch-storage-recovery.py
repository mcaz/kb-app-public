"""2026-09-07: 本人のTerminalから旧writerを止め、検証済み復旧専用GUIだけを起動する。

Vault・bindingは読まず変更しない。旧Desktop登録の退避だけを行い、3面への再接続は
復旧後の製品GUIへ任せる。manifestが未確定なら、設定変更やprocess取得より先に止まる。

検証済みmanifestと、旧Desktop登録にある実行ファイルの絶対pathを明示する。
    python3 scripts/launch-storage-recovery.py --manifest /absolute/manifest.json \
        --legacy-executable /absolute/old-checkout/target/release/kb [--check-only]
旧実行ファイルの場所を推測しない。現在のcheckoutにあるrelease bundleだけを候補にする。
"""

import hashlib
import json
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import sys
import tempfile
import time
from typing import NamedTuple


SOURCE_ROOT = Path(__file__).absolute().parent.parent
DESKTOP_CONFIG = Path.home() / "Library/Application Support/Claude/claude_desktop_config.json"
INSTALLED_EXE = Path("/Applications/kb-app.app/Contents/MacOS/kb-app")
CANDIDATE_APP = SOURCE_ROOT / "target/release/bundle/macos/kb-app.app"
CANDIDATE_EXE = CANDIDATE_APP / "Contents/MacOS/kb-app"
AUTOSTART_PLIST = Path.home() / "Library/LaunchAgents/app.kb.desktop.plist"
PROCESS_LINE = re.compile(
    r"^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\S+\s+\S+\s+\d+\s+\d{2}:\d{2}:\d{2}\s+\d{4})\s+(.+)$"
)


class LaunchError(Exception):
    """例外detailには設定・processの引数を入れず、固定codeだけを返す。"""


class Process(NamedTuple):
    pid: int
    parent_pid: int
    uid: int
    started_at: str
    executable: str


class LaunchInputs(NamedTuple):
    manifest_path: Path
    legacy_executable: Path
    check_only: bool


def parse_inputs(args):
    # 不完全な指定で個人設定を読まず、重複flagの後勝ちでも対象を変えない。
    values = {}
    position = 0
    while position < len(args):
        flag = args[position]
        if flag in values or flag not in {"--manifest", "--legacy-executable", "--check-only"}:
            raise LaunchError("unsupported_arguments")
        if flag == "--check-only":
            values[flag] = True
            position += 1
            continue
        if position + 1 >= len(args) or args[position + 1].startswith("--"):
            raise LaunchError("required_path_missing")
        raw = args[position + 1]
        path = Path(raw)
        if not path.is_absolute() or ".." in path.parts or any(ord(char) < 32 for char in raw):
            raise LaunchError("absolute_path_required")
        values[flag] = path
        position += 2
    if "--manifest" not in values or "--legacy-executable" not in values:
        raise LaunchError("required_path_missing")
    legacy = values["--legacy-executable"]
    if legacy.name not in {"kb", "kb-app"} or legacy in {INSTALLED_EXE, CANDIDATE_EXE}:
        raise LaunchError("legacy_executable_invalid")
    return LaunchInputs(values["--manifest"], legacy, values.get("--check-only", False))


def known_executables(legacy_executable):
    return frozenset((str(INSTALLED_EXE), str(legacy_executable)))


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_regular(path):
    if path.resolve() != path.absolute():
        raise LaunchError("symbolic_link_refused")
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW), "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise LaunchError("regular_file_required")
        return stream.read()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise LaunchError("duplicate_json_key")
        result[key] = value
    return result


def load_json(data):
    return json.loads(data, object_pairs_hook=unique_object)


def validate_manifest(manifest):
    lengths = {
        "source_commit": 40,
        "source_tree": 40,
        "candidate_executable_sha256": 64,
        "candidate_bundle_sha256": 64,
        "installed_executable_sha256": 64,
        "desktop_config_sha256": 64,
        "autostart_plist_sha256": 64,
    }
    if not isinstance(manifest, dict) or set(manifest) != {"format_version", *lengths}:
        raise LaunchError("manifest_shape_mismatch")
    if manifest["format_version"] != 1:
        raise LaunchError("manifest_version_mismatch")
    for key, length in lengths.items():
        if not isinstance(manifest[key], str) or not re.fullmatch("[0-9a-f]{%d}" % length, manifest[key]):
            raise LaunchError("manifest_not_finalized")


def bundle_digest(app):
    """実行fileだけでなく、画面資産も同じ候補であることを固定する。"""
    result = hashlib.sha256()
    for path in sorted(app.rglob("*")):
        if path.is_symlink():
            raise LaunchError("candidate_symlink_refused")
        relative = path.relative_to(app).as_posix().encode()
        if path.is_dir():
            result.update(b"directory\0" + relative + b"\0")
        elif path.is_file():
            result.update(b"file\0" + relative + b"\0" + digest(read_regular(path)).encode() + b"\0")
        else:
            raise LaunchError("candidate_entry_refused")
    return result.hexdigest()


def without_legacy_registration(original, legacy_executable):
    value = load_json(original)
    if not isinstance(value, dict) or not isinstance(value.get("mcpServers"), dict):
        raise LaunchError("desktop_config_shape_mismatch")
    entry = value["mcpServers"].get("kb-app")
    if not isinstance(entry, dict) or entry.get("command") != str(legacy_executable):
        raise LaunchError("legacy_registration_changed")
    del value["mcpServers"]["kb-app"]
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode()


def desktop_backup_path(path, expected_hash):
    return path.with_name(path.name + ".pre-storage-recovery-" + expected_hash[:12] + ".bak")


def verified_desktop_backup(path, expected_hash):
    backup = desktop_backup_path(path, expected_hash)
    metadata = backup.stat()
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o600:
        raise LaunchError("desktop_backup_conflict")
    original = read_regular(backup)
    if digest(original) != expected_hash:
        raise LaunchError("desktop_backup_conflict")
    return original


def desktop_config_state(path, expected_hash, legacy_executable):
    if path.stat().st_uid != os.getuid():
        raise LaunchError("desktop_config_owner_mismatch")
    current = read_regular(path)
    if digest(current) == expected_hash:
        return current, without_legacy_registration(current, legacy_executable), False
    # 前回の停止処理が途中で止まっても、本人だけの原本退避から導ける変更だけを再受入する。
    try:
        original = verified_desktop_backup(path, expected_hash)
    except FileNotFoundError:
        raise LaunchError("desktop_config_changed")
    updated = without_legacy_registration(original, legacy_executable)
    if current != updated:
        raise LaunchError("desktop_config_changed")
    return original, updated, True


def approved_desktop_config_hash(path, expected_hash, legacy_executable):
    original, updated, disabled = desktop_config_state(path, expected_hash, legacy_executable)
    return digest(updated if disabled else original)


def disable_legacy_registration(path, expected_hash, legacy_executable):
    original, updated, disabled = desktop_config_state(path, expected_hash, legacy_executable)
    if disabled:
        return digest(updated)
    backup = desktop_backup_path(path, expected_hash)
    # 設定には他MCPの資格情報もあり得るため、退避も置換候補も本人だけが読める。
    try:
        with os.fdopen(os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as stream:
            stream.write(original)
            stream.flush()
            os.fsync(stream.fileno())
    except FileExistsError:
        if verified_desktop_backup(path, expected_hash) != original:
            raise LaunchError("desktop_backup_conflict") from None
    candidate = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, prefix=".kb-recovery-", delete=False) as stream:
            candidate = Path(stream.name)
            stream.write(updated)
            stream.flush()
            os.fsync(stream.fileno())
        if read_regular(path) != original:
            raise LaunchError("desktop_config_changed")
        os.replace(candidate, path)
        candidate = None
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
        if read_regular(path) != updated:
            raise LaunchError("desktop_config_changed_after_replace")
    finally:
        if candidate is not None:
            candidate.unlink(missing_ok=True)
    return digest(updated)


def parse_processes(text):
    processes = []
    for line in text.splitlines():
        if not line.strip():
            continue
        match = PROCESS_LINE.fullmatch(line)
        if match is None:
            raise LaunchError("process_inventory_unparseable")
        pid, parent, uid, started, executable = match.groups()
        processes.append(Process(int(pid), int(parent), int(uid), " ".join(started.split()), executable))
    return processes


class TerminalSystem:
    def command(self, args):
        try:
            result = subprocess.run(args, capture_output=True, text=True, timeout=30, check=False)
        except (OSError, subprocess.SubprocessError):
            raise LaunchError("system_command_failed") from None
        if result.returncode != 0:
            raise LaunchError("system_command_failed")
        return result.stdout

    def processes(self):
        # command/args/環境は取得しない。開始時刻とUIDはPID再利用・他ユーザー誤終了防止だけに使う。
        output = self.command(["/bin/ps", "-ww", "-axo", "pid=,ppid=,uid=,lstart=,comm="])
        return [process for process in parse_processes(output) if process.uid == os.getuid()]

    def quit_gui(self):
        self.command([
            "/usr/bin/osascript", "-e",
            'if application id "app.kb.desktop" is running then tell application id "app.kb.desktop" to quit',
        ])

    def terminate(self, process):
        current = next((item for item in self.processes() if item.pid == process.pid), None)
        if current is None:
            return
        if current != process:
            raise LaunchError("process_identity_changed")
        try:
            os.kill(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except OSError:
            raise LaunchError("process_stop_failed") from None

    def sleep(self, seconds):
        time.sleep(seconds)


def validate_inventory(processes, legacy_executable, allow_candidate=False):
    for process in processes:
        name = Path(process.executable).name.lower()
        if name in {"claude", "claude desktop"} and ".app/contents/macos/" in process.executable.lower():
            raise LaunchError("claude_desktop_running")
        if name == "tunnel-client":
            raise LaunchError("tunnel_process_running")
        if name in {"kb", "kb-app"} and process.executable not in known_executables(legacy_executable):
            if not (allow_candidate and process.executable == str(CANDIDATE_EXE)):
                raise LaunchError("unrecognized_kb_process")


def checked_inventory(system, legacy_executable, allow_candidate=False):
    processes = system.processes()
    validate_inventory(processes, legacy_executable, allow_candidate)
    return processes


def quiesce(system, legacy_executable):
    checked_inventory(system, legacy_executable)
    system.quit_gui()
    system.sleep(2)
    targets = [process for process in checked_inventory(system, legacy_executable) if process.executable in known_executables(legacy_executable)]
    for process in targets:
        system.terminate(process)
    quiet_samples = 0
    for _ in range(15):
        remaining = [process for process in checked_inventory(system, legacy_executable) if process.executable in known_executables(legacy_executable)]
        if any(process not in targets for process in remaining):
            raise LaunchError("kb_process_restarted")
        quiet_samples = quiet_samples + 1 if not remaining else 0
        if quiet_samples >= 5:
            return len(targets)
        system.sleep(1)
    raise LaunchError("kb_process_survived")


def verify_sources(system, manifest, config_hash):
    if SOURCE_ROOT.resolve() != SOURCE_ROOT:
        raise LaunchError("source_root_changed")
    for revision, key in [("HEAD", "source_commit"), ("HEAD^{tree}", "source_tree")]:
        if system.command(["/usr/bin/git", "--no-optional-locks", "-C", str(SOURCE_ROOT), "rev-parse", revision]).strip() != manifest[key]:
            raise LaunchError("source_revision_changed")
    if system.command(["/usr/bin/git", "--no-optional-locks", "-C", str(SOURCE_ROOT), "status", "--porcelain=v1"]).strip():
        raise LaunchError("source_worktree_changed")
    checks = [
        (INSTALLED_EXE, manifest["installed_executable_sha256"]),
        (CANDIDATE_EXE, manifest["candidate_executable_sha256"]),
        (DESKTOP_CONFIG, config_hash),
        (AUTOSTART_PLIST, manifest["autostart_plist_sha256"]),
    ]
    if any(digest(read_regular(path)) != expected for path, expected in checks):
        raise LaunchError("fixed_file_changed")
    if bundle_digest(CANDIDATE_APP) != manifest["candidate_bundle_sha256"]:
        raise LaunchError("candidate_bundle_changed")
    system.command(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(CANDIDATE_APP)])


def launch_candidate(system, legacy_executable):
    if any(process.executable in known_executables(legacy_executable) for process in checked_inventory(system, legacy_executable)):
        raise LaunchError("kb_process_restarted")
    observed = set()

    def capture():
        processes = system.processes()
        # 異常processが同時に見えても、新たに起動した専用GUIの停止対象を失わない。
        observed.update(process for process in processes if process.executable == str(CANDIDATE_EXE))
        validate_inventory(processes, legacy_executable, allow_candidate=True)
        return processes

    try:
        system.command(["/usr/bin/open", "-n", "-a", str(CANDIDATE_APP), "--args", "--storage-recovery"])
        for _ in range(15):
            processes = capture()
            candidates = [process for process in processes if process.executable == str(CANDIDATE_EXE)]
            if any(process.executable in known_executables(legacy_executable) for process in processes):
                raise LaunchError("kb_process_restarted_after_launch")
            if len(candidates) == 1:
                system.sleep(1)
                accepted = capture()
                if any(process.executable in known_executables(legacy_executable) for process in accepted):
                    raise LaunchError("kb_process_restarted_after_launch")
                if sum(process.executable == str(CANDIDATE_EXE) for process in accepted) > 1:
                    raise LaunchError("multiple_recovery_processes")
                if candidates[0] in accepted:
                    return
            elif len(candidates) > 1:
                raise LaunchError("multiple_recovery_processes")
            system.sleep(1)
        raise LaunchError("recovery_gui_not_observed")
    except LaunchError as failure:
        # 起動を失敗扱いしたのに専用画面だけが残り、そこで復旧されることを避ける。
        inventory_unconfirmed = False
        try:
            observed.update(process for process in system.processes() if process.executable == str(CANDIDATE_EXE))
        except LaunchError:
            inventory_unconfirmed = True
        for candidate in observed:
            try:
                system.terminate(candidate)
            except LaunchError:
                raise LaunchError(str(failure) + "_and_recovery_stop_failed") from None
        if observed:
            for _ in range(5):
                if not any(process.executable == str(CANDIDATE_EXE) for process in system.processes()):
                    break
                system.sleep(1)
            else:
                raise LaunchError(str(failure) + "_and_recovery_stop_failed") from None
        if inventory_unconfirmed:
            raise LaunchError(str(failure) + "_and_recovery_status_unconfirmed") from None
        raise


def main(args):
    try:
        inputs = parse_inputs(args)
        legacy_executable = inputs.legacy_executable
        manifest = load_json(read_regular(inputs.manifest_path))
        validate_manifest(manifest)
        system = TerminalSystem()
        config_hash = approved_desktop_config_hash(DESKTOP_CONFIG, manifest["desktop_config_sha256"], legacy_executable)
        verify_sources(system, manifest, config_hash)
        checked_inventory(system, legacy_executable)
        if inputs.check_only:
            print("固定済みの反映元・配置先・設定・processを確認しました。変更していません。")
            return 0
        updated_hash = disable_legacy_registration(DESKTOP_CONFIG, manifest["desktop_config_sha256"], legacy_executable)
        print("旧Claude Desktop登録を本人だけが読める形で保全し、無効化しました。")
        stopped = quiesce(system, legacy_executable)
        verify_sources(system, manifest, updated_hash)
        launch_candidate(system, legacy_executable)
        print("旧KB processを停止しました（%d件）。復旧専用画面を起動しました。" % stopped)
        print("まだ復元は実行していません。通常のアプリ反映・Desktop再接続は復旧後に行います。")
        return 0
    except LaunchError as error:
        print("復旧起動を停止しました: %s。設定やprocessの引数は出力していません。" % error, file=sys.stderr)
    except Exception:
        print("復旧起動を停止しました: unexpected_failure。詳細は出力していません。", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
