"""Exercise filesystem hangs without requiring a broken network mount."""

from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
import signal
import shlex
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, PropertyMock, patch

from pydantic import ValidationError

from crabcode_core import io_worker
from crabcode_core.config.manager import ConfigManager
from crabcode_core.subprocess_utils import terminate_process_tree
from crabcode_core.tools.file_read import FileReadTool
from crabcode_core.tools.file_write import FileWriteTool
from crabcode_core.tools.file_edit import FileEditTool
from crabcode_core.tools.glob import GlobTool
from crabcode_core.tools.grep import GrepTool
from crabcode_core.tools.bash import BashTool
from crabcode_core.tools.apply_patch import ApplyPatchTool
from crabcode_core.types.config import CrabCodeSettings
from crabcode_core.types.tool import ToolContext


async def wait_until(predicate, timeout=5):
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() >= deadline:
            raise AssertionError("condition did not become true")
        await asyncio.sleep(0.01)


def blocking_stat_bootstrap(target: Path, ready: Path) -> str:
    # Inject the hang inside the real child, before the tool runs. Parent
    # filesystem calls remain untouched; the ready marker proves it got there.
    injection = f'''
import os, time, shutil
_original_stat = os.stat
def blocked_stat(path, *args, **kwargs):
    if isinstance(path, (str, bytes, os.PathLike)) and os.fsdecode(path) in { [str(target), str(target.resolve())]!r}:
        with open({str(ready)!r}, "w") as marker:
            marker.write(str(os.getpid()))
        time.sleep(60)
    return _original_stat(path, *args, **kwargs)
os.stat = blocked_stat
shutil.which = lambda *args, **kwargs: None
_worker_main()
'''
    return io_worker._BOOTSTRAP.replace("_worker_main()", injection)


class FilesystemTimeoutConfigTests(unittest.TestCase):
    def test_default_timeout_is_one_hour(self):
        self.assertEqual(CrabCodeSettings().filesystem_timeout, 3600)
        self.assertEqual(ToolContext().filesystem_timeout, 3600)
        self.assertEqual(io_worker.IO_TIMEOUT, 3600)

    def test_settings_layers_override_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            user_path = Path(directory) / "user.json"
            project_path = Path(directory) / "project.json"
            user_path.write_text(json.dumps({"filesystem_timeout": 7200}))
            for timeout in (42.5, None):
                with self.subTest(timeout=timeout):
                    project_path.write_text(json.dumps({"filesystem_timeout": timeout}))
                    with patch.object(ConfigManager, "settings_file_paths", new_callable=PropertyMock,
                                      return_value={"userSettings": str(user_path),
                                                    "projectSettings": str(project_path)}):
                        settings = ConfigManager(cwd=directory).load()
                    self.assertEqual(settings.filesystem_timeout, timeout)
                    self.assertEqual(settings.model_dump()["filesystem_timeout"], timeout)

    def test_timeout_must_be_finite_and_positive(self):
        for timeout in (0, -1, float("inf"), float("-inf"), float("nan"), "invalid"):
            with self.subTest(timeout=timeout), self.assertRaises(ValidationError):
                CrabCodeSettings(filesystem_timeout=timeout)


