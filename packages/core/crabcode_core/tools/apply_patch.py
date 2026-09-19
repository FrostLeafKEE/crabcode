"""ApplyPatchTool — apply structured, multi-file patches safely."""

from __future__ import annotations

from dataclasses import dataclass, replace
import os
from pathlib import Path
import re
import uuid
from typing import Any, Literal

from crabcode_core.logging_utils import get_logger
from crabcode_core.text_io import normalize_newlines, read_utf8_text, write_utf8_text
from crabcode_core.tools.diff_utils import compute_diff
from crabcode_core.types.tool import (
    PermissionBehavior,
    PermissionResult,
    Tool,
    ToolContext,
    ToolResult,
)
from crabcode_core.io_worker import run_file_tool


logger = get_logger(__name__)

_BEGIN = "*** Begin Patch"
_END = "*** End Patch"
_BEGIN_RE = re.compile(r"^\s*\*\*\* Begin Patch(?:\s+\*\*\*)?\s*$")
_END_RE = re.compile(r"^\s*\*\*\* End Patch(?:\s+\*\*\*)?\s*$")
_END_OF_FILE_RE = re.compile(r"^\s*\*\*\* End of File(?:\s+\*\*\*)?\s*$")
_ACTION_RE = re.compile(r"^\*\*\* (Add|Update|Delete) File: (.+)$")
_MOVE_RE = re.compile(r"^\*\*\* Move to: (.+)$")
_HUNK_RE = re.compile(r"^@@(?:\s(.*?))?\s*$")
_RANGE_HINT_RE = re.compile(r"^-?\d+(?:,\d+)?\s+\+?\d+(?:,\d+)?(?:\s+@@)?$")
_MAX_PATCH_CHARS = 2_000_000


class PatchError(ValueError):
    """Raised when a patch cannot be parsed or applied safely."""


@dataclass(frozen=True)
class PatchLine:
    kind: Literal[" ", "+", "-"]
    text: str


@dataclass(frozen=True)
class PatchHunk:
    hint: str | None
    lines: tuple[PatchLine, ...]
    end_of_file: bool = False


@dataclass(frozen=True)
class PatchAction:
    kind: Literal["add", "update", "delete"]
    path: str
    move_to: str | None = None
    content: str | None = None
    hunks: tuple[PatchHunk, ...] = ()


@dataclass(frozen=True)
class _FileState:
    text: str
    newline: str | None
    has_bom: bool
    mode: int | None


def parse_patch(patch: str) -> list[PatchAction]:
    """Parse the Codex apply_patch envelope into validated actions."""
    if not isinstance(patch, str) or not patch.strip():
        raise PatchError("patch must be a non-empty string")
    if len(patch) > _MAX_PATCH_CHARS:
        raise PatchError(f"patch exceeds the {_MAX_PATCH_CHARS} character limit")

    lines = patch.replace("\r\n", "\n").replace("\r", "\n").split("\n")
    start = 0
    while start < len(lines) and not lines[start].strip():
        start += 1
    end = len(lines)
    while end > start and not lines[end - 1].strip():
        end -= 1
    lines = lines[start:end]
    if not lines or not _BEGIN_RE.fullmatch(lines[0]):
        raise PatchError(f"patch must start with {_BEGIN!r}")
    if (
        len(lines) >= 3
        and _END_OF_FILE_RE.fullmatch(lines[-1])
        and _END_RE.fullmatch(lines[-2])
    ):
        lines.pop()
    if len(lines) < 2 or not _END_RE.fullmatch(lines[-1]):
        raise PatchError(f"patch must end with {_END!r}")

    actions: list[PatchAction] = []
    index = 1
    final = len(lines) - 1
    while index < final:
        match = _ACTION_RE.match(lines[index])
        if not match:
            raise PatchError(f"expected a file action at patch line {index + 1}: {lines[index]!r}")
        verb, raw_path = match.groups()
        path = raw_path.strip()
        if not path:
            raise PatchError(f"empty file path at patch line {index + 1}")
        index += 1

        if verb == "Add":
            content_lines: list[str] = []
            while index < final and not _ACTION_RE.match(lines[index]):
                line = lines[index]
                if not line.startswith("+"):
                    raise PatchError(
                        f"added file content must start with '+' at patch line {index + 1}"
                    )
                content_lines.append(line[1:])
                index += 1
            content = "\n".join(content_lines)
            if content_lines:
                content += "\n"
            actions.append(PatchAction(kind="add", path=path, content=content))
            continue

        if verb == "Delete":
            actions.append(PatchAction(kind="delete", path=path))
            continue

        move_to: str | None = None
        if index < final:
            move_match = _MOVE_RE.match(lines[index])
            if move_match:
                move_to = move_match.group(1).strip()
                if not move_to:
                    raise PatchError(f"empty move destination at patch line {index + 1}")
                index += 1

        hunks: list[PatchHunk] = []
        while index < final and not _ACTION_RE.match(lines[index]):
            hunk_match = _HUNK_RE.match(lines[index])
            if not hunk_match:
                raise PatchError(f"expected a hunk header at patch line {index + 1}: {lines[index]!r}")
            raw_hint = (hunk_match.group(1) or "").strip()
            hint = raw_hint if raw_hint and not _RANGE_HINT_RE.match(raw_hint) else None
            index += 1
            hunk_lines: list[PatchLine] = []
            end_of_file = False
            while index < final:
                line = lines[index]
                if _END_OF_FILE_RE.fullmatch(line):
                    end_of_file = True
                    index += 1
                    break
                if _ACTION_RE.match(line) or _HUNK_RE.match(line):
                    break
                if not line or line[0] not in {" ", "+", "-"}:
                    raise PatchError(
                        f"hunk lines must start with space, '+' or '-' at patch line {index + 1}"
                    )
                hunk_lines.append(PatchLine(line[0], line[1:]))
                index += 1
            if not hunk_lines:
                raise PatchError("empty update hunk")
            hunks.append(PatchHunk(hint=hint, lines=tuple(hunk_lines), end_of_file=end_of_file))

        if not hunks and move_to is None:
            raise PatchError(f"update for {path!r} has no hunks")
        actions.append(
            PatchAction(kind="update", path=path, move_to=move_to, hunks=tuple(hunks))
        )

    if not actions:
        raise PatchError("patch contains no file actions")
    return actions


