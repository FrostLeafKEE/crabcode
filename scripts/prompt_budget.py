#!/usr/bin/env python3
"""Offline default-prompt benchmark; no credentials, project content or API calls.

Run from the repo with: python scripts/prompt_budget.py
Counts serialized schema text, not provider billing or hidden protocol overhead.
"""

import asyncio
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "packages/core"))

from crabcode_core.prompts.system import get_system_prompt
from crabcode_core.tools import get_default_tools
from crabcode_core.tools.agent import AgentTool
from crabcode_core.tools.loading import ToolCatalog, ToolLoadingState
from crabcode_core.types.config import ToolLoadingSettings
from crabcode_core.types.tool import ToolContext


async def measure():
    import tiktoken

    encoding = tiktoken.get_encoding("o200k_base")
    tools = [*get_default_tools(), AgentTool()]
    await asyncio.gather(*(tool.resolve_prompt() for tool in tools))
    results = {}
    for mode in ("eager", "discovery"):
        catalog = ToolCatalog(tools, ToolContext(), ToolLoadingSettings(mode=mode), ToolLoadingState())
        system = get_system_prompt(
            [tool.name for tool in tools], "gpt-5.6-sol", cwd="/workspace",
            platform="darwin", shell="zsh", os_version="Darwin", is_git=True,
        )
        directory = catalog.directory()
        count = lambda text: len(encoding.encode(text))
        system_tokens = count("\n\n".join(system))
        directory_tokens = count(directory)
        schema_tokens = count(json.dumps([tool.to_api_schema() for tool in catalog.loaded], ensure_ascii=False, separators=(",", ":")))
        results[mode] = {
            "system_tokens": system_tokens, "directory_tokens": directory_tokens,
            "tool_tokens": schema_tokens, "total_tokens": system_tokens + directory_tokens + schema_tokens,
            "loaded_tools": len(catalog.loaded), "available_tools": len(catalog.tools),
        }
    return results


if __name__ == "__main__":
    print(json.dumps(asyncio.run(measure()), ensure_ascii=False, indent=2))
