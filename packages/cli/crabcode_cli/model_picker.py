"""Full-screen selection of configured model profiles."""

from __future__ import annotations

from typing import Any

from prompt_toolkit.application import Application
from prompt_toolkit.buffer import Buffer
from prompt_toolkit.data_structures import Point
from prompt_toolkit.filters import Condition
from prompt_toolkit.key_binding import KeyBindings
from prompt_toolkit.layout import HSplit, Layout, VSplit, Window
from prompt_toolkit.layout.controls import BufferControl, FormattedTextControl

from crabcode_cli.session_picker import PICKER_STYLE, _plain
from crabcode_core.events import CoreSession


class ModelPicker:
    def __init__(self, session: CoreSession) -> None:
        # The REPL initializes its session lazily. Before the first request,
        # the configured default is already the effective model profile.
        self.current_name = getattr(session, "_current_model_name", None) or session.settings.default_model
        active = session.settings.get_api_config(self.current_name)
        self.current_label = (
            f"{self.current_name or 'default'} · {active.provider or 'anthropic'}/"
            f"{active.model or 'default'} · {active.group or 'default'}"
        )
        grouped: dict[str, list[dict[str, str]]] = {}
        for name, description in session.list_models().items():
            config = session.settings.get_api_config(name)
            group = config.group or "default"
            grouped.setdefault(group, []).append({
                "name": name, "description": description, "group": group,
                "provider": config.provider or "anthropic", "model": config.model or "default",
            })
        self.rows = [row for rows in grouped.values() for row in rows]
        self.groups = list(grouped)
        self.group_index = 0  # Zero means all groups; actual names may also be "All".
        self.matches = self.rows.copy()
        self.selected = next((i for i, row in enumerate(self.matches)
                              if row["name"] == self.current_name), 0)
        self.query = Buffer(on_text_changed=lambda _: self.refresh())
        self.search = BufferControl(buffer=self.query)
        self.group_control = FormattedTextControl(self._group, focusable=True)
        self.list_window = Window(
            FormattedTextControl(self._list, get_cursor_position=lambda: Point(0, self._selected_line())),
            wrap_lines=False, always_hide_cursor=True,
        )
        bindings = KeyBindings()

        @bindings.add("escape", eager=True)
        @bindings.add("c-c", eager=True)
        @bindings.add("c-d", eager=True)
        def cancel(event: Any) -> None:
            event.app.exit(result=None)

        @bindings.add("enter", eager=True)
        def confirm(event: Any) -> None:
            if self.matches:
                event.app.exit(result=self.matches[self.selected]["name"])

        @bindings.add("up")
        @bindings.add("down")
        @bindings.add("pageup")
        @bindings.add("pagedown")
        def navigate(event: Any) -> None:
            page = max(1, self.list_window.render_info.window_height - 1) if self.list_window.render_info else 10
            delta = {"up": -1, "down": 1, "pageup": -page, "pagedown": page}[event.key_sequence[0].key]
            self.selected = max(0, min(len(self.matches) - 1, self.selected + delta))

        @bindings.add("tab")
        @bindings.add("s-tab")
        def focus(event: Any) -> None:
            event.app.layout.focus(self.search if event.app.layout.has_focus(self.group_control) else self.group_control)

        @bindings.add("left", filter=Condition(lambda: self.app.layout.has_focus(self.group_control)))
        @bindings.add("right", filter=Condition(lambda: self.app.layout.has_focus(self.group_control)))
        @bindings.add("space", filter=Condition(lambda: self.app.layout.has_focus(self.group_control)))
        def change_group(event: Any) -> None:
            delta = -1 if event.key_sequence[0].key == "left" else 1
            self.group_index = (self.group_index + delta) % (len(self.groups) + 1)
            self.refresh()

        self.app: Application[str | None] = Application(
            layout=Layout(HSplit([
                Window(FormattedTextControl([("class:accent", "  ╭─ CrabCode · Select model")]), height=1),
                Window(FormattedTextControl([("class:hint", f"  Current: {_plain(self.current_label)}")]), height=1, wrap_lines=False),
                Window(height=1),
                VSplit([
                    Window(FormattedTextControl([("class:accent", "  Search ❯ ")]), width=11),
                    Window(self.search, height=1, wrap_lines=False),
                ], height=1),
                Window(self.group_control, height=1),
                Window(height=1),
                self.list_window,
                Window(FormattedTextControl(self._details), height=2, wrap_lines=False),
                Window(char="─", height=1, style="class:hint"),
                Window(FormattedTextControl(self._footer), height=2, wrap_lines=True),
            ]), focused_element=self.search),
            key_bindings=bindings, full_screen=True, erase_when_done=True, style=PICKER_STYLE,
        )

    def refresh(self) -> None:
        terms = self.query.text.casefold().split()
        group = self.groups[self.group_index - 1] if self.group_index else None
        self.matches = [row for row in self.rows
                        if (group is None or row["group"] == group)
                        and all(term in " ".join(row.values()).casefold() for term in terms)]
        self.selected = next((i for i, row in enumerate(self.matches)
                              if row["name"] == self.current_name), 0)
        self.app.invalidate()

    def _group(self) -> list[tuple[str, str]]:
        focused = self.app.layout.has_focus(self.group_control)
        group = self.groups[self.group_index - 1] if self.group_index else "All groups"
        return [("class:focused" if focused else "class:hint", f"  Group: {_plain(group)}")]

    def _list(self) -> list[tuple[str, str]]:
        if not self.rows:
            return [("class:hint", "  No named models configured. Add models to settings.json.")]
        if not self.matches:
            return [("class:hint", "  No matching models. Clear search or select All groups.")]
        fragments = []
        previous_group = None
        for i, row in enumerate(self.matches):
            if row["group"] != previous_group:
                fragments.append(("class:accent", f"  ─ {_plain(row['group'])}\n"))
                previous_group = row["group"]
            style = "class:selected" if i == self.selected else ""
            marker = "  ← active" if row["name"] == self.current_name else ""
            fragments.extend([
                (style, "    ❯ " if i == self.selected else "      "),
                (style, _plain(row["name"]) + marker),
                (style if i == self.selected else "class:hint",
                 f"  · {_plain(row['provider'])}/{_plain(row['model'])}"),
                ("", "\n" if i < len(self.matches) - 1 else ""),
            ])
        return fragments

    def _selected_line(self) -> int:
        # Group headings occupy display lines but are never selectable.
        headings = 0
        previous_group = None
        for row in self.matches[:self.selected + 1]:
            if row["group"] != previous_group:
                headings += 1
                previous_group = row["group"]
        return self.selected + headings

    def _details(self) -> list[tuple[str, str]]:
        if not self.matches:
            return []
        row = self.matches[self.selected]
        return [("class:hint", f"  {_plain(row['name'])} · {_plain(row['group'])}\n"
                 f"  {_plain(row['description'])}")]

    def _footer(self) -> list[tuple[str, str]]:
        count = len(self.matches)
        return [("class:hint", f"  ↑↓ browse · PgUp/PgDn page · Enter switch · Esc/Ctrl+C cancel    {self.selected + 1 if count else 0}/{count}\n"
                 "  Type to search · Tab focus group · ←→ change group")]

    async def run(self) -> str | None:
        return await self.app.run_async()


async def select_model(session: CoreSession) -> str | None:
    return await ModelPicker(session).run()