def patch_paths(patch: str) -> list[str]:
    """Return source and destination paths in stable order."""
    paths: list[str] = []
    for action in parse_patch(patch):
        for path in (action.path, action.move_to):
            if path and path not in paths:
                paths.append(path)
    return paths


def _resolve_path(root: Path, raw_path: str) -> Path:
    if "\x00" in raw_path:
        raise PatchError("file paths must not contain NUL bytes")
    candidate = Path(raw_path)
    if candidate.is_absolute():
        raise PatchError(f"absolute paths are not allowed: {raw_path}")
    resolved = (root / candidate).resolve(strict=False)
    try:
        resolved.relative_to(root)
    except ValueError as exc:
        raise PatchError(f"path escapes the workspace: {raw_path}") from exc
    return resolved


def _find_hint(lines: list[str], hint: str, start: int) -> int | None:
    for index in range(start, len(lines)):
        if lines[index] == hint or lines[index].strip() == hint.strip():
            return index
    return None


def _find_sequence(
    lines: list[str],
    expected: list[str],
    *,
    start: int,
    hint: str | None,
    end_of_file: bool,
) -> int | None:
    if end_of_file:
        position = len(lines) - len(expected)
        return position if position >= start and lines[position:] == expected else None

    search_start = start
    if hint:
        hinted = _find_hint(lines, hint, start)
        if hinted is None:
            return None
        search_start = hinted

    if not expected:
        return search_start
    last = len(lines) - len(expected)
    for position in range(search_start, last + 1):
        if lines[position : position + len(expected)] == expected:
            return position
    return None


def _apply_hunks(text: str, hunks: tuple[PatchHunk, ...], path: str) -> str:
    normalized = text.replace("\r\n", "\n").replace("\r", "\n")
    had_final_newline = normalized.endswith("\n")
    lines = normalized.split("\n")
    if had_final_newline:
        lines.pop()

    cursor = 0
    for hunk_number, hunk in enumerate(hunks, start=1):
        expected = [line.text for line in hunk.lines if line.kind != "+"]
        replacement = [line.text for line in hunk.lines if line.kind != "-"]
        position = _find_sequence(
            lines,
            expected,
            start=cursor,
            hint=hunk.hint,
            end_of_file=hunk.end_of_file,
        )
        if position is None:
            preview = "\\n".join(expected[:3])
            raise PatchError(
                f"hunk {hunk_number} for {path!r} did not match the current file"
                + (f": {preview!r}" if preview else "")
            )
        lines[position : position + len(expected)] = replacement
        cursor = position + len(replacement)

    result = "\n".join(lines)
    if had_final_newline:
        result += "\n"
    return result


