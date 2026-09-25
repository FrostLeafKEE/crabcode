"""Discover and activate tool schemas without executing the discovered tools."""

from crabcode_core.types.tool import PermissionBehavior, PermissionResult, Tool, ToolContext, ToolResult


class ToolSearchTool(Tool):
    name = "ToolSearch"
    description = (
        "Find and load tools by exact names, group, or keyword query. Their schemas "
        "become available in your NEXT response, never in the same batch. "
        "Use list=true to browse without loading; paginate with offset. "
        "Loading does not execute tools or grant permissions."
    )
    is_read_only = True
    input_schema = {
        "type": "object",
        "properties": {
            "names": {
                "type": "array", "items": {"type": "string"},
                "description": (
                    "Exact registered tool names from the directory's names field; "
                    "do not add a group prefix."
                ),
            },
            "group": {
                "type": "string",
                "description": "Tool category to search or load. This is not a tool-name namespace.",
            },
            "query": {"type": "string"},
            "list": {"type": "boolean", "default": False},
            "offset": {"type": "integer", "minimum": 0, "default": 0},
            "limit": {"type": "integer", "minimum": 1, "maximum": 30, "default": 5},
        },
        "additionalProperties": False,
    }

    async def check_permissions(self, tool_input, context):
        return PermissionResult(behavior=PermissionBehavior.ALLOW)

    async def validate_input(self, tool_input):
        names = tool_input.get("names", [])
        if not isinstance(names, list) or any(not isinstance(name, str) for name in names):
            return "names must be an array of tool names"
        if any(not isinstance(tool_input.get(key, ""), str) for key in ("query", "group")):
            return "query and group must be strings"
        if not isinstance(tool_input.get("list", False), bool):
            return "list must be a boolean"
        offset, limit = tool_input.get("offset", 0), tool_input.get("limit", 5)
        if type(offset) is not int or offset < 0:
            return "offset must be a nonnegative integer"
        if type(limit) is not int or not 1 <= limit <= 30:
            return "limit must be an integer between 1 and 30"
        return None

    async def call(self, tool_input: dict, context: ToolContext) -> ToolResult:
        if context.tool_catalog is None:
            return ToolResult(result_for_model="Tool discovery is unavailable in this context.", is_error=True)
        result = context.tool_catalog.search(
            names=tool_input.get("names", []), group=tool_input.get("group", ""),
            query=tool_input.get("query", ""),
            list_only=tool_input.get("list", False) or not any(tool_input.get(k) for k in ("names", "group", "query")),
            offset=tool_input.get("offset", 0), limit=tool_input.get("limit", 5),
        )
        return ToolResult(result_for_model=result)
