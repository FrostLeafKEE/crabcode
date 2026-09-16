"""Keyboard-driven history browser using the CLI's terminal-native palette."""

from __future__ import annotations

import asyncio
import os
import time
from datetime import datetime, timezone
from typing import Any

from prompt_toolkit.application import Application
from prompt_toolkit.buffer import Buffer
from prompt_toolkit.data_structures import Point
from prompt_toolkit.filters import Condition
from prompt_toolkit.key_binding import KeyBindings
from prompt_toolkit.layout import HSplit, Layout, VSplit, Window
from prompt_toolkit.layout.controls import BufferControl, FormattedTextControl
from prompt_toolkit.styles import Style

from crabcode_core.io_worker import run_io

PICKER_STYLE = Style.from_dict({
    "accent": "ansicyan bold", "selected": "ansicyan bold",
    "hint": "ansibrightblack", "focused": "ansicyan bold underline",
    "error": "ansired",
})


def _timestamp(value: Any) -> float:
    try:
        return float(value or 0)
    except (ValueError, TypeError):
        try:
            parsed = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
            return parsed.replace(tzinfo=parsed.tzinfo or timezone.utc).timestamp()
        except (ValueError, TypeError, OverflowError):
            return 0


def _plain(value: Any) -> str:
    # Transcript text is literal, single-line terminal content, never markup.
    return " ".join("".join(c for c in str(value or "") if c.isprintable() or c.isspace()).split())


def _age(timestamp: float) -> str:
    if not timestamp:
        return "unknown"
    seconds = max(0, int(time.time() - timestamp))
    for unit, size in (("d", 86400), ("h", 3600), ("m", 60)):
        if seconds >= size:
            return f"{seconds // size}{unit} ago"
    return "just now"


def _load_sessions(cwd: str) -> list[dict[str, Any]]:
    """Reconcile local JSONL history and include the global metadata index."""
    from crabcode_core.session.meta_db import SessionMetaStore
    from crabcode_core.session.storage import SessionStorage

    local = SessionStorage.list_sessions(cwd)
    store = SessionMetaStore()
    try:
        # SQLite's -1 means no limit: older history must remain searchable.
        indexed = store.list_recent(limit=-1)
    finally:
        store.close()
    # Local storage also checks durable archive markers; do not reintroduce
    # rows excluded by that reconciliation from the metadata index.
    rows = {r["id"]: r for r in indexed if os.path.normcase(r["cwd"]) != os.path.normcase(cwd)}
    by_id = {r["id"]: r for r in indexed}
    for row in local:
        sid = row["session_id"]
        rows[sid] = {
            **by_id.get(sid, {}), **row, "id": sid, "cwd": cwd,
            "updated_at": _timestamp(row.get("modified")),
            "created_at": _timestamp(row.get("created_at")),
        }
    return list(rows.values())


