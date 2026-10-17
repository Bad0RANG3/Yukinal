from __future__ import annotations

import os
import re
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any, Callable, Literal, Mapping
from urllib.parse import urlsplit

Provider = Literal["openai", "openai-compatible", "anthropic", "gemini"]
_ENV_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,127}$")
_ENV_REFERENCE = re.compile(r"^\$\{([A-Za-z_][A-Za-z0-9_]{0,127})\}$")
_API_VERSION = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_SAFE_HEADERS = {
    "http-referer", "referer", "origin", "user-agent", "x-app-name", "x-app-version",
    "x-client-name", "x-client-version", "x-title", "anthropic-beta", "openai-organization",
    "openai-project", "x-goog-user-project",
}


class ModelFactoryError(RuntimeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


@dataclass(frozen=True)
class ModelConfig:
    provider: Provider
    model: str
    api_key_env: str | None = None
    temperature: float = 0.0
    timeout_seconds: float = 60.0
    max_retries: int = 2
    max_output_tokens: int = 2048
    base_url: str | None = None
    custom_headers: dict[str, str] | None = None
    api_version: str | None = None
    wire_api: Literal["chat", "responses"] | None = None

    def __post_init__(self) -> None:
        if self.provider not in ("openai", "openai-compatible", "anthropic", "gemini"):
            raise ValueError("model provider is not supported")
        if not isinstance(self.model, str) or not self.model.strip() or len(self.model) > 256:
            raise ValueError("model name is invalid")
        if "\n" in self.model or "\r" in self.model:
            raise ValueError("model name is invalid")
        if self.api_key_env is not None and (
            not isinstance(self.api_key_env, str) or not _ENV_NAME.fullmatch(self.api_key_env)
        ):
            raise ValueError("API key must be referenced by an environment variable name")
        if isinstance(self.temperature, bool) or not isinstance(self.temperature, (int, float)) or not 0 <= self.temperature <= 2:
            raise ValueError("temperature must be between 0 and 2")
        if isinstance(self.timeout_seconds, bool) or not isinstance(self.timeout_seconds, (int, float)) or not 0.1 <= self.timeout_seconds <= 600:
            raise ValueError("timeout_seconds must be between 0.1 and 600")
        if isinstance(self.max_retries, bool) or not isinstance(self.max_retries, int) or not 0 <= self.max_retries <= 10:
            raise ValueError("max_retries must be between 0 and 10")
        if isinstance(self.max_output_tokens, bool) or not isinstance(self.max_output_tokens, int) or not 1 <= self.max_output_tokens <= 32768:
            raise ValueError("max_output_tokens must be between 1 and 32768")
        if self.base_url is not None:
            _validate_base_url(self.base_url)
        if self.api_version is not None and (
            self.provider != "anthropic" or not _API_VERSION.fullmatch(self.api_version)
        ):
            raise ValueError("api_version is a YYYY-MM-DD value for Anthropic only")
        if self.wire_api is not None and (
            self.provider not in ("openai", "openai-compatible") or self.wire_api not in ("chat", "responses")
        ):
            raise ValueError("wire_api is supported by OpenAI-compatible models only")
        if self.custom_headers is not None:
            if not isinstance(self.custom_headers, Mapping) or len(self.custom_headers) > 32:
                raise ValueError("too many provider headers")
            normalized_names: set[str] = set()
            for name, value in self.custom_headers.items():
                normalized_name = name.strip() if isinstance(name, str) else ""
                lowered_name = normalized_name.lower()
                if (
                    not isinstance(name, str)
                    or not normalized_name
                    or len(normalized_name) > 128
                    or lowered_name not in _SAFE_HEADERS
                    or lowered_name in normalized_names
                    or not isinstance(value, str)
                    or not value.strip()
                    or len(value.strip()) > 4096
                    or "\r" in value
                    or "\n" in value
                    or re.match(r"^(?:bearer|basic)\s", value, re.IGNORECASE)
                ):
                    raise ValueError("provider header is not approved metadata")
                normalized_names.add(lowered_name)
            headers = {name.strip(): value.strip() for name, value in self.custom_headers.items()}
            object.__setattr__(self, "custom_headers", MappingProxyType(headers))


ModelBuilder = Callable[..., Any]


class ModelFactory:
    """Create provider adapters from secret references, with no provider-specific agent code."""

    def __init__(self, builders: Mapping[str, ModelBuilder] | None = None) -> None:
        self._builders = dict(builders or {})

    def create(self, config: ModelConfig) -> Any:
        api_key = os.environ.get(config.api_key_env) if config.api_key_env else None
        return self.create_with_api_key(config, api_key)

    def create_with_api_key(self, config: ModelConfig, api_key: str | None) -> Any:
        """Build a single-run model from a transient host-injected key."""
        if api_key is not None and (
            not isinstance(api_key, str)
            or len(api_key) > 4096
            or "\r" in api_key
            or "\n" in api_key
        ):
            raise ModelFactoryError("invalid_api_key", "Configured model credential is invalid")
        provider = "openai" if config.provider == "openai-compatible" else config.provider
        if (provider in {"anthropic", "gemini"} or config.provider == "openai") and not api_key:
            raise ModelFactoryError("missing_api_key", "Configured model API key is unavailable")
        model_name = _resolve(config.model)
        if not model_name:
            raise ModelFactoryError("missing_model", "Configured model name is unavailable")
        if len(model_name) > 256 or "\n" in model_name or "\r" in model_name:
            raise ModelFactoryError("invalid_model", "Configured model name is invalid")
        base_url = _resolve(config.base_url) if config.base_url is not None else None
        if config.base_url is not None and not base_url:
            raise ModelFactoryError("missing_base_url", "Configured model endpoint is unavailable")
        if base_url is not None:
            try:
                _validate_base_url(base_url)
            except ValueError:
                raise ModelFactoryError("invalid_base_url", "Configured model endpoint is invalid") from None
        builder = self._builders.get(provider)
        if builder is None:
            if provider == "openai" and base_url is not None and urlsplit(base_url).hostname == "api.deepseek.com" and config.wire_api != "responses":
                from .deepseek_model import DeepSeekChatModel

                builder = DeepSeekChatModel
            else:
                builder = self._load_builder(provider)
        if provider == "gemini":
            kwargs: dict[str, Any] = {
                "model": model_name,
                "api_key": api_key,
                "temperature": config.temperature,
                "request_timeout": float(config.timeout_seconds),
                "retries": config.max_retries,
                "max_tokens": config.max_output_tokens,
            }
            if base_url is not None:
                kwargs["base_url"] = base_url
            if config.custom_headers:
                kwargs["additional_headers"] = config.custom_headers
        elif provider == "anthropic":
            headers = dict(config.custom_headers or {})
            if config.api_version:
                headers["anthropic-version"] = config.api_version
            kwargs = {
                "model": model_name,
                "api_key": api_key,
                "temperature": config.temperature,
                "timeout": float(config.timeout_seconds),
                "max_retries": config.max_retries,
                "max_tokens": config.max_output_tokens,
            }
            if base_url is not None:
                kwargs["base_url"] = base_url
            if headers:
                kwargs["default_headers"] = headers
        else:
            # An explicit placeholder prevents a machine-level OPENAI_API_KEY from
            # being sent to a custom endpoint when the user configured a local model.
            kwargs = {
                "model": model_name,
                "api_key": api_key or "EMPTY",
                "temperature": config.temperature,
                "timeout": float(config.timeout_seconds),
                "max_retries": config.max_retries,
                "max_tokens": config.max_output_tokens,
                "use_responses_api": config.wire_api == "responses",
            }
            if base_url is not None:
                kwargs["base_url"] = base_url
            if config.custom_headers:
                kwargs["default_headers"] = config.custom_headers
        try:
            return builder(**kwargs)
        except Exception:
            raise ModelFactoryError("model_configuration_invalid", "Model provider configuration could not be initialized") from None

    @staticmethod
    def _load_builder(provider: str) -> ModelBuilder:
        try:
            if provider == "openai":
                from langchain_openai import ChatOpenAI

                return ChatOpenAI
            if provider == "anthropic":
                from langchain_anthropic import ChatAnthropic

                return ChatAnthropic
            from langchain_google_genai import ChatGoogleGenerativeAI

            return ChatGoogleGenerativeAI
        except ImportError:
            raise ModelFactoryError("provider_dependency_missing", "The configured model provider is not installed") from None


def _resolve(value: str | None) -> str | None:
    if value is None:
        return None
    reference = _ENV_REFERENCE.fullmatch(value)
    return os.environ.get(reference.group(1), "") if reference is not None else value


def with_reasoning_effort(model: Any, effort: str) -> Any:
    """Apply a documented DeepSeek parameter before tools/output budgets are bound."""
    from .deepseek_model import DeepSeekChatModel

    if (
        effort not in {"none", "low", "high", "max"}
        or not isinstance(model, DeepSeekChatModel)
        or urlsplit(model.openai_api_base or "").scheme != "https"
        or urlsplit(model.openai_api_base or "").hostname != "api.deepseek.com"
        or model.model_name not in {"deepseek-flash", "deepseek-v4-pro"}
    ):
        raise ModelFactoryError("reasoning_not_supported", "This model does not support the selected reasoning effort")
    extra_body = {**(model.extra_body or {}), "thinking": {"type": "disabled" if effort == "none" else "enabled"}}
    return model.model_copy(update={"reasoning_effort": None if effort == "none" else effort, "extra_body": extra_body})


def _validate_base_url(value: str) -> None:
    if not isinstance(value, str):
        raise ValueError("base_url is invalid")
    reference = _ENV_REFERENCE.fullmatch(value)
    url = "https://placeholder.invalid" if reference is not None else value
    parsed = urlsplit(url)
    if (
        parsed.scheme not in ("http", "https")
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.port is not None and not 1 <= parsed.port <= 65535
    ):
        raise ValueError("base_url must be an http(s) endpoint without embedded credentials")
    if parsed.scheme == "http" and parsed.hostname not in {"localhost", "127.0.0.1", "::1"}:
        raise ValueError("non-local model endpoints must use HTTPS")
