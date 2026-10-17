"""Control-plane settings, read from the environment only.

Secrets never live in code or JSON: the shared bearer token is read from the
environment, and the SQLAlchemy URL may embed a password but is only ever read
from the environment. Neither is ever logged or serialized.
"""

from __future__ import annotations

from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    """Environment-driven settings (prefix ``YUKINAL_CONTROL_``).

    ``DATABASE_URL`` defaults to a local SQLite file so tests and development do
    not need a PostgreSQL server; production is expected to set
    ``YUKINAL_CONTROL_DATABASE_URL`` to ``postgresql+asyncpg://...``.
    """

    model_config = SettingsConfigDict(env_prefix="YUKINAL_CONTROL_", extra="ignore")

    database_url: str = "sqlite+aiosqlite:///./yukinal-control.db"
    # Bind to loopback by default. A deployment that must accept node traffic
    # from a network sets YUKINAL_CONTROL_BIND_HOST explicitly and terminates
    # TLS in front of the process.
    bind_host: str = "127.0.0.1"
    port: int = 8765
    # Shared bearer token for the node/operator API. Absent by default: the app
    # refuses to start unless a token is set or anonymous access is explicit.
    token: str | None = None
    # Explicit, local-only opt-out of the token requirement. Never enable this in
    # a deployment reachable from a network.
    allow_anonymous: bool = False

    def require_token(self) -> str | None:
        """Return the configured token, or ``None`` when anonymous access is on.

        Raises ``RuntimeError`` when neither is configured, so ``create_app``
        fails closed instead of serving an unauthenticated API.
        """
        if self.token:
            return self.token
        if self.allow_anonymous:
            return None
        raise RuntimeError(
            "YUKINAL_CONTROL_TOKEN is not set and YUKINAL_CONTROL_ALLOW_ANONYMOUS is off; "
            "refusing to start an unauthenticated control plane"
        )