class FileIsolationTests(unittest.IsolatedAsyncioTestCase):
    async def test_file_tools_share_default_and_configured_timeouts(self):
        for settings in (CrabCodeSettings(), CrabCodeSettings(filesystem_timeout=7200)):
            context = ToolContext(filesystem_timeout=settings.filesystem_timeout)
            for tool in (FileReadTool(), FileWriteTool(), FileEditTool(), ApplyPatchTool(), GlobTool(), GrepTool(), BashTool()):
                with self.subTest(tool=tool.name, timeout=settings.filesystem_timeout):
                    with patch.object(io_worker, "run_io", new_callable=AsyncMock,
                                      side_effect=asyncio.TimeoutError) as run_io:
                        result = await tool.call({}, context)
                    self.assertEqual(run_io.await_args.kwargs["timeout"], settings.filesystem_timeout)
                    self.assertTrue(result.is_error)
                    self.assertIn(f"timed out after {settings.filesystem_timeout:g}s", result.result_for_model)

    async def test_file_tools_can_disable_timeouts(self):
        settings = CrabCodeSettings(filesystem_timeout=None)
        context = ToolContext(filesystem_timeout=settings.filesystem_timeout)
        for tool in (FileReadTool(), FileWriteTool(), FileEditTool(), ApplyPatchTool(), GlobTool(), GrepTool(), BashTool()):
            with self.subTest(tool=tool.name):
                with patch.object(io_worker, "run_io", new_callable=AsyncMock,
                                  return_value={"result_for_model": "done"}) as run_io:
                    result = await tool.call({}, context)
                self.assertFalse(result.is_error)
                self.assertIsNone(run_io.await_args.kwargs["timeout"])

    async def test_worker_errors_survive_disabled_timeout(self):
        with patch.object(io_worker, "run_io", new_callable=AsyncMock, side_effect=OSError()):
            result = await FileReadTool().call({}, ToolContext(filesystem_timeout=None))
        self.assertTrue(result.is_error)
        self.assertIn("Read filesystem operation failed: OSError", result.result_for_model)

    async def test_shell_explicit_timeout_overrides_filesystem_default(self):
        for default in (42, None):
            context = ToolContext(filesystem_timeout=default)
            for timeout in (1, 7200):
                with self.subTest(default=default, timeout=timeout):
                    with patch.object(io_worker, "run_io", new_callable=AsyncMock,
                                      return_value={"result_for_model": "done"}) as run_io:
                        result = await BashTool().call({"command": "echo done", "timeout": timeout}, context)
                    self.assertFalse(result.is_error)
                    self.assertEqual(run_io.await_args.kwargs["timeout"], timeout)

    async def test_shell_rejects_invalid_explicit_timeouts(self):
        for timeout in (0, -1, float("inf"), float("nan"), None, "invalid"):
            with self.subTest(timeout=timeout):
                with patch.object(io_worker, "run_io", new_callable=AsyncMock) as run_io:
                    result = await BashTool().call({"command": "echo done", "timeout": timeout}, ToolContext())
                self.assertTrue(result.is_error)
                self.assertIn("timeout must be a positive number of seconds", result.result_for_model)
                run_io.assert_not_awaited()

    @unittest.skipIf(os.name == "nt", "POSIX process group verification")
    async def test_shell_timeout_terminates_descendants(self):
        with tempfile.TemporaryDirectory() as directory:
            ready = Path(directory) / "ready"
            code = ("import os, signal, time; "
                    "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                    f"open({str(ready)!r}, 'w').write(str(os.getpid())); time.sleep(60)")
            command = f"{shlex.quote(sys.executable)} -c {shlex.quote(code)}"
            result = await BashTool().call({"command": command, "timeout": 1}, ToolContext(cwd=directory))
            self.assertTrue(result.is_error)
            self.assertIn("timed out", result.result_for_model)
            self.assertTrue(ready.exists(), "command did not start")
            pid = int(ready.read_text())
            def exited():
                state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                                       capture_output=True, text=True).stdout.strip()
                return not state or state.startswith("Z")
            await wait_until(exited)

    async def test_shell_preserves_cwd_environment_and_output(self):
        with tempfile.TemporaryDirectory() as directory:
            context = ToolContext(cwd=directory, env={"CRABCODE_IO_TEST": "custom"}, filesystem_timeout=None)
            command = ('Write-Output $env:CRABCODE_IO_TEST; (Get-Location).Path'
                       if os.name == "nt" else 'printf "%s\\n" "$CRABCODE_IO_TEST"; pwd')
            result = await BashTool().call({"command": command}, context)
            self.assertFalse(result.is_error, result.result_for_model)
            self.assertIn("custom", result.data["stdout"])
            actual_cwd = Path(result.data["stdout"].splitlines()[-1])
            self.assertEqual(actual_cwd.resolve(), Path(directory).resolve())

    async def test_tools_preserve_read_write_edit_glob_and_search(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "中文 file.txt"
            original = b"\xef\xbb\xbf" + "甲\r\n乙\r\n".encode()
            path.write_bytes(original)
            context = ToolContext(cwd=directory, session_id="isolation-test", filesystem_timeout=None)
            read = await FileReadTool().call({"path": path.name, "offset": -1}, context)
            self.assertFalse(read.is_error, read.result_for_model)
            self.assertIn("2|乙", read.result_for_model)
            edit = await FileEditTool().call(
                {"file_path": path.name, "old_string": "甲\n乙", "new_string": "甲\n丙"}, context)
            self.assertFalse(edit.is_error, edit.result_for_model)
            self.assertEqual(path.read_bytes(), b"\xef\xbb\xbf" + "甲\r\n丙\r\n".encode())
            backups = list((Path(directory) / ".crabcode/snapshots/files").glob("**/content"))
            self.assertEqual(len(backups), 1)
            self.assertEqual(backups[0].read_bytes(), original)
            write = await FileWriteTool().call({"file_path": path.name, "content": "新\n内容\n"}, context)
            self.assertFalse(write.is_error, write.result_for_model)
            self.assertEqual(path.read_bytes(), b"\xef\xbb\xbf" + "新\r\n内容\r\n".encode())
            glob = await GlobTool().call({"pattern": "*.txt"}, context)
            self.assertEqual(glob.data["files"], [str(path)])
            grep = await GrepTool().call({"pattern": "内容", "path": path.name}, context)
            self.assertFalse(grep.is_error, grep.result_for_model)
            self.assertIn("内容", grep.result_for_model)

    async def test_relative_context_keeps_caller_working_directory(self):
        with tempfile.TemporaryDirectory(dir=os.getcwd()) as directory:
            path = Path(directory) / "sample.txt"
            path.write_text("relative")
            result = await FileReadTool().call({"file_path": path.name},
                ToolContext(cwd=os.path.relpath(directory)))
            self.assertFalse(result.is_error, result.result_for_model)
            self.assertEqual(result.data["content"], "relative")

    async def test_all_file_tools_time_out_without_stalling_event_loop(self):
        cases = [
            (FileReadTool(), {"file_path": "target.txt"}),
            (FileWriteTool(), {"file_path": "target.txt", "content": "changed"}),
            (FileEditTool(), {"file_path": "target.txt", "old_string": "before", "new_string": "after"}),
            (ApplyPatchTool(), {"patch": "*** Begin Patch\n*** Update File: target.txt\n@@\n-before\n+after\n*** End Patch"}),
            (GlobTool(), {"pattern": "*.txt"}),
            (GrepTool(), {"pattern": "before", "path": "target.txt"}),
            (BashTool(), {"command": "echo done"}),
        ]
        for tool, inputs in cases:
            with self.subTest(tool=tool.name), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "target.txt"
                path.write_text("before")
                ready = Path(directory) / "ready"
                target = Path(directory) if tool.name in {"Glob", "Bash"} else path
                bootstrap = blocking_stat_bootstrap(target, ready)
                ticks = 0
                async def heartbeat():
                    nonlocal ticks
                    while True:
                        ticks += 1
                        await asyncio.sleep(0.01)
                pulse = asyncio.create_task(heartbeat())
                try:
                    with patch.object(io_worker, "_BOOTSTRAP", bootstrap):
                        started = time.monotonic()
                        settings = CrabCodeSettings(filesystem_timeout=1)
                        result = await tool.call(inputs, ToolContext(
                            cwd=directory, session_id="timeout-test",
                            filesystem_timeout=settings.filesystem_timeout,
                        ))
                    self.assertTrue(ready.exists(), f"{tool.name}: worker never reached injected stat: {result}")
                    self.assertTrue(result.is_error)
                    self.assertIn("timed out", result.result_for_model)
                    self.assertLess(time.monotonic() - started, 2)
                    self.assertGreater(ticks, 20)
                    self.assertEqual(path.read_text(), "before")
                    if tool.name in {"Write", "Edit", "apply_patch", "Bash"}:
                        self.assertIn("outcome is unknown", result.result_for_model)
                finally:
                    pulse.cancel()
                    await asyncio.gather(pulse, return_exceptions=True)

    async def test_outer_cancellation_returns_promptly(self):
        with tempfile.TemporaryDirectory() as directory:
            target, ready = Path(directory) / "target", Path(directory) / "ready"
            target.write_text("hello")
            with patch.object(io_worker, "_BOOTSTRAP", blocking_stat_bootstrap(target, ready)):
                task = asyncio.create_task(FileReadTool().call(
                    {"file_path": str(target)}, ToolContext(cwd=directory, filesystem_timeout=None)))
                await wait_until(ready.exists)
                started = time.monotonic()
                task.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await asyncio.wait_for(task, 1)
                self.assertLess(time.monotonic() - started, 0.5)

    async def test_unreaped_worker_retains_capacity(self):
        gate = threading.Event()
        slots = threading.BoundedSemaphore(1)
        class UnkillableProcess:
            returncode = None
            stdin = stdout = stderr = None
            def communicate(self, payload):
                gate.wait()
                self.returncode = 0
                return b'{"result": null}', b''
            def poll(self):
                return self.returncode
            def wait(self):
                gate.wait()
                return 0
        with patch.object(io_worker, "_SLOTS", slots), \
             patch.object(io_worker.subprocess, "Popen", return_value=UnkillableProcess()), \
             patch.object(io_worker, "_kill"):
            try:
                with self.assertRaises(asyncio.TimeoutError):
                    await io_worker.run_io("unused", "unused", timeout=0.05)
                with self.assertRaisesRegex(OSError, "still blocked"):
                    await io_worker.run_io("unused", "unused", timeout=0.05)
            finally:
                gate.set()
                await wait_until(lambda: slots.acquire(blocking=False))
                slots.release()

    async def test_cancel_during_process_creation_never_sends_operation(self):
        gate = threading.Event()
        slots = threading.BoundedSemaphore(1)
        child = SimpleNamespace(returncode=0, stdin=None, stdout=None, stderr=None,
                                poll=lambda: 0, wait=lambda: 0)
        from unittest.mock import Mock
        child.communicate = Mock(return_value=(b'{"result": null}', b''))
        def slow_spawn(*args, **kwargs):
            gate.wait()
            return child
        with patch.object(io_worker, "_SLOTS", slots), \
             patch.object(io_worker.subprocess, "Popen", side_effect=slow_spawn):
            try:
                with self.assertRaises(asyncio.TimeoutError):
                    await io_worker.run_io("unused", "unused", timeout=0.05)
            finally:
                gate.set()
                await wait_until(lambda: slots.acquire(blocking=False))
                slots.release()
            child.communicate.assert_called_once_with(None)

    def test_lsp_uri_encoding_never_queries_metadata(self):
        from crabcode_core.lsp.client import _path_to_uri
        with patch.object(Path, "resolve", side_effect=AssertionError("metadata lookup")):
            uri = _path_to_uri(os.path.abspath("中文 file.py"))
        self.assertIn("%20file.py", uri)

    @unittest.skipIf(os.name == "nt", "POSIX signal branch")
    async def test_process_tree_wait_is_bounded_after_sigkill(self):
        process = SimpleNamespace(pid=12345, returncode=None,
                                  wait=AsyncMock(side_effect=lambda: None))
        async def blocked_wait():
            await asyncio.Event().wait()
        process.wait = blocked_wait
        with patch("crabcode_core.subprocess_utils.os.killpg") as kill:
            await asyncio.wait_for(terminate_process_tree(process, timeout=0.02), 0.5)
        self.assertEqual([call.args[1] for call in kill.call_args_list],
                         [signal.SIGTERM, signal.SIGKILL])


class ForcedExitTests(unittest.TestCase):
    def test_shutdown_deadline_survives_a_blocked_main_thread(self):
        roots = [p for p in sys.path if p and os.path.isabs(p)]
        script = f'''
import sys, threading
sys.path[:0] = {roots!r}
from crabcode_cli.repl import _shutdown_deadline
with _shutdown_deadline(timeout=0.1):
    threading.Event().wait()
'''
        child = subprocess.run([sys.executable, "-c", script], timeout=10,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(child.returncode, 130, child.stderr.decode(errors="replace"))

    def test_stalled_log_sink_does_not_block_emit_or_close(self):
        from crabcode_core.logging_utils import _AsyncFileHandler
        import logging
        gate, entered = threading.Event(), threading.Event()
        class StalledSink:
            def __enter__(self):
                return self
            def __exit__(self, *args):
                pass
            def write(self, text):
                entered.set()
                gate.wait()
            def flush(self):
                pass
        with patch("crabcode_core.logging_utils.open", return_value=StalledSink(), create=True):
            handler = _AsyncFileHandler(Path("unused"))
            try:
                record = logging.LogRecord("test", logging.ERROR, __file__, 1, "message", (), None)
                handler.handle(record)
                self.assertTrue(entered.wait(2))
                started = time.monotonic()
                for _ in range(1100):
                    handler.handle(record)
                handler.close()
                self.assertLess(time.monotonic() - started, 1)
                self.assertLessEqual(handler._records.qsize(), 1024)
            finally:
                gate.set()
                handler._writer.join(timeout=3)
            self.assertFalse(handler._writer.is_alive())

    def test_force_exit_does_not_flush_or_join_threads(self):
        roots = [p for p in sys.path if p and os.path.isabs(p)]
        script = f'''
import sys, threading
sys.path[:0] = {roots!r}
from crabcode_cli.repl import _force_exit
class BlockedOutput:
    def flush(self):
        threading.Event().wait()
sys.stdout = sys.stderr = BlockedOutput()
threading.Thread(target=threading.Event().wait).start()
_force_exit(130)
'''
        child = subprocess.run([sys.executable, "-c", script], timeout=10,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(child.returncode, 130, child.stderr.decode(errors="replace"))
