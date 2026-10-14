from __future__ import annotations

import os
import re
from dataclasses import dataclass
from typing import Any, Callable, Literal, Mapping
from urllib.parse import urlsplit

Provider = Literal["openai", "anthropic"]
_ENV_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,127}$")
_ENV_REFERENCE = re.compile(r"^\$\{([A-Za-z_][A-Za-z0-9_]{0,127})\}$")


class ModelFactoryError(RuntimeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


@dataclass(frozen=True)
class ModelConfig:
    provider: Provider
    model: str
    api_key_env: str
    temperature: float = 0.0
    timeout_seconds: float = 60.0
    max_retries: int = 2
    max_output_tokens: int = 2048
    base_url: str | None = None

    def __post_init__(self) -> None:
        if self.provider not in ("openai", "anthropic"):
            raise ValueError("model provider is not supported")
        if not isinstance(self.model, str) or not self.model.strip() or len(self.model) > 256:
            raise ValueError("model name is invalid")
        if "\n" in self.model or "\r" in self.model:
            raise ValueError("model name is invalid")
        if not isinstance(self.api_key_env, str) or not _ENV_NAME.fullmatch(self.api_key_env):
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


ModelBuilder = Callable[..., Any]


class ModelFactory:
    """Create provider adapters from secret references, with no provider-specific agent code."""

    def __init__(self, builders: Mapping[str, ModelBuilder] | None = None) -> None:
        self._builders = dict(builders or {})

    def create(self, config: ModelConfig) -> Any:
        api_key = os.environ.get(config.api_key_env)
        if not api_key:
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
        builder = self._builders.get(config.provider)
        if builder is None:
            builder = self._load_builder(config.provider)
        kwargs: dict[str, Any] = {
            "model": model_name,
            "api_key": api_key,
            "temperature": config.temperature,
            "timeout": float(config.timeout_seconds),
            "max_retries": config.max_retries,
            "max_tokens": config.max_output_tokens,
        }
        if base_url is not None:
            kwargs["base_url"] = base_url
        try:
            return builder(**kwargs)
        except Exception:
            raise ModelFactoryError("model_configuration_invalid", "Model provider configuration could not be initialized") from None

    @staticmethod
    def _load_builder(provider: Provider) -> ModelBuilder:
        try:
            if provider == "openai":
                from langchain_openai import ChatOpenAI

                return ChatOpenAI
            from langchain_anthropic import ChatAnthropic

            return ChatAnthropic
        except ImportError:
            raise ModelFactoryError("provider_dependency_missing", "The configured model provider is not installed") from None


def _resolve(value: str | None) -> str | None:
    if value is None:
        return None
    reference = _ENV_REFERENCE.fullmatch(value)
    return os.environ.get(reference.group(1), "") if reference is not None else value


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
