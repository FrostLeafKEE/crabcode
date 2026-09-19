from __future__ import annotations

import asyncio
import codecs
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from crabcode_core.api.base import StreamChunk
from crabcode_core.permissions.manager import PermissionManager
from crabcode_core.query.loop import QueryParams, query_loop
from crabcode_core.tools import get_default_tools
from crabcode_core.tools.apply_patch import ApplyPatchTool, PatchError, parse_patch
from crabcode_core.types.config import ApiConfig, PermissionRule, PermissionsSettings
from crabcode_core.types.event import PermissionRequestEvent, ToolResultEvent
from crabcode_core.types.message import create_user_message
from crabcode_core.types.tool import PermissionBehavior, ToolContext
from crabcode_gateway.acp.types import to_locations, to_tool_kind


class ApplyPatchParserTests(unittest.TestCase):
    def test_parses_all_supported_actions(self) -> None:
        actions = parse_patch(
            """*** Begin Patch
*** Add File: added.txt
+added
*** Update File: old.txt
*** Move to: moved.txt
@@ function_name
 before
-old
+new
*** Delete File: removed.txt
*** End Patch"""
        )
        self.assertEqual([action.kind for action in actions], ["add", "update", "delete"])
        self.assertEqual(actions[0].content, "added\n")
        self.assertEqual(actions[1].move_to, "moved.txt")
        self.assertEqual(actions[1].hunks[0].hint, "function_name")

    def test_accepts_unambiguous_envelope_variants(self) -> None:
        for begin, end in (
            ("*** Begin Patch ***", "*** End Patch ***"),
            ("  *** Begin Patch ***  ", "  *** End Patch  "),
            (
                "*** Begin Patch ***",
                "*** End Patch ***\n*** End of File ***",
            ),
        ):
            with self.subTest(begin=begin, end=end):
                actions = parse_patch(
                    f"""\n{begin}
*** Add File: added.txt
+added
{end}\n"""
                )
                self.assertEqual(len(actions), 1)
                self.assertEqual(actions[0].path, "added.txt")
                self.assertEqual(actions[0].content, "added\n")

    def test_rejects_malformed_envelopes_and_hunks(self) -> None:
        for value in (
            "",
            "*** Update File: a.txt\n@@\n-old\n+new",
            "*** Begin Patch\n*** Update File: a.txt\nold\n*** End Patch",
            "*** Begin Patch\n*** Add File: a.txt\nplain\n*** End Patch",
        ):
            with self.subTest(value=value), self.assertRaises(PatchError):
                parse_patch(value)


