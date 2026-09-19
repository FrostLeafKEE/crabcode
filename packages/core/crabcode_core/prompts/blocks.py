"""System sections with cache metadata that never becomes model-visible text."""


class SystemPrompt(list[str]):
    def __init__(self, sections=(), *, static_count: int = 0):
        super().__init__(sections)
        self.static_count = min(max(static_count, 0), len(self))

    def with_dynamic(self, *sections: str) -> "SystemPrompt":
        return SystemPrompt([*self, *(s for s in sections if s)], static_count=self.static_count)