def _read_state(path: Path) -> _FileState | None:
    if not path.exists():
        return None
    if not path.is_file():
        raise PatchError(f"path is not a regular file: {path}")
    source = read_utf8_text(path)
    return _FileState(
        text=source.text,
        newline=source.newline,
        has_bom=source.has_bom,
        mode=path.stat().st_mode,
    )


def _relative(path: Path, root: Path) -> str:
    return path.relative_to(root).as_posix()


def _render_diff(
    root: Path,
    initial: dict[Path, _FileState | None],
    final: dict[Path, _FileState | None],
) -> tuple[str, list[dict[str, Any]], dict[str, int]]:
    diffs: list[str] = []
    files: list[dict[str, Any]] = []
    totals = {"added": 0, "removed": 0}
    for path in initial:
        before = initial[path]
        after = final[path]
        if before == after:
            continue
        display_path = _relative(path, root)
        info = compute_diff(
            before.text if before else "",
            after.text if after else "",
            display_path,
        )
        if before is None:
            action = "create"
        elif after is None:
            action = "delete"
        else:
            action = "modify"
        stats = dict(info["stats"])
        totals["added"] += stats["added"]
        totals["removed"] += stats["removed"]
        files.append({"path": display_path, "action": action, "stats": stats})
        if info["diff_text"]:
            diffs.append(info["diff_text"])
    display = "\n".join(diffs)
    if len(display) > 50_000:
        display = display[:50_000] + "\n... (diff truncated)"
    return display, files, totals


def _write_transaction(
    initial: dict[Path, _FileState | None],
    final: dict[Path, _FileState | None],
) -> None:
    staged: dict[Path, Path] = {}
    try:
        for path, state in final.items():
            if state is None or state == initial[path]:
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            temporary = path.with_name(f".{path.name}.crabcode-{uuid.uuid4().hex}.tmp")
            write_utf8_text(
                temporary,
                state.text,
                newline=state.newline,
                has_bom=state.has_bom,
            )
            if state.mode is not None:
                os.chmod(temporary, state.mode)
            staged[path] = temporary

        for path, temporary in staged.items():
            os.replace(temporary, path)
        for path, state in final.items():
            if state is None and initial[path] is not None:
                path.unlink()
    except Exception as exc:
        rollback_errors: list[str] = []
        for path, state in initial.items():
            try:
                if state is None:
                    path.unlink(missing_ok=True)
                else:
                    path.parent.mkdir(parents=True, exist_ok=True)
                    write_utf8_text(
                        path,
                        state.text,
                        newline=state.newline,
                        has_bom=state.has_bom,
                    )
                    if state.mode is not None:
                        os.chmod(path, state.mode)
            except Exception as rollback_exc:
                rollback_errors.append(f"{path}: {rollback_exc}")
        detail = f"; rollback failures: {'; '.join(rollback_errors)}" if rollback_errors else ""
        raise OSError(f"failed to commit patch: {exc}{detail}") from exc
    finally:
        for temporary in staged.values():
            try:
                temporary.unlink(missing_ok=True)
            except OSError:
                pass


