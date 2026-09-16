"""Exercise real prompt_toolkit input/rendering and history reconciliation."""
from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from prompt_toolkit.application.current import create_app_session
from prompt_toolkit.data_structures import Size
from prompt_toolkit.input import create_pipe_input
from prompt_toolkit.output import DummyOutput

from crabcode_cli import session_picker
from crabcode_core.session.meta_db import SessionMetaStore
from crabcode_core.session.storage import SessionStorage


class TerminalOutput(DummyOutput):
    size = Size(rows=24, columns=90)

    def get_size(self):
        return self.size


def row(sid, title, *, cwd="/project", updated=100, created=50):
    return dict(id=sid, title=title, cwd=cwd, updated_at=updated, created_at=created, model="model")


class PickerTests(unittest.IsolatedAsyncioTestCase):
    async def wait_for(self, predicate):
        async def poll():
            while not predicate():
                await asyncio.sleep(0.01)
        await asyncio.wait_for(poll(), 3)

    @asynccontextmanager
    async def picker(self, rows):
        async def load(*args):
            return rows
        with create_pipe_input() as pipe, create_app_session(input=pipe, output=TerminalOutput()), patch.object(session_picker, "run_io", side_effect=load):
            picker = session_picker.SessionPicker("/project", "current")
            task = asyncio.create_task(picker.run())
            try:
                await self.wait_for(lambda: picker.app.is_running and not picker.loading)
                yield picker, pipe, task
            finally:
                if not task.done():
                    task.cancel()
                await asyncio.gather(task, return_exceptions=True)

    async def test_search_chinese_and_resume_exact_selection(self):
        async with self.picker([row("current", "current"), row("a", "修复 中文 API [/provider]"), row("b", "other")]) as (picker, pipe, task):
            pipe.send_text("中文 API")
            await self.wait_for(lambda: [r["id"] for r in picker.matches] == ["a"])
            pipe.send_text("\r")
            self.assertEqual(await task, "a")

    async def test_project_sort_and_focus_controls(self):
        rows = [row("a", "old", updated=200, created=1), row("b", "new", updated=100, created=2), row("c", "remote", cwd="/other", updated=300, created=3)]
        async with self.picker(rows) as (picker, pipe, task):
            self.assertEqual([r["id"] for r in picker.matches], ["a", "b"])
            pipe.send_text("\t\x1b[C")
            await self.wait_for(lambda: picker.all_projects)
            self.assertEqual(picker.matches[0]["id"], "c")
            pipe.send_text("\x1b[D\t\x1b[C")
            await self.wait_for(lambda: picker.sort_key == "created_at")
            self.assertEqual([r["id"] for r in picker.matches], ["b", "a"])
            pipe.send_text("\tnew")
            await self.wait_for(lambda: picker.query.text == "new")
            pipe.send_text("\r")
            self.assertEqual(await task, "b")

    async def test_scrolling_and_terminal_resize(self):
        async with self.picker([row(f"{i:03}", f"History {i:03}", updated=i) for i in range(80)]) as (picker, pipe, task):
            pipe.send_text("\x1b[B" * 45)
            await self.wait_for(lambda: picker.selected == 45 and picker.list_window.vertical_scroll > 0)
            picker.app.output.size = Size(rows=14, columns=45)
            picker.app._on_resize()
            await self.wait_for(lambda: picker.list_window.render_info.window_height < 10)
            pipe.send_text("\r")
            self.assertEqual(await task, "034")

    async def test_empty_search_enter_does_not_exit_and_cancel_returns_none(self):
        for key in ("\x1b", "\x03", "\x04"):
            async with self.picker([row("a", "one")]) as (picker, pipe, task):
                pipe.send_text("missing\r")
                await self.wait_for(lambda: not picker.matches)
                self.assertFalse(task.done())
                pipe.send_text(key)
                self.assertIsNone(await asyncio.wait_for(task, 2))

    async def test_loading_failure_and_cancel_pending_load(self):
        for fail in (False, True):
            cancelled = asyncio.Event()
            async def load(*args):
                if fail:
                    raise OSError("disk [/error]")
                try:
                    await asyncio.Event().wait()
                finally:
                    cancelled.set()
            with create_pipe_input() as pipe, create_app_session(input=pipe, output=TerminalOutput()), patch.object(session_picker, "run_io", side_effect=load):
                picker = session_picker.SessionPicker("/project")
                task = asyncio.create_task(picker.run())
                await self.wait_for(lambda: bool(picker.error) if fail else picker.app.is_running)
                pipe.send_text("\x03")
                self.assertIsNone(await asyncio.wait_for(task, 2))
                if not fail:
                    self.assertTrue(cancelled.is_set())


class HistoryLoadingTests(unittest.TestCase):
    def test_real_storage_includes_older_rows_jsonl_fallback_and_skips_archived(self):
        with tempfile.TemporaryDirectory() as tmp, patch("crabcode_core.session.storage.get_config_home", return_value=Path(tmp)), patch("crabcode_core.session.meta_db.get_config_home", return_value=Path(tmp)):
            cwd = "/project"
            store = SessionMetaStore()
            try:
                for i in range(120):
                    store.upsert(row(f"sid-{i}", f"History {i}", cwd="/other"))
                    store.upsert(row(f"local-{i}", f"Local history {i}", cwd=cwd))
                store.upsert(row("local", "local", cwd=cwd))
                store.upsert(row("archived", "archived", cwd=cwd))
                store.archive("archived")
                # A transcript with no index record must still appear.
                storage = SessionStorage(cwd, "fallback")
                storage._transcript_path.parent.mkdir(parents=True, exist_ok=True)
                storage._transcript_path.write_text('{"type":"session_meta","cwd":"/project","title":"JSONL only","created_at":"2026-01-02T00:00:00Z"}\n')
                rows = session_picker._load_sessions(cwd)
                self.assertEqual(len(rows), 242)
                self.assertNotIn("archived", [r["id"] for r in rows])
                fallback = next(r for r in rows if r["id"] == "fallback")
                self.assertGreater(fallback["created_at"], 0)
                self.assertEqual(fallback["title"], "JSONL only")
            finally:
                store.close()
