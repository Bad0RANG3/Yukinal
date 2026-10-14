from __future__ import annotations

import sys
from importlib import import_module

from yukinal_agent.sidecar import main


def run() -> None:
    if len(sys.argv) == 2 and sys.argv[1] == "--version":
        print("0.1.0")
        return
    if len(sys.argv) == 2 and sys.argv[1] == "--check-imports":
        for module in (
            "jsonschema",
            "langchain_openai",
            "langchain_anthropic",
            "langchain_google_genai",
            "langgraph.graph",
            "langgraph.checkpoint.sqlite.aio",
            "aiosqlite",
            "sqlite_vec",
            "mcp.client.stdio",
            "mcp.client.streamable_http",
        ):
            import_module(module)
        print("packaged imports ok")
        return
    main()


if __name__ == "__main__":
    run()