class ApplyPatchToolTests(unittest.IsolatedAsyncioTestCase):
    async def test_applies_multi_file_patch_and_returns_final_diff(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "old.txt").write_text("before\nold\n", encoding="utf-8")
            (root / "removed.txt").write_text("remove me\n", encoding="utf-8")
            patch_text = """*** Begin Patch
*** Update File: old.txt
*** Move to: moved.txt
@@
 before
-old
+new
*** Add File: added.txt
+created
*** Delete File: removed.txt
*** End Patch"""

            result = await ApplyPatchTool().call(
                {"patch": patch_text},
                ToolContext(cwd=directory, filesystem_timeout=None),
            )

            self.assertFalse(result.is_error, result.result_for_model)
            self.assertFalse((root / "old.txt").exists())
            self.assertEqual((root / "moved.txt").read_text(), "before\nnew\n")
            self.assertEqual((root / "added.txt").read_text(), "created\n")
            self.assertFalse((root / "removed.txt").exists())
            self.assertEqual(result.data["stats"], {"added": 3, "removed": 3})
            self.assertEqual(
                result.data["affected_paths"],
                ["old.txt", "moved.txt", "added.txt", "removed.txt"],
            )
            self.assertIn("--- old.txt", result.result_for_display)
            self.assertIn("+++ moved.txt", result.result_for_display)
            self.assertNotIn("\n\n-old", result.result_for_display)

    async def test_preserves_utf8_bom_and_crlf(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "中文.txt"
            path.write_bytes(codecs.BOM_UTF8 + "甲\r\n乙\r\n".encode())
            result = await ApplyPatchTool().call(
                {
                    "patch": """*** Begin Patch
*** Update File: 中文.txt
@@
 甲
-乙
+丙
*** End Patch"""
                },
                ToolContext(cwd=directory, filesystem_timeout=None),
            )
            self.assertFalse(result.is_error, result.result_for_model)
            self.assertEqual(path.read_bytes(), codecs.BOM_UTF8 + "甲\r\n丙\r\n".encode())

    async def test_failed_hunk_and_escaping_path_leave_workspace_unchanged(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "target.txt"
            path.write_text("actual\n")
            for patch_text in (
                """*** Begin Patch
*** Update File: target.txt
@@
-missing
+changed
*** End Patch""",
                """*** Begin Patch
*** Add File: ../escaped.txt
+nope
*** End Patch""",
            ):
                with self.subTest(patch=patch_text):
                    result = await ApplyPatchTool().call(
                        {"patch": patch_text},
                        ToolContext(cwd=directory, filesystem_timeout=None),
                    )
                    self.assertTrue(result.is_error)
                    self.assertEqual(path.read_text(), "actual\n")
                    self.assertFalse((Path(directory).parent / "escaped.txt").exists())

    async def test_write_failure_rolls_back_files_already_replaced(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = root / "first.txt"
            second = root / "second.txt"
            first.write_text("one\n")
            second.write_text("two\n")
            patch_text = """*** Begin Patch
*** Update File: first.txt
@@
-one
+changed one
*** Update File: second.txt
@@
-two
+changed two
*** End Patch"""
            real_replace = os.replace
            calls = 0

            def fail_second_replace(source: str | os.PathLike[str], target: str | os.PathLike[str]) -> None:
                nonlocal calls
                calls += 1
                if calls == 2:
                    raise OSError("simulated failure")
                real_replace(source, target)

            with patch("crabcode_core.tools.apply_patch.os.replace", side_effect=fail_second_replace):
                result = await ApplyPatchTool()._call_local(
                    {"patch": patch_text},
                    ToolContext(cwd=directory, filesystem_timeout=None),
                )

            self.assertTrue(result.is_error)
            self.assertEqual(first.read_text(), "one\n")
            self.assertEqual(second.read_text(), "two\n")
            self.assertEqual(list(root.glob("*.tmp")), [])

    async def test_records_one_full_snapshot_for_a_multi_file_patch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "first.txt").write_text("one\n")
            patch_text = """*** Begin Patch
*** Update File: first.txt
@@
-one
+changed
*** Add File: second.txt
+two
*** End Patch"""

            with (
                patch(
                    "crabcode_core.snapshot.tracker.pre_bash_snapshot",
                    return_value="snapshot-id",
                ) as full_snapshot,
                patch("crabcode_core.snapshot.tracker.track_snapshot_for_file") as file_snapshot,
            ):
                result = await ApplyPatchTool()._call_local(
                    {"patch": patch_text},
                    ToolContext(
                        cwd=directory,
                        session_id="session-id",
                        filesystem_timeout=None,
                    ),
                )

            self.assertFalse(result.is_error, result.result_for_model)
            full_snapshot.assert_called_once_with(
                directory,
                "session-id",
                enabled=True,
                max_size_mb=1024,
                tool_name="apply_patch",
                action="patch",
            )
            file_snapshot.assert_not_called()

    async def test_permission_input_includes_every_patch_path(self) -> None:
        tool = ApplyPatchTool()
        tool_input = {
            "patch": """*** Begin Patch
*** Update File: src/a.py
*** Move to: src/b.py
@@
-old
+new
*** End Patch"""
        }
        permission = await tool.check_permissions(tool_input, ToolContext(cwd="."))
        assert permission.updated_input is not None
        self.assertEqual(permission.updated_input["affected_paths"], ["src/a.py", "src/b.py"])

        manager = PermissionManager(
            PermissionsSettings(allow=[PermissionRule(tool="apply_patch", path="src/*")])
        )
        self.assertEqual(
            manager.check(tool, permission.updated_input).behavior,
            PermissionBehavior.ALLOW,
        )
        mixed = {**permission.updated_input, "affected_paths": ["src/a.py", "outside.txt"]}
        self.assertEqual(manager.check(tool, mixed).behavior, PermissionBehavior.ASK)
        deny_manager = PermissionManager(
            PermissionsSettings(deny=[PermissionRule(tool="apply_patch", path="outside*")])
        )
        self.assertEqual(deny_manager.check(tool, mixed).behavior, PermissionBehavior.DENY)
        ask_manager = PermissionManager(
            PermissionsSettings(ask=[PermissionRule(tool="apply_patch", path="outside*")])
        )
        self.assertEqual(ask_manager.check(tool, mixed).behavior, PermissionBehavior.ASK)

        normalized = await tool.check_permissions(
            {
                "patch": """*** Begin Patch
*** Add File: src/../outside.txt
+new
*** End Patch"""
            },
            ToolContext(cwd="."),
        )
        assert normalized.updated_input is not None
        self.assertEqual(normalized.updated_input["affected_paths"], ["outside.txt"])
        self.assertEqual(manager.check(tool, normalized.updated_input).behavior, PermissionBehavior.ASK)

        escaped = await tool.check_permissions(
            {
                "patch": """*** Begin Patch
*** Add File: ../escaped.txt
+nope
*** End Patch"""
            },
            ToolContext(cwd="."),
        )
        self.assertEqual(escaped.behavior, PermissionBehavior.DENY)

    def test_default_registry_contains_apply_patch(self) -> None:
        self.assertIn("apply_patch", [tool.name for tool in get_default_tools()])

    def test_acp_maps_patch_to_edit_with_all_locations(self) -> None:
        patch_text = """*** Begin Patch
*** Update File: src/a.py
*** Move to: src/b.py
@@
-old
+new
*** End Patch"""
        self.assertEqual(to_tool_kind("apply_patch"), "edit")
        self.assertEqual(
            to_locations("apply_patch", {"patch": patch_text}),
            [{"path": "src/a.py"}, {"path": "src/b.py"}],
        )


class _ValidationAdapter:
    def __init__(self) -> None:
        self.config = ApiConfig(
            model="test",
            thinking_enabled=False,
            max_tokens=1000,
            max_retries=0,
        )
        self.requests = 0

    async def stream_message(self, messages, system, tools, config):
        self.requests += 1
        if self.requests == 1:
            yield StreamChunk(
                type="tool_use_start",
                tool_name="apply_patch",
                tool_use_id="patch-1",
            )
            yield StreamChunk(
                type="tool_use_end",
                tool_name="apply_patch",
                tool_use_id="patch-1",
                tool_input_json='{"patch":"not a patch"}',
            )
        else:
            yield StreamChunk(type="text", text="done")
        yield StreamChunk(type="message_stop")

    async def count_input_tokens(self, messages, system, tools, config):
        return None


class ApplyPatchQueryLoopTests(unittest.IsolatedAsyncioTestCase):
    async def test_malformed_patch_is_validation_error_before_permission(self) -> None:
        adapter = _ValidationAdapter()
        messages = [create_user_message("edit a file")]
        events = [
            event
            async for event in query_loop(
                QueryParams(
                    messages=messages,
                    system_prompt=[],
                    user_context={},
                    system_context={},
                    tools=[ApplyPatchTool()],
                    tool_context=ToolContext(messages=messages),
                    api_adapter=adapter,
                    api_config=adapter.config,
                    permission_manager=PermissionManager(),
                    permission_queue=asyncio.Queue(),
                    auto_compact_enabled=False,
                )
            )
        ]

        result = next(event for event in events if isinstance(event, ToolResultEvent))
        self.assertEqual(
            result.result,
            "Validation error: patch must start with '*** Begin Patch'",
        )
        self.assertFalse(any(isinstance(event, PermissionRequestEvent) for event in events))
