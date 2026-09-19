"""BashTool — execute shell commands."""

from __future__ import annotations

import asyncio
import os
from typing import Any

from crabcode_core.subprocess_utils import (
    decode_subprocess_output,
    managed_process_command,
    shell_command,
)
from crabcode_core.types.tool import Tool, ToolContext, ToolResult
from crabcode_core.io_worker import run_file_tool


class BashTool(Tool):
    name = "Bash"
    description = "Execute a command in the active platform shell."
    is_read_only = False
    is_concurrency_safe = False
    input_schema = {
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "The platform-shell command to execute.",
            },
            "timeout": {
                "type": "integer",
                "description": "Timeout in seconds (defaults to filesystem_timeout in settings: 3600 if unset, unlimited if null).",
            },
        },
        "required": ["command"],
    }

    async def get_prompt(self, **kwargs: Any) -> str:
        windows_chaining = (
            "- On Windows, commands run in PowerShell. Do not assume `&&` is "
            "available because Windows PowerShell 5.1 does not support it; "
            "use `; if ($?) { ... }` for conditional sequencing.\n"
            "- If PowerShell blocks npm.ps1/npx.ps1 with PSSecurityException, "
            "use npm.cmd/npx.cmd; do not change the system execution policy.\n"
            if os.name == "nt"
            else ""
        )
        return (
            "Execute a command in the active platform shell. Use for system commands, "
            "running scripts, git operations, and other terminal tasks. "
            "Prefer dedicated tools (Read, apply_patch, Edit, Write, Glob, Grep) over "
            "bash when they can accomplish the task.\n\n"
            "Commands run in an explicit Bash-compatible shell on Unix and "
            "PowerShell on Windows, with the user's environment. "
            "The command is executed in the working directory of the "
            "current session. Each call starts a new shell, so environment "
            "variables and directory changes do not persist across calls.\n\n"
            "Guidelines:\n"
            "- Always quote file paths that contain spaces with double "
            'quotes: cd "/path/with spaces" (correct) vs cd /path/with '
            "spaces (incorrect, will fail).\n"
            "- For long-running processes (dev servers, watchers), they "
            "will be killed when the timeout expires. Warn the user if "
            "a command is expected to run indefinitely.\n"
            "- When issuing multiple independent commands, call Bash "
            "multiple times in parallel rather than chaining with &&.\n"
            "- If commands depend on each other and must run sequentially, "
            "use the active platform shell's conditional chaining syntax in "
            "a single call.\n"
            f"{windows_chaining}"
            "- Do NOT use interactive commands (e.g., git rebase -i, "
            "vim, nano) — they require user input that is not supported.\n"
            "- If a command fails, read the error output carefully before "
            "retrying. Do not blindly retry the same command."
        )

    async def call(
        self, tool_input: dict[str, Any], context: ToolContext,
    ) -> ToolResult:
        return await run_file_tool(self.name, tool_input, context)

    async def _call_local(
        self,
        tool_input: dict[str, Any],
        context: ToolContext,
    ) -> ToolResult:
        command = tool_input.get("command", "")

        if not command.strip():
            return ToolResult(
                result_for_model="Error: empty command",
                is_error=True,
            )

        env = {**os.environ, **context.env}

        # Create snapshot before bash commands that might modify files
        if context.session_id:
            try:
                from crabcode_core.snapshot.tracker import pre_bash_snapshot
                # The entire operation is isolated, including snapshot I/O.
                pre_bash_snapshot(
                    context.cwd,
                    context.session_id,
                    enabled=context.snapshot_enabled,
                    max_size_mb=context.snapshot_max_size_mb,
                )
            except Exception:
                pass  # Best-effort; don't block bash if snapshot fails

        try:
            proc = await asyncio.create_subprocess_exec(
                *managed_process_command(shell_command(command)),
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                cwd=context.cwd,
                env=env,
            )

            # The parent worker supervisor owns the deadline and process group.
            stdout_bytes, stderr_bytes = await proc.communicate()

            stdout = decode_subprocess_output(stdout_bytes)
            stderr = decode_subprocess_output(stderr_bytes)
            exit_code = proc.returncode or 0

            max_chars = 100_000
            if len(stdout) > max_chars:
                stdout = stdout[:max_chars] + "\n... (truncated)"
            if len(stderr) > max_chars:
                stderr = stderr[:max_chars] + "\n... (truncated)"

            parts: list[str] = []
            if stdout:
                parts.append(stdout)
            if stderr:
                parts.append(f"stderr:\n{stderr}")
            if exit_code != 0:
                parts.append(f"Exit code: {exit_code}")

            output = "\n".join(parts) if parts else "(no output)"

            return ToolResult(
                data={"stdout": stdout, "stderr": stderr, "exit_code": exit_code},
                result_for_model=output,
                result_for_display=output,
                is_error=exit_code != 0,
            )

        except Exception as e:
            return ToolResult(
                result_for_model=f"Error executing command: {e}",
                is_error=True,
            )
