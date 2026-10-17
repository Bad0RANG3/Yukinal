"""Entry point: ``python -m yukinal_control_plane`` serves the ASGI app."""

from __future__ import annotations

import uvicorn

from .app import create_app
from .config import Settings


def main() -> None:
    settings = Settings()
    application = create_app(settings)
    uvicorn.run(application, host=settings.bind_host, port=settings.port)


if __name__ == "__main__":
    main()