class ApplyPatchTool(Tool):
    name = "apply_patch"
    description = "Apply a structured patch to one or more workspace files."
    is_read_only = False
    is_concurrency_safe = False
    input_schema = {
        "type": "object",
        "properties": {
            "patch": {
                "type": "string",
                "minLength": 1,
                "description": "Patch text using the *** Begin Patch / *** End Patch format.",
            },
        },
        "required": ["patch"],
    }

    async def get_prompt(self, **kwargs: Any) -> str:
        return (
            "Apply a patch to one or more files. Prefer this tool for code edits, especially "
            "multi-file changes. The patch must use this format:\n\n"
            "*** Begin Patch\n"
            "*** Update File: path/to/file.py\n"
            "@@\n"
            " unchanged context\n"
            "-old line\n"
            "+new line\n"
            "*** End Patch\n\n"
            "Supported actions are Add File, Update File, Delete File, and an optional "
            "*** Move to: new/path immediately after Update File. Every added-file line "
            "must start with '+'. Hunk lines must start with a space, '+' or '-'. Use "
            "workspace-relative paths only. The entire patch is validated before any file "
            "is changed, and a failed hunk applies no changes."
        )

    async def validate_input(self, tool_input: dict[str, Any]) -> str | None:
        patch = tool_input.get("patch")
        if not isinstance(patch, str):
            return "patch must be a string"
        try:
            parse_patch(patch)
        except PatchError as exc:
            return str(exc)
        return None

    async def check_permissions(
        self,
        tool_input: dict[str, Any],
        context: ToolContext,
    ) -> PermissionResult:
        patch = tool_input.get("patch")
        if not isinstance(patch, str):
            return PermissionResult()
        try:
            root = Path(context.cwd).resolve()
            affected = [
                _relative(_resolve_path(root, raw_path), root)
                for raw_path in patch_paths(patch)
            ]
        except PatchError as exc:
            return PermissionResult(
                behavior=PermissionBehavior.DENY,
                reason=f"Invalid patch path: {exc}",
            )
        return PermissionResult(
            updated_input={**tool_input, "affected_paths": affected},
        )

    async def call(
        self,
        tool_input: dict[str, Any],
        context: ToolContext,
    ) -> ToolResult:
        result = await run_file_tool(self.name, tool_input, context)
        if result.is_error or not result.data or context.lsp_manager is None:
            return result
        from crabcode_core.lsp.diagnostics import collect_and_format_diagnostics

        diagnostics: list[str] = []
        for file_info in result.data.get("files", []):
            if file_info.get("action") == "delete":
                continue
            path = Path(context.cwd) / str(file_info.get("path", ""))
            diagnostics.append(
                await collect_and_format_diagnostics(context.lsp_manager, str(path))
            )
        result.result_for_model += "".join(diagnostics)
        return result

    async def _call_local(
        self,
        tool_input: dict[str, Any],
        context: ToolContext,
    ) -> ToolResult:
        patch = tool_input.get("patch", "")
        try:
            actions = parse_patch(patch)
            root = Path(context.cwd).resolve()
            initial: dict[Path, _FileState | None] = {}
            current: dict[Path, _FileState | None] = {}

            def load(path: Path) -> _FileState | None:
                if path not in current:
                    state = _read_state(path)
                    initial[path] = state
                    current[path] = state
                return current[path]

            for action in actions:
                path = _resolve_path(root, action.path)
                state = load(path)
                if action.kind == "add":
                    if state is not None:
                        raise PatchError(f"cannot add an existing file: {action.path}")
                    current[path] = _FileState(
                        text=action.content or "",
                        newline="\n",
                        has_bom=False,
                        mode=None,
                    )
                    continue
                if state is None:
                    raise PatchError(f"file does not exist: {action.path}")
                if action.kind == "delete":
                    current[path] = None
                    continue

                updated = _apply_hunks(state.text, action.hunks, action.path)
                updated_state = replace(state, text=updated)
                if action.move_to:
                    destination = _resolve_path(root, action.move_to)
                    if destination == path:
                        raise PatchError("move destination must differ from the source path")
                    if load(destination) is not None:
                        raise PatchError(f"move destination already exists: {action.move_to}")
                    current[path] = None
                    current[destination] = updated_state
                else:
                    current[path] = updated_state

            final = {path: current[path] for path in initial}
            display, files, totals = _render_diff(root, initial, final)
            if not files:
                raise PatchError("patch would not change any files")

            if context.session_id:
                try:
                    from crabcode_core.snapshot.tracker import (
                        pre_bash_snapshot,
                        track_snapshot_for_file,
                    )

                    snapshot_id = pre_bash_snapshot(
                        context.cwd,
                        context.session_id,
                        enabled=context.snapshot_enabled,
                        max_size_mb=context.snapshot_max_size_mb,
                        tool_name=self.name,
                        action="patch",
                    )
                    if snapshot_id is None:
                        for path, before in initial.items():
                            after = final[path]
                            if before == after:
                                continue
                            if before is None:
                                action_name = "create"
                            elif after is None:
                                action_name = "delete"
                            else:
                                action_name = "modify"
                            track_snapshot_for_file(
                                cwd=context.cwd,
                                session_id=context.session_id,
                                file_path=str(path),
                                old_content=before.text if before is not None else None,
                                action=action_name,
                            )
                except Exception:
                    logger.debug("Failed to track snapshots for ApplyPatch", exc_info=True)

            _write_transaction(initial, final)
            changed = len(files)
            summary = (
                f"Applied patch to {changed} file{'s' if changed != 1 else ''} "
                f"[+{totals['added']}/-{totals['removed']}]"
            )
            return ToolResult(
                data={
                    "files": files,
                    "affected_paths": [
                        _relative(path, root)
                        for path in initial
                        if initial[path] != final[path]
                    ],
                    "stats": totals,
                },
                result_for_model=summary,
                result_for_display=f"{summary}\n{display}" if display else summary,
            )
        except (OSError, UnicodeError, PatchError) as exc:
            return ToolResult(result_for_model=f"ApplyPatch error: {exc}", is_error=True)


__all__ = ["ApplyPatchTool", "PatchError", "parse_patch", "patch_paths"]
