"""Domain errors mapped to HTTP status codes by the app."""

from __future__ import annotations


class ControlPlaneError(Exception):
    status_code = 500
    code = "internal"

    def __init__(self, message: str, *, code: str | None = None) -> None:
        super().__init__(message)
        self.message = message
        if code is not None:
            self.code = code


class Unauthorized(ControlPlaneError):
    status_code = 401
    code = "unauthorized"


class NotFound(ControlPlaneError):
    status_code = 404
    code = "not_found"


class Conflict(ControlPlaneError):
    status_code = 409
    code = "conflict"


class InvalidTransition(Conflict):
    code = "invalid_transition"


class SecretRejected(ControlPlaneError):
    status_code = 400
    code = "secret_rejected"
