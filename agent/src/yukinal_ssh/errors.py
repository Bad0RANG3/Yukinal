from __future__ import annotations


class SshServiceError(RuntimeError):
    """A structured error returned by, or while contacting, the local SSH service."""

    def __init__(self, code: str, message: str, *, http_status: int | None = None) -> None:
        super().__init__(message)
        self.code = code
        self.http_status = http_status


class SshServiceTransportError(SshServiceError):
    """The local service could not be reached or returned an invalid response."""
