"""MCP server — expose KoiosBase as tools (§11)."""

from .server import (  # noqa: F401
    HANDLERS,
    PROTOCOL_VERSION,
    TOOLS,
    handle_request,
    serve,
    tool_ask,
    tool_search,
    tool_write_answer,
)
