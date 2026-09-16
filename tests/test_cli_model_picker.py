"""Model picker input, filtering, rendering, and empty states."""
from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from types import SimpleNamespace
import unittest

from prompt_toolkit.application.current import create_app_session
from prompt_toolkit.data_structures import Size
from prompt_toolkit.input import create_pipe_input
from prompt_toolkit.output import DummyOutput

from crabcode_cli.model_picker import ModelPicker
from crabcode_core.types.config import ApiConfig, CrabCodeSettings


class TerminalOutput(DummyOutput):
    def get_size(self):
        return Size(rows=20, columns=90)


class ModelPickerTests(unittest.IsolatedAsyncioTestCase):
    async def wait_for(self, predicate):
        async def poll():
            while not predicate():
                await asyncio.sleep(0.01)
        await asyncio.wait_for(poll(), 3)

    @asynccontextmanager
    async def picker(self, models, current=None, default=None):
        session = SimpleNamespace(
            settings=CrabCodeSettings(models=models, default_model=default), _current_model_name=current,
            list_models=lambda: {name: f"{cfg.provider}/{cfg.model}" for name, cfg in models.items()},
        )
        with create_pipe_input() as pipe, create_app_session(input=pipe, output=TerminalOutput()):
            picker = ModelPicker(session)
            task = asyncio.create_task(picker.run())
            try:
                await self.wait_for(lambda: picker.app.is_running)
                yield picker, pipe, task
            finally:
                if not task.done():
                    task.cancel()
                await asyncio.gather(task, return_exceptions=True)

    async def test_current_model_selected_and_search_by_name_model_provider_group(self):
        models = {
            "first": ApiConfig(provider="openai", model="gpt-first", group="work"),
            "中文 [/literal]": ApiConfig(provider="anthropic", model="claude-second", group="思考"),
        }
        async with self.picker(models, "中文 [/literal]") as (picker, pipe, task):
            self.assertEqual(picker.matches[picker.selected]["name"], "中文 [/literal]")
            for query in ("中文", "ANTHROPIC", "claude-second", "思考"):
                pipe.send_text("\x15" + query)
                await self.wait_for(lambda: picker.query.text == query and len(picker.matches) == 1)
                self.assertEqual(picker.matches[0]["name"], "中文 [/literal]")
            pipe.send_text("\r")
            self.assertEqual(await task, "中文 [/literal]")

    async def test_default_profile_is_active_before_lazy_session_initialization(self):
        models = {name: ApiConfig(provider="openai", model=name) for name in ("first", "default-profile")}
        async with self.picker(models, default="default-profile") as (picker, pipe, task):
            self.assertEqual(picker.matches[picker.selected]["name"], "default-profile")
            pipe.send_text("\r")
            self.assertEqual(await task, "default-profile")

    async def test_group_headings_render_and_navigation_skips_them(self):
        models = {
            "one": ApiConfig(provider="openai", model="api-1", group="work"),
            "two": ApiConfig(provider="openai", model="api-2", group="personal"),
            "three": ApiConfig(provider="openai", model="api-3", group="work"),
        }
        async with self.picker(models, "one") as (picker, pipe, task):
            def screen_lines():
                screen = picker.app.renderer._last_screen
                return [] if screen is None else ["".join(cell.char for _, cell in sorted(row.items()))
                                                 for _, row in sorted(screen.data_buffer.items())]

            await self.wait_for(lambda: any("─ personal" in line for line in screen_lines()))
            lines = screen_lines()
            work = next(i for i, line in enumerate(lines) if "─ work" in line)
            personal = next(i for i, line in enumerate(lines) if "─ personal" in line)
            self.assertIn("one", lines[work + 1])
            self.assertIn("three", lines[work + 2])
            self.assertIn("two", lines[personal + 1])
            pipe.send_text("\x1b[B\x1b[B")
            await self.wait_for(lambda: picker.selected == 2)
            pipe.send_text("\r")
            self.assertEqual(await task, "two")

    async def test_group_filter_keyboard_and_scroll(self):
        models = {f"model-{i:02}": ApiConfig(provider="openai", model=f"api-{i}", group="work" if i < 40 else "other") for i in range(60)}
        async with self.picker(models, "model-00") as (picker, pipe, task):
            pipe.send_text("\t\x1b[C")
            await self.wait_for(lambda: len(picker.matches) == 40)
            pipe.send_text("\t\x1b[B" + "\x1b[6~" * 3)
            await self.wait_for(lambda: picker.selected > 10 and picker.list_window.vertical_scroll > 0)
            chosen = picker.matches[picker.selected]["name"]
            pipe.send_text("\r")
            self.assertEqual(await task, chosen)

    async def test_empty_configuration_and_no_matches_can_cancel(self):
        for models, query, cancel in (
            ({}, "", "\x1b"),
            ({"one": ApiConfig(provider="openai", model="gpt")}, "missing", "\x03"),
        ):
            async with self.picker(models) as (picker, pipe, task):
                pipe.send_text(query + "\r")
                await self.wait_for(lambda: not picker.matches)
                self.assertFalse(task.done())
                pipe.send_text(cancel)
                self.assertIsNone(await asyncio.wait_for(task, 2))
