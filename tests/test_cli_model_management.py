"""CLI model/group add-delete menus and persistent catalog mutations."""

from __future__ import annotations

import asyncio
import codecs
import json
from collections import deque
from contextlib import asynccontextmanager
from io import StringIO
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

from prompt_toolkit.application.current import create_app_session
from prompt_toolkit.data_structures import Size
from prompt_toolkit.input import create_pipe_input
from prompt_toolkit.output import DummyOutput
from rich.console import Console

from crabcode_cli import repl
from crabcode_cli.model_manager import (
    MenuOption,
    SearchableMenu,
    manage_model_catalog,
)
from crabcode_core.config.manager import ConfigManager
from crabcode_core.config.model_catalog import (
    CatalogMutationResult,
    ModelCatalogError,
    add_catalog_entry,
    delete_catalog_entry,
)
from crabcode_core.events import CoreSession
from crabcode_core.types.config import CrabCodeSettings


class TerminalOutput(DummyOutput):
    def get_size(self):
        return Size(rows=20, columns=100)


class ScriptedPrompter:
    def __init__(self, *, selects=(), texts=(), confirms=()):
        self.selects = deque(selects)
        self.texts = deque(texts)
        self.confirms = deque(confirms)
        self.select_calls = []
        self.notes = []
        self.errors = []

    async def select(self, message, options, **kwargs):
        self.select_calls.append((message, list(options), kwargs))
        return self.selects.popleft()

    async def text(self, message, *, initial=""):
        return self.texts.popleft()

    async def confirm(self, message, *, default=False):
        return self.confirms.popleft()

    def note(self, title, body):
        self.notes.append((title, body))

    def error(self, message):
        self.errors.append(message)


class SearchableMenuTests(unittest.IsolatedAsyncioTestCase):
    async def wait_for(self, predicate):
        async def poll():
            while not predicate():
                await asyncio.sleep(0.01)

        await asyncio.wait_for(poll(), 3)

    @asynccontextmanager
    async def menu(self, *, searchable=True):
        options = (
            MenuOption("model", "Model", "named profile"),
            MenuOption("group", "Group", "shared provider settings"),
        )
        with create_pipe_input() as pipe, create_app_session(
            input=pipe, output=TerminalOutput()
        ):
            menu = SearchableMenu(
                "Manage",
                "Choose an entry",
                options,
                searchable=searchable,
                initial="model",
            )
            task = asyncio.create_task(menu.run())
            try:
                await self.wait_for(lambda: menu.app.is_running)
                yield menu, pipe, task
            finally:
                if not task.done():
                    task.cancel()
                await asyncio.gather(task, return_exceptions=True)

    async def test_searches_labels_and_hints_then_selects(self):
        async with self.menu() as (menu, pipe, task):
            pipe.send_text("provider")
            await self.wait_for(lambda: len(menu.matches) == 1)
            self.assertEqual(menu.matches[0].value, "group")
            pipe.send_text("\r")
            self.assertEqual(await task, "group")

    async def test_non_search_menu_uses_arrow_navigation(self):
        async with self.menu(searchable=False) as (_menu, pipe, task):
            pipe.send_text("\x1b[B\r")
            self.assertEqual(await task, "group")


class ModelCatalogMutationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.cwd = self.root / "project"
        self.home.mkdir()
        self.cwd.mkdir()
        self.home_patch = patch("pathlib.Path.home", return_value=self.home)
        self.home_patch.start()

    def tearDown(self):
        self.home_patch.stop()
        self.temp.cleanup()

    def write_settings(self, source, value):
        path = Path(ConfigManager(cwd=str(self.cwd)).settings_file_paths[source])
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        return path

    def test_add_preserves_unrelated_settings_and_rejects_cross_layer_duplicate(self):
        project_path = self.write_settings(
            "projectSettings",
            {"language": "zh-CN", "models": {"existing": {"model": "old"}}},
        )
        self.write_settings("userSettings", {"groups": {"shared": {"provider": "openai"}}})

        result = add_catalog_entry(
            str(self.cwd),
            "projectSettings",
            "model",
            "new-model",
            {"group": "shared", "model": "gpt-new"},
        )

        self.assertEqual(result.path, project_path)
        saved = json.loads(project_path.read_text(encoding="utf-8"))
        self.assertEqual(saved["language"], "zh-CN")
        self.assertEqual(saved["models"]["existing"]["model"], "old")
        self.assertEqual(
            saved["models"]["new-model"],
            {"group": "shared", "model": "gpt-new"},
        )
        with self.assertRaisesRegex(ModelCatalogError, "already exists"):
            add_catalog_entry(
                str(self.cwd), "localSettings", "group", "shared", {"provider": "router"}
            )

    def test_delete_is_layer_exact_and_clears_same_layer_default(self):
        self.write_settings("userSettings", {"models": {"same": {"model": "user"}}})
        project_path = self.write_settings(
            "projectSettings",
            {
                "default_model": "same",
                "models": {"same": {"model": "project"}},
                "unrelated": {"keep": True},
            },
        )

        result = delete_catalog_entry(
            str(self.cwd), "projectSettings", "model", "same"
        )

        self.assertTrue(result.cleared_default)
        saved = json.loads(project_path.read_text(encoding="utf-8"))
        self.assertNotIn("models", saved)
        self.assertIsNone(saved["default_model"])
        self.assertEqual(saved["unrelated"], {"keep": True})
        self.assertEqual(ConfigManager(cwd=str(self.cwd)).load().models["same"].model, "user")

    def test_atomic_write_preserves_bom_crlf_and_file_mode(self):
        path = Path(ConfigManager(cwd=str(self.cwd)).settings_file_paths["projectSettings"])
        path.parent.mkdir(parents=True)
        path.write_bytes(codecs.BOM_UTF8 + b'{\r\n  "language": "zh-CN"\r\n}\r\n')
        path.chmod(0o640)

        add_catalog_entry(
            str(self.cwd), "projectSettings", "group", "router", {"provider": "router"}
        )

        raw = path.read_bytes()
        self.assertTrue(raw.startswith(codecs.BOM_UTF8))
        self.assertIn(b"\r\n", raw)
        self.assertNotIn(b"\n", raw.replace(b"\r\n", b""))
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o640)


class ModelManagementWizardTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.cwd = self.root / "project"
        self.home.mkdir()
        self.cwd.mkdir()
        self.home_patch = patch("pathlib.Path.home", return_value=self.home)
        self.home_patch.start()

    def tearDown(self):
        self.home_patch.stop()
        self.temp.cleanup()

    def write_project(self, value):
        path = self.cwd / ".crabcode" / "settings.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
        return path

    def make_session(self):
        settings = ConfigManager(cwd=str(self.cwd)).load()
        settings._crabcode_explicit_settings = CrabCodeSettings()
        return CoreSession(cwd=str(self.cwd), settings=settings)

    async def test_no_arg_add_chooses_group_and_builds_router_config(self):
        session = self.make_session()
        prompter = ScriptedPrompter(
            selects=("group", "projectSettings", "router", "openai"),
            texts=("my-router", "https://router.example/v1", "ROUTER_KEY", ""),
            confirms=(True,),
        )

        result = await manage_model_catalog(session, "add", prompter=prompter)

        self.assertIsNotNone(result)
        self.assertEqual(result.kind, "group")
        saved = json.loads((self.cwd / ".crabcode" / "settings.json").read_text())
        self.assertEqual(
            saved["groups"]["my-router"],
            {
                "provider": "router",
                "base_url": "https://router.example/v1",
                "api_key_env": "ROUTER_KEY",
                "format": "openai",
            },
        )
        self.assertIn("my-router", session.settings.groups)

    async def test_add_model_inherits_group_and_accepts_advanced_json(self):
        self.write_project({"groups": {"shared": {"provider": "openai"}}})
        session = self.make_session()
        prompter = ScriptedPrompter(
            selects=("projectSettings", "shared", "__inherit__"),
            texts=(
                "fast",
                "gpt-fast",
                '{"reasoning_effort":"low","http_headers":{"Authorization":"Bearer test"}}',
            ),
            confirms=(True,),
        )

        result = await manage_model_catalog(
            session, "add", "model", prompter=prompter
        )

        self.assertEqual(result.name, "fast")
        raw = ConfigManager(cwd=str(self.cwd)).get_settings_for_source("projectSettings")
        self.assertEqual(
            raw["models"]["fast"],
            {
                "model": "gpt-fast",
                "reasoning_effort": "low",
                "http_headers": {"Authorization": "Bearer test"},
                "group": "shared",
            },
        )
        self.assertIn("[redacted]", prompter.notes[-1][1])
        self.assertNotIn("Bearer test", prompter.notes[-1][1])
        self.assertEqual(session.settings.get_api_config("fast").provider, "openai")
        self.assertIn("fast", session.list_models())

    async def test_delete_group_warns_about_references_and_removes_exact_definition(self):
        self.write_project(
            {
                "groups": {"shared": {"provider": "openai"}},
                "models": {"fast": {"group": "shared", "model": "gpt-fast"}},
            }
        )
        session = self.make_session()
        prompter = ScriptedPrompter(selects=("0",), confirms=(True,))

        result = await manage_model_catalog(
            session, "delete", "group", prompter=prompter
        )

        self.assertEqual(result.name, "shared")
        self.assertIn("fast", prompter.notes[-1][1])
        raw = ConfigManager(cwd=str(self.cwd)).get_settings_for_source("projectSettings")
        self.assertNotIn("groups", raw)
        self.assertEqual(raw["models"]["fast"]["group"], "shared")

    async def test_active_model_and_its_group_are_protected(self):
        self.write_project(
            {
                "groups": {"shared": {"provider": "openai"}},
                "models": {"fast": {"group": "shared", "model": "gpt-fast"}},
            }
        )
        session = self.make_session()
        session._current_model_name = "fast"

        with self.assertRaisesRegex(ModelCatalogError, "active model"):
            await manage_model_catalog(
                session,
                "delete",
                "model",
                prompter=ScriptedPrompter(selects=("0",)),
            )
        with self.assertRaisesRegex(ModelCatalogError, "while active model"):
            await manage_model_catalog(
                session,
                "delete",
                "group",
                prompter=ScriptedPrompter(selects=("0",)),
            )


class ModelManagementCommandTests(unittest.IsolatedAsyncioTestCase):
    async def test_add_and_del_dispatch_kind_to_guided_manager(self):
        output = StringIO()
        session = SimpleNamespace(skills=[])
        added = CatalogMutationResult(
            "add",
            "model",
            "fast [/literal]",
            "projectSettings",
            Path("/tmp/settings.json"),
        )
        deleted = CatalogMutationResult(
            "delete",
            "group",
            "shared",
            "localSettings",
            Path("/tmp/settings.local.json"),
        )
        manager = AsyncMock(side_effect=(added, deleted))
        with (
            patch("crabcode_cli.model_manager.manage_model_catalog", manager),
            patch.object(repl, "console", Console(file=output, width=160)),
        ):
            self.assertTrue(await repl._handle_command("/add model", session, None, []))
            self.assertTrue(await repl._handle_command("/del group", session, None, []))

        self.assertEqual(manager.await_args_list[0].args[1:], ("add", "model"))
        self.assertEqual(manager.await_args_list[1].args[1:], ("delete", "group"))
        self.assertIn("fast [/literal]", output.getvalue())
        self.assertIn("shared", output.getvalue())
        self.assertIn("/add", repl._SLASH_COMMANDS)
        self.assertEqual(repl._SLASH_COMMANDS["/del"], ["model", "group"])

    async def test_invalid_subcommand_does_not_open_manager(self):
        manager = AsyncMock()
        with patch("crabcode_cli.model_manager.manage_model_catalog", manager):
            self.assertTrue(
                await repl._handle_command(
                    "/add model extra", SimpleNamespace(skills=[]), None, []
                )
            )
        manager.assert_not_awaited()