class SessionPicker:
    def __init__(self, cwd: str, current_session_id: str = "") -> None:
        self.cwd = os.path.abspath(cwd)
        self.current_session_id = current_session_id
        self.rows: list[dict[str, Any]] = []
        self.matches: list[dict[str, Any]] = []
        self.selected = 0
        self.all_projects = False
        self.sort_key = "updated_at"
        self.loading = True
        self.error = ""
        self.query = Buffer(on_text_changed=lambda _: self.refresh())
        self.search = BufferControl(buffer=self.query)
        self.options = FormattedTextControl(self._options, focusable=True)
        self.list_control = FormattedTextControl(
            self._list, get_cursor_position=lambda: Point(x=0, y=self.selected),
        )
        self.list_window = Window(
            self.list_control, wrap_lines=False, always_hide_cursor=True,
        )
        self.option_index = 0
        bindings = KeyBindings()

        @bindings.add("escape", eager=True)
        @bindings.add("c-c", eager=True)
        @bindings.add("c-d", eager=True)
        def cancel(event: Any) -> None:
            event.app.exit(result=None)

        @bindings.add("enter", eager=True)
        def confirm(event: Any) -> None:
            if self.matches:
                event.app.exit(result=self.matches[self.selected]["id"])

        @bindings.add("up")
        @bindings.add("down")
        @bindings.add("pageup")
        @bindings.add("pagedown")
        def navigate(event: Any) -> None:
            key = event.key_sequence[0].key
            page = max(1, self.list_window.render_info.window_height - 1) if self.list_window.render_info else 10
            delta = {"up": -1, "down": 1, "pageup": -page, "pagedown": page}[key]
            self.selected = max(0, min(len(self.matches) - 1, self.selected + delta))

        @bindings.add("tab")
        @bindings.add("s-tab")
        def focus(event: Any) -> None:
            current = self.option_index + 1 if event.app.layout.has_focus(self.options) else 0
            current = (current + (-1 if event.key_sequence[0].key == "s-tab" else 1)) % 3
            if current == 0:
                event.app.layout.focus(self.search)
            else:
                self.option_index = current - 1
                event.app.layout.focus(self.options)

        @bindings.add("left", filter=Condition(lambda: self.app.layout.has_focus(self.options)))
        @bindings.add("right", filter=Condition(lambda: self.app.layout.has_focus(self.options)))
        @bindings.add("space", filter=Condition(lambda: self.app.layout.has_focus(self.options)))
        def change_option(event: Any) -> None:
            if self.option_index == 0:
                self.all_projects = not self.all_projects
            else:
                self.sort_key = "created_at" if self.sort_key == "updated_at" else "updated_at"
            self.refresh()

        self.app: Application[str | None] = Application(
            layout=Layout(HSplit([
                Window(FormattedTextControl([("class:accent", "  ╭─ CrabCode · Resume session")]), height=1),
                Window(height=1),
                VSplit([
                    Window(FormattedTextControl([("class:accent", "  Search ❯ ")]), width=11),
                    Window(self.search, height=1, wrap_lines=False),
                ], height=1),
                Window(self.options, height=1),
                Window(height=1),
                self.list_window,
                Window(FormattedTextControl(self._details), height=2, wrap_lines=False),
                Window(char="─", height=1, style="class:hint"),
                Window(FormattedTextControl(self._footer), height=2, wrap_lines=True),
            ]), focused_element=self.search),
            key_bindings=bindings, full_screen=True, erase_when_done=True,
            style=PICKER_STYLE,
        )

    def refresh(self) -> None:
        terms = self.query.text.casefold().split()
        self.matches = [r for r in self.rows if r["id"] != self.current_session_id
                        and (self.all_projects or os.path.normcase(r["cwd"]) == os.path.normcase(self.cwd))
                        and all(term in " ".join(str(r.get(k, "")) for k in
                            ("title", "first_user_message", "preview", "id", "cwd")).casefold() for term in terms)]
        self.matches.sort(key=lambda r: (_timestamp(r.get(self.sort_key)), r["id"]), reverse=True)
        self.selected = 0
        self.app.invalidate()

    def _options(self) -> list[tuple[str, str]]:
        focused = self.app.layout.has_focus(self.options)
        return [
            ("class:focused" if focused and self.option_index == 0 else "class:hint",
             f"  Project: {'All' if self.all_projects else 'Cwd'}"),
            ("class:focused" if focused and self.option_index == 1 else "class:hint",
             f"    Sort: {'Updated' if self.sort_key == 'updated_at' else 'Created'}"),
        ]

    def _list(self) -> list[tuple[str, str]]:
        if self.loading:
            return [("class:hint", "  Loading history…")]
        if self.error:
            return [("class:error", f"  Could not load history: {_plain(self.error)}")]
        if not self.matches:
            return [("class:hint", "  No matching sessions. Clear search or switch Project to All.")]
        fragments = []
        for i, row in enumerate(self.matches):
            style = "class:selected" if i == self.selected else ""
            title = _plain(row.get("title") or row.get("first_user_message") or row.get("preview") or "Untitled session")
            fragments.extend([
                (style, "  ❯ " if i == self.selected else "    "),
                (style if i == self.selected else "class:hint", f"{_age(_timestamp(row.get(self.sort_key))):>9}  "),
                (style, title + ("\n" if i < len(self.matches) - 1 else "")),
            ])
        return fragments

    def _details(self) -> list[tuple[str, str]]:
        if not self.matches:
            return []
        row = self.matches[self.selected]
        return [("class:hint", f"  {_plain(row.get('title') or row.get('preview') or row.get('first_user_message'))}\n"
                 f"  {_plain(row['cwd'])} · {row['id'][:8]} · {_plain(row.get('model'))}")]

    def _footer(self) -> list[tuple[str, str]]:
        count = len(self.matches)
        return [("class:hint", f"  ↑↓ browse · PgUp/PgDn page · Enter resume · Esc/Ctrl+C cancel    {self.selected + 1 if count else 0}/{count}\n"
                 "  Type to search · Tab focus project/sort · ←→ change option")]

    async def run(self) -> str | None:
        async def load() -> None:
            try:
                self.rows = await run_io(__name__, "_load_sessions", self.cwd)
            except Exception as exc:
                self.error = str(exc)
            finally:
                self.loading = False
                self.refresh()

        task: asyncio.Task[None] | None = None

        def start() -> None:
            nonlocal task
            task = asyncio.create_task(load())

        try:
            return await self.app.run_async(pre_run=start)
        finally:
            if task is not None:
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)


async def select_session(cwd: str, current_session_id: str = "") -> str | None:
    return await SessionPicker(cwd, current_session_id).run()
