"""Bounded process isolation for filesystem calls that may never return.

Neither process creation nor pipe I/O runs on the event loop or its default
executor. A daemon supervisor owns each child until it is reaped. Cancelled
children still consume a slot: a broken mount must not produce unlimited
processes, threads, or open pipes. SIGKILL is a request, not a successful reap.
"""

from __future__ import annotations

import asyncio
from dataclasses import asdict
import json
import math
import os
import signal
import subprocess
import sys
import threading
from typing import Any, Callable

from crabcode_core.subprocess_utils import managed_process_command, subprocess_group_options
from crabcode_core.types.config import DEFAULT_FILESYSTEM_TIMEOUT

_SLOTS = threading.BoundedSemaphore(8)
_ACTIVE_GUARD = threading.Lock()
_ACTIVE_ABORTS: set[Callable[[], None]] = set()
IO_TIMEOUT = DEFAULT_FILESYSTEM_TIMEOUT

# Do not resolve/stat package paths in the caller. Imports and path resolution
# happen in the child, including for editable installs on network storage.
_BOOTSTRAP = """
import sys
sys.path[:0] = sys.argv[1:]
from crabcode_core.io_worker import _worker_main
_worker_main()
"""


def _kill(process: subprocess.Popen[bytes]) -> None:
    if process.returncode is not None:
        return
    try:
        if os.name == "nt":
            process.kill()  # The Windows supervisor owns a kill-on-close job.
        else:
            os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def abort_io_workers() -> None:
    """Request worker termination during emergency exit, without waiting."""
    with _ACTIVE_GUARD:
        callbacks = tuple(_ACTIVE_ABORTS)
    for abort in callbacks:
        abort()


async def run_io(module: str, function: str, *args: Any,
                 timeout: float | None = IO_TIMEOUT, **kwargs: Any) -> Any:
    """Call an internal function in an expendable process; None disables the deadline."""
    slots = _SLOTS
    if not slots.acquire(blocking=False):
        raise OSError("Filesystem workers are busy or still blocked; stop retrying until storage recovers")
    loop = asyncio.get_running_loop()
    result = loop.create_future()
    guard = threading.Lock()
    abandoned = False
    process: subprocess.Popen[bytes] | None = None

    def abort() -> None:
        nonlocal abandoned
        with guard:
            abandoned = True
            child = process
        if child is not None:
            _kill(child)

    def deliver(value: Any, error: BaseException | None) -> None:
        if not result.done():
            if error is None:
                result.set_result(value)
            else:
                result.set_exception(error)

    def supervise() -> None:
        nonlocal process
        value, error = None, None
        child = None
        try:
            payload = json.dumps([module, function, args, kwargs, os.getcwd()]).encode("utf-8")
            # Preserve editable-install search roots, but never search the
            # target workspace implicitly via an empty sys.path entry.
            roots = [p for p in sys.path if p and os.path.isabs(p)]
            child = subprocess.Popen(
                managed_process_command([sys.executable, "-I", "-c", _BOOTSTRAP, *roots]),
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                cwd=os.path.abspath(os.sep), **subprocess_group_options(),
            )
            with guard:
                process = child
                cancelled = abandoned
            if cancelled:
                _kill(child)
                payload = None  # Never begin an operation after its caller left.
            stdout, stderr = child.communicate(payload)
            if child.returncode:
                raise OSError(f"Filesystem worker failed ({child.returncode}): "
                              + stderr.decode("utf-8", errors="replace")[-2000:])
            reply = json.loads(stdout)
            if "error" in reply:
                raise OSError(reply["error"])
            value = reply["result"]
        except BaseException as exc:
            error = exc
        finally:
            # Even exceptional pipe handling must not release a live child slot.
            if child is not None:
                try:
                    if child.poll() is None:
                        _kill(child)
                    child.wait()  # Only this daemon thread can wait indefinitely.
                finally:
                    for stream in (child.stdin, child.stdout, child.stderr):
                        if stream is not None:
                            stream.close()
            with _ACTIVE_GUARD:
                _ACTIVE_ABORTS.discard(abort)
            slots.release()
            try:
                loop.call_soon_threadsafe(deliver, value, error)
            except RuntimeError:
                pass  # Caller and its event loop have already exited.

    try:
        with _ACTIVE_GUARD:
            _ACTIVE_ABORTS.add(abort)
        threading.Thread(target=supervise, name="crabcode-fs-worker", daemon=True).start()
    except BaseException:
        with _ACTIVE_GUARD:
            _ACTIVE_ABORTS.discard(abort)
        slots.release()
        raise
    try:
        return await asyncio.wait_for(result, timeout=timeout)
    finally:
        if not result.done() or result.cancelled():
            abort()


async def run_file_tool(name: str, tool_input: dict[str, Any], context: Any) -> Any:
    from crabcode_core.types.tool import ToolResult

    # All session objects/callbacks stay in the parent. Snapshot metadata is
    # explicit; filesystem changes and their backups execute in the same child.
    fields = {key: getattr(context, key) for key in (
        "cwd", "session_id", "snapshot_enabled", "snapshot_max_size_mb", "env",
        "filesystem_timeout",
    )}
    timeout = context.filesystem_timeout
    if name == "Bash" and "timeout" in tool_input:
        try:
            timeout = float(tool_input["timeout"])
            if not math.isfinite(timeout) or timeout <= 0:
                raise ValueError
        except (ValueError, TypeError):
            return ToolResult(result_for_model="Error: timeout must be a positive number of seconds",
                              is_error=True)
    try:
        data = await run_io(__name__, "_call_file_tool", name, tool_input, fields,
                            timeout=timeout)
        return ToolResult(**data)
    except (OSError, asyncio.TimeoutError) as exc:
        detail = str(exc) or (
            f"timed out after {timeout:g}s" if timeout is not None else type(exc).__name__
        )
        if name in {"apply_patch", "Edit", "Write", "Bash"}:
            detail += ". Write outcome is unknown; inspect affected files and snapshots before retrying"
        return ToolResult(result_for_model=f"{name} filesystem operation failed: {detail}",
                          is_error=True)


async def _call_file_tool(name: str, tool_input: dict[str, Any], fields: dict[str, Any]) -> dict:
    from importlib import import_module
    from crabcode_core.types.tool import ToolContext

    modules = {"Read": ("file_read", "FileReadTool"), "Write": ("file_write", "FileWriteTool"),
               "apply_patch": ("apply_patch", "ApplyPatchTool"),
               "Edit": ("file_edit", "FileEditTool"), "Glob": ("glob", "GlobTool"),
               "Grep": ("grep", "GrepTool"), "Bash": ("bash", "BashTool")}
    module, cls = modules[name]
    tool = getattr(import_module(f"crabcode_core.tools.{module}"), cls)()
    return asdict(await tool._call_local(tool_input, ToolContext(**fields)))


def _worker_main() -> None:
    import importlib
    import inspect

    try:
        module, function, args, kwargs, caller_cwd = json.load(sys.stdin)
        os.chdir(caller_cwd)
        value = getattr(importlib.import_module(module), function)(*args, **kwargs)
        if inspect.isawaitable(value):
            value = asyncio.run(value)
        reply = {"result": value}
    except Exception as exc:
        reply = {"error": f"{type(exc).__name__}: {exc}"}
    sys.stdout.write(json.dumps(reply, ensure_ascii=True))
