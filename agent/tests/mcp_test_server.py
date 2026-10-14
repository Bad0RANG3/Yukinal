from mcp.server import MCPServer

server = MCPServer("yukinal-stdio-test")


@server.tool()
def ping() -> dict[str, str]:
    return {"state": "ready"}


if __name__ == "__main__":
    server.run(transport="stdio")
