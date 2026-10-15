from __future__ import annotations

import ipaddress
import json
import re
import sqlite3
from contextlib import closing
from datetime import UTC, date, datetime
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit


class ProviderStoreError(RuntimeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


_KINDS = {"openai-compatible", "anthropic", "gemini"}
_SAFE_HEADERS = {
    "http-referer", "referer", "origin", "user-agent", "x-app-name",
    "x-app-version", "x-client-name", "x-client-version", "x-title",
    "anthropic-beta", "openai-organization", "openai-project", "x-goog-user-project",
}
_PROVIDER_ID = re.compile(r"^[a-z0-9][a-z0-9_-]{0,255}$")


def validate_provider_save(value: object) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ProviderStoreError("invalid_params", "Provider configuration must be an object")
    allowed = {
        "providerId", "kind", "label", "baseUrl", "model", "apiKey",
        "customHeaders", "apiVersion", "wireApi", "models",
    }
    if set(value) - allowed:
        raise ProviderStoreError("invalid_params", "Provider configuration contains unsupported fields")
    kind = value.get("kind")
    if not isinstance(kind, str) or kind not in _KINDS:
        raise ProviderStoreError("invalid_params", "Provider kind is not supported")
    provider_id = value.get("providerId")
    if provider_id is not None and not isinstance(provider_id, str):
        raise ProviderStoreError("invalid_params", "Provider ID must use lowercase letters, numbers, hyphens, or underscores")
    provider_id = provider_id.strip() if isinstance(provider_id, str) else None
    if provider_id == "":
        provider_id = None
    if provider_id is not None and not _PROVIDER_ID.fullmatch(provider_id):
        raise ProviderStoreError("invalid_params", "Provider ID must use lowercase letters, numbers, hyphens, or underscores")
    base_url = _validate_base_url(value.get("baseUrl"))
    model = value.get("model")
    if not isinstance(model, str) or not model.strip() or len(model.strip()) > 256 or "\n" in model or "\r" in model:
        raise ProviderStoreError("invalid_params", "Provider model is invalid")
    label = value.get("label")
    if label is not None and (not isinstance(label, str) or len(label) > 256):
        raise ProviderStoreError("invalid_params", "Provider label is invalid")
    api_key = value.get("apiKey")
    if api_key is not None and (
        not isinstance(api_key, str) or not 1 <= len(api_key) <= 4096
        or "\r" in api_key or "\n" in api_key
    ):
        raise ProviderStoreError("invalid_params", "Provider API key is invalid")
    if isinstance(api_key, str) and not api_key.strip():
        api_key = None
    wire_api = value.get("wireApi")
    if isinstance(wire_api, str):
        wire_api = wire_api.strip() or None
    if wire_api is not None and (
        kind != "openai-compatible" or not isinstance(wire_api, str) or wire_api not in {"chat", "responses"}
    ):
        raise ProviderStoreError("invalid_params", "wireApi is supported only by openai-compatible providers")
    api_version = value.get("apiVersion")
    if isinstance(api_version, str):
        api_version = api_version.strip() or None
    if api_version is not None:
        if kind != "anthropic" or not isinstance(api_version, str):
            raise ProviderStoreError("invalid_params", "apiVersion is supported only by Anthropic providers")
        try:
            parsed_date = date.fromisoformat(api_version)
        except ValueError:
            raise ProviderStoreError("invalid_params", "apiVersion must be a valid YYYY-MM-DD date") from None
        if parsed_date.year < 1970 or len(api_version) != 10:
            raise ProviderStoreError("invalid_params", "apiVersion must be a valid YYYY-MM-DD date")
    headers = _validate_headers(value.get("customHeaders"))
    models = _validate_models(value.get("models"))
    return {
        "providerId": provider_id,
        "kind": kind,
        "label": (label.strip() or base_url) if isinstance(label, str) else base_url,
        "baseUrl": base_url,
        "model": model.strip(),
        "apiKey": api_key,
        "customHeaders": headers,
        "apiVersion": api_version,
        "wireApi": wire_api,
        "models": models,
    }


def _validate_base_url(value: object) -> str:
    if not isinstance(value, str) or not value.strip() or len(value.strip()) > 2048:
        raise ProviderStoreError("invalid_params", "baseUrl is invalid")
    value = value.strip()
    try:
        parsed = urlsplit(value)
        host = parsed.hostname
        port = parsed.port
    except ValueError:
        raise ProviderStoreError("invalid_params", "baseUrl is not a valid URL") from None
    if (
        parsed.scheme not in {"http", "https"}
        or not host
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or port is not None and not 1 <= port <= 65535
    ):
        raise ProviderStoreError("invalid_params", "baseUrl must be an http(s) endpoint without credentials, query, or fragment")
    if parsed.scheme == "http" and not _is_loopback_host(host):
        raise ProviderStoreError("invalid_params", "plain HTTP is allowed only for loopback providers")
    return value.rstrip("/")


def _is_loopback_host(host: str) -> bool:
    lowered = host.strip("[]").lower()
    if lowered == "localhost" or lowered.endswith(".localhost"):
        return True
    try:
        return ipaddress.ip_address(lowered).is_loopback
    except ValueError:
        return False


def _validate_headers(value: object) -> dict[str, str] | None:
    if value is None:
        return None
    if not isinstance(value, dict) or len(value) > 32:
        raise ProviderStoreError("invalid_params", "customHeaders must contain at most 32 approved metadata headers")
    result: dict[str, str] = {}
    seen: set[str] = set()
    for name, raw in value.items():
        normalized = name.strip().lower() if isinstance(name, str) else ""
        text = raw.strip() if isinstance(raw, str) else ""
        if (
            normalized not in _SAFE_HEADERS or normalized in seen
            or not text or len(text) > 4096 or "\r" in text or "\n" in text
            or re.match(r"^(?:bearer|basic)\s", text, re.IGNORECASE)
        ):
            raise ProviderStoreError("invalid_params", "customHeaders contains a non-approved or credential-like value")
        seen.add(normalized)
        result[name.strip()] = text
    return result or None


def _validate_models(value: object) -> list[dict[str, Any]] | None:
    if value is None:
        return None
    if not isinstance(value, list) or len(value) > 1000:
        raise ProviderStoreError("invalid_params", "Provider model catalog is invalid")
    models: list[dict[str, Any]] = []
    for item in value:
        if not isinstance(item, dict) or set(item) - {
            "id", "label", "contextWindow", "supportsToolCalling", "supportsStreaming"
        }:
            raise ProviderStoreError("invalid_params", "Provider model entry is invalid")
        model_id = item.get("id")
        label = item.get("label")
        context_window = item.get("contextWindow")
        supports_tools = item.get("supportsToolCalling")
        supports_streaming = item.get("supportsStreaming")
        if (
            not isinstance(model_id, str) or not model_id.strip() or len(model_id.strip()) > 256
            or not isinstance(label, str) or not label.strip() or len(label.strip()) > 256
            or context_window is not None and (
                isinstance(context_window, bool) or not isinstance(context_window, int)
                or not 1 <= context_window <= 10_000_000
            )
            or not isinstance(supports_tools, bool)
            or not isinstance(supports_streaming, bool)
        ):
            raise ProviderStoreError("invalid_params", "Provider model entry is invalid")
        entry: dict[str, Any] = {
            "id": model_id.strip(),
            "label": label.strip(),
            "supportsToolCalling": supports_tools,
            "supportsStreaming": supports_streaming,
        }
        if context_window is not None:
            entry["contextWindow"] = context_window
        models.append(entry)
    return models


class ProviderStore:
    """Python-owned access to AI provider rows in the existing desktop SQLite database."""

    def __init__(self, database_path: str | Path) -> None:
        self.path = Path(database_path)

    def list_ai(self) -> list[dict[str, Any]]:
        with closing(self._connect()) as connection:
            rows = connection.execute(
                "SELECT id, kind, label, base_url, model, api_key_credential_ref, enabled, "
                "custom_headers, max_input_tokens, settings, created_at, updated_at, wire_api, api_version "
                "FROM provider_configs WHERE family = 'ai' ORDER BY label"
            ).fetchall()
        return [_provider_from_row(row) for row in rows]

    def normalize_ai_state(self, usable_ids: set[str]) -> list[dict[str, Any]]:
        """Keep exactly the most recent usable AI provider active and sanitize stored headers."""
        connection = self._connect()
        try:
            connection.execute("BEGIN IMMEDIATE")
            rows = connection.execute(
                "SELECT id, kind, label, base_url, model, api_key_credential_ref, enabled, "
                "custom_headers, max_input_tokens, settings, created_at, updated_at, wire_api, api_version "
                "FROM provider_configs WHERE family = 'ai'"
            ).fetchall()
            providers = [_provider_from_row(row) for row in rows]
            eligible = [provider for provider in providers if provider["id"] in usable_ids]
            active_eligible = [provider for provider in eligible if provider["enabled"]]
            candidates = active_eligible or eligible
            primary_id = max(
                candidates,
                key=lambda provider: (
                    provider.get("updatedAt") or "",
                    provider.get("createdAt") or "",
                    provider["id"],
                ),
                default=None,
            )
            primary_id = primary_id["id"] if primary_id is not None else None
            now = _now()
            for row, provider in zip(rows, providers, strict=True):
                should_enable = provider["id"] == primary_id
                stored_headers = _decode_json_object(row[7])
                headers = _sanitize_stored_headers(stored_headers) if stored_headers else None
                raw_normalized = json.dumps(headers, separators=(",", ":"), allow_nan=False) if headers else None
                if provider["enabled"] != should_enable or raw_normalized != row[7]:
                    connection.execute(
                        "UPDATE provider_configs SET enabled = ?, custom_headers = ?, updated_at = ? "
                        "WHERE id = ? AND family = 'ai'",
                        (int(should_enable), raw_normalized, now, provider["id"]),
                    )
            connection.commit()
        except ProviderStoreError:
            connection.rollback()
            raise
        except sqlite3.Error:
            connection.rollback()
            raise ProviderStoreError("persistence_failed", "Could not normalize AI provider configuration") from None
        finally:
            connection.close()
        return self.list_ai()

    def get_ai(self, provider_id: str) -> dict[str, Any] | None:
        with closing(self._connect()) as connection:
            row = connection.execute(
                "SELECT id, kind, label, base_url, model, api_key_credential_ref, enabled, "
                "custom_headers, max_input_tokens, settings, created_at, updated_at, wire_api, api_version "
                "FROM provider_configs WHERE id = ? AND family = 'ai'",
                (provider_id,),
            ).fetchone()
        return _provider_from_row(row) if row is not None else None

    def get_save_target(self, requested_id: str | None) -> dict[str, Any] | None:
        if requested_id is not None:
            return self.get_ai(requested_id)
        with closing(self._connect()) as connection:
            row = connection.execute(
                "SELECT id, kind, label, base_url, model, api_key_credential_ref, enabled, "
                "custom_headers, max_input_tokens, settings, created_at, updated_at, wire_api, api_version "
                "FROM provider_configs WHERE family = 'ai' AND enabled != 0 ORDER BY label LIMIT 1"
            ).fetchone()
        return _provider_from_row(row) if row is not None else None

    def save_ai(
        self,
        provider_id: str,
        config: dict[str, Any],
        new_credential_ref: str | None,
    ) -> tuple[dict[str, Any], str | None]:
        now = _now()
        connection = self._connect()
        try:
            connection.execute("BEGIN IMMEDIATE")
            existing_row = connection.execute(
                "SELECT id, kind, label, base_url, model, api_key_credential_ref, enabled, "
                "custom_headers, max_input_tokens, settings, created_at, updated_at, wire_api, api_version, family "
                "FROM provider_configs WHERE id = ?",
                (provider_id,),
            ).fetchone()
            if existing_row is not None and existing_row[14] != "ai":
                raise ProviderStoreError("conflict", "Provider ID is already used by another provider family")
            existing = _provider_from_row(existing_row[:14]) if existing_row is not None else None
            old_ref = existing.get("apiKeyCredentialRef") if existing else None
            credential_ref = new_credential_ref or old_ref
            models = config["models"] if config["models"] is not None else (existing or {}).get("models")
            created_at = (existing or {}).get("createdAt", now)
            headers = config["customHeaders"]
            wire_api = config["wireApi"] if config["kind"] == "openai-compatible" else None
            settings = json.dumps({"models": models}, separators=(",", ":"), allow_nan=False) if models is not None else None
            connection.execute(
                "UPDATE provider_configs SET enabled = 0, updated_at = ? "
                "WHERE family = 'ai' AND enabled != 0 AND id != ?",
                (now, provider_id),
            )
            connection.execute(
                "INSERT INTO provider_configs (id, family, kind, label, base_url, model, "
                "api_key_credential_ref, enabled, custom_headers, max_input_tokens, settings, "
                "wire_api, api_version, created_at, updated_at) "
                "VALUES (?, 'ai', ?, ?, ?, ?, ?, 1, ?, NULL, ?, ?, ?, ?, ?) "
                "ON CONFLICT(id) DO UPDATE SET family='ai', kind=excluded.kind, label=excluded.label, "
                "base_url=excluded.base_url, model=excluded.model, "
                "api_key_credential_ref=excluded.api_key_credential_ref, enabled=1, "
                "custom_headers=excluded.custom_headers, max_input_tokens=NULL, settings=excluded.settings, "
                "wire_api=excluded.wire_api, api_version=excluded.api_version, updated_at=excluded.updated_at",
                (
                    provider_id, config["kind"], config["label"], config["baseUrl"], config["model"],
                    credential_ref, json.dumps(headers, separators=(",", ":"), allow_nan=False) if headers else None,
                    settings, wire_api or "", config["apiVersion"], created_at, now,
                ),
            )
            connection.commit()
        except ProviderStoreError:
            connection.rollback()
            raise
        except sqlite3.Error:
            connection.rollback()
            raise ProviderStoreError("persistence_failed", "Could not save provider configuration") from None
        finally:
            connection.close()
        saved = self.get_ai(provider_id)
        if saved is None:
            raise ProviderStoreError("persistence_failed", "Saved provider configuration could not be read back")
        return saved, old_ref

    def activate_ai(self, provider_id: str) -> dict[str, Any]:
        now = _now()
        connection = self._connect()
        try:
            connection.execute("BEGIN IMMEDIATE")
            selected = connection.execute(
                "SELECT 1 FROM provider_configs WHERE id = ? AND family = 'ai'", (provider_id,)
            ).fetchone()
            if selected is None:
                raise ProviderStoreError("not_found", f"Provider `{provider_id}` was not found")
            connection.execute(
                "UPDATE provider_configs SET enabled = 0, updated_at = ? "
                "WHERE family = 'ai' AND enabled != 0 AND id != ?",
                (now, provider_id),
            )
            connection.execute(
                "UPDATE provider_configs SET enabled = 1, updated_at = ? "
                "WHERE id = ? AND family = 'ai' AND enabled = 0",
                (now, provider_id),
            )
            connection.commit()
        except ProviderStoreError:
            connection.rollback()
            raise
        except sqlite3.Error:
            connection.rollback()
            raise ProviderStoreError("persistence_failed", "Could not activate provider") from None
        finally:
            connection.close()
        selected_provider = self.get_ai(provider_id)
        if selected_provider is None:
            raise ProviderStoreError("not_found", f"Provider `{provider_id}` was not found")
        return selected_provider

    def delete_ai(self, provider_id: str) -> tuple[str | None, bool]:
        connection = self._connect()
        try:
            connection.execute("BEGIN IMMEDIATE")
            row = connection.execute(
                "SELECT api_key_credential_ref FROM provider_configs WHERE id = ? AND family = 'ai'",
                (provider_id,),
            ).fetchone()
            if row is None:
                raise ProviderStoreError("not_found", f"Provider `{provider_id}` was not found")
            reference = row[0]
            shared = False
            if reference:
                shared = connection.execute(
                    "SELECT EXISTS(SELECT 1 FROM provider_configs WHERE id != ? "
                    "AND (api_key_credential_ref = ? OR credential_ref = ?))",
                    (provider_id, reference, reference),
                ).fetchone()[0] != 0
            connection.execute("DELETE FROM provider_configs WHERE id = ? AND family = 'ai'", (provider_id,))
            connection.commit()
            return reference, shared
        except ProviderStoreError:
            connection.rollback()
            raise
        except sqlite3.Error:
            connection.rollback()
            raise ProviderStoreError("persistence_failed", "Could not delete provider configuration") from None
        finally:
            connection.close()

    def credential_ref_used_elsewhere(self, reference: str, excluding_id: str) -> bool:
        with closing(self._connect()) as connection:
            row = connection.execute(
                "SELECT EXISTS(SELECT 1 FROM provider_configs WHERE id != ? "
                "AND (api_key_credential_ref = ? OR credential_ref = ?))",
                (excluding_id, reference, reference),
            ).fetchone()
        return bool(row[0])

    def update_models(self, provider_id: str, models: list[dict[str, Any]]) -> None:
        now = _now()
        connection = self._connect()
        try:
            changed = connection.execute(
                "UPDATE provider_configs SET settings = ?, updated_at = ? WHERE id = ? AND family = 'ai'",
                (json.dumps({"models": models}, separators=(",", ":"), allow_nan=False), now, provider_id),
            ).rowcount
            if changed == 0:
                raise ProviderStoreError("not_found", f"Provider `{provider_id}` was not found")
            connection.commit()
        except ProviderStoreError:
            raise
        except sqlite3.Error:
            raise ProviderStoreError("persistence_failed", "Could not save provider model catalog") from None
        finally:
            connection.close()

    def _connect(self) -> sqlite3.Connection:
        try:
            connection = sqlite3.connect(self.path, timeout=5.0, isolation_level=None)
            connection.row_factory = sqlite3.Row
            connection.execute("PRAGMA busy_timeout = 5000")
            return connection
        except sqlite3.Error:
            raise ProviderStoreError("persistence_failed", "Provider database is unavailable") from None


def _provider_from_row(row: sqlite3.Row | tuple[Any, ...]) -> dict[str, Any]:
    raw_kind = row[1]
    kind = "openai-compatible" if raw_kind == "openaicompatible" else raw_kind
    if kind not in _KINDS:
        raise ProviderStoreError("persistence_failed", "Provider database contains an unsupported provider kind")
    config: dict[str, Any] = {
        "id": row[0],
        "kind": kind,
        "label": row[2],
        "baseUrl": row[3] or "",
        "model": row[4] or "",
        "enabled": bool(row[6]),
        "createdAt": row[10],
        "updatedAt": row[11],
    }
    if row[5]:
        config["apiKeyCredentialRef"] = row[5]
    if row[7]:
        try:
            headers = json.loads(row[7])
        except (TypeError, json.JSONDecodeError):
            headers = None
        if isinstance(headers, dict):
            config["customHeaders"] = _sanitize_stored_headers(headers)
            if not config["customHeaders"]:
                config.pop("customHeaders")
    maximum = row[8]
    if isinstance(maximum, int) and not isinstance(maximum, bool) and 1 <= maximum <= 10_000_000:
        config["maxInputTokens"] = maximum
    if row[9]:
        try:
            settings = json.loads(row[9])
        except (TypeError, json.JSONDecodeError):
            settings = None
        models = settings.get("models") if isinstance(settings, dict) else settings
        if isinstance(models, list):
            try:
                config["models"] = _validate_models(models)
            except ProviderStoreError:
                pass
    if kind == "openai-compatible":
        config["wireApi"] = row[12] if isinstance(row[12], str) and row[12].strip() else "chat"
    if kind == "anthropic" and isinstance(row[13], str) and row[13].strip():
        config["apiVersion"] = row[13].strip()
    return config


def _sanitize_stored_headers(headers: dict[str, Any]) -> dict[str, str]:
    safe: dict[str, str] = {}
    for name, value in headers.items():
        lowered = name.strip().lower() if isinstance(name, str) else ""
        text = value.strip() if isinstance(value, str) else ""
        if (
            lowered in _SAFE_HEADERS and text and len(text) <= 4096
            and "\r" not in text and "\n" not in text
            and not re.match(r"^(?:bearer|basic)\s", text, re.IGNORECASE)
        ):
            safe[name.strip()] = text
    return safe


def _decode_json_object(value: object) -> dict[str, Any] | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        decoded = json.loads(value)
    except json.JSONDecodeError:
        return None
    return decoded if isinstance(decoded, dict) else None


def _now() -> str:
    return datetime.now(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")
