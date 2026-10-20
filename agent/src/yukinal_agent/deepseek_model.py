from __future__ import annotations

from typing import Any

from langchain_core.messages import AIMessage, AIMessageChunk
from langchain_openai import ChatOpenAI


class DeepSeekChatModel(ChatOpenAI):
    """Preserve DeepSeek's private thinking state across a tool exchange.

    ChatOpenAI deliberately drops non-OpenAI message fields. DeepSeek's thinking
    tool loop requires reasoning_content from the original assistant tool-call
    response on the next request. Keep it in additional_kwargs (and therefore in
    private checkpoints), never in content or the public agent event stream.
    """

    # DeepSeek supports stream_options.include_usage; request actual terminal
    # token counts rather than estimating usage from displayed characters.
    stream_usage: bool | None = True

    def _convert_chunk_to_generation_chunk(
        self, chunk: dict[str, Any], default_chunk_class: type, base_generation_info: dict[str, Any] | None,
    ) -> Any:
        result = super()._convert_chunk_to_generation_chunk(chunk, default_chunk_class, base_generation_info)
        choices = chunk.get("choices") or chunk.get("chunk", {}).get("choices", [])
        delta = choices[0].get("delta") if choices else None
        reasoning = delta.get("reasoning_content") if isinstance(delta, dict) else None
        if result is not None and isinstance(result.message, AIMessageChunk) and isinstance(reasoning, str):
            result.message.additional_kwargs["reasoning_content"] = reasoning
        if result is not None and isinstance(result.message, AIMessageChunk):
            usage = _cache_usage(chunk.get("usage"))
            if usage:
                result.message.response_metadata["deepseek_usage"] = usage
        return result

    def _create_chat_result(self, response: Any, generation_info: dict[str, Any] | None = None) -> Any:
        result = super()._create_chat_result(response, generation_info)
        raw = response if isinstance(response, dict) else response.model_dump()
        for generation, choice in zip(result.generations, raw.get("choices", [])):
            reasoning = choice.get("message", {}).get("reasoning_content")
            if isinstance(generation.message, AIMessage) and isinstance(reasoning, str):
                generation.message.additional_kwargs["reasoning_content"] = reasoning
            if isinstance(generation.message, AIMessage) and (usage := _cache_usage(raw.get("usage"))):
                generation.message.response_metadata["deepseek_usage"] = usage
        return result

    def _get_request_payload(self, input_: Any, *, stop: list[str] | None = None, **kwargs: Any) -> dict[str, Any]:
        payload = super()._get_request_payload(input_, stop=stop, **kwargs)
        # DeepSeek's documented chat contract names the output bound max_tokens.
        if "max_completion_tokens" in payload:
            payload["max_tokens"] = payload.pop("max_completion_tokens")
        messages = self._convert_input(input_).to_messages()
        for message, outgoing in zip(messages, payload.get("messages", [])):
            if isinstance(message, AIMessage):
                reasoning = message.additional_kwargs.get("reasoning_content")
                if isinstance(reasoning, str):
                    outgoing["reasoning_content"] = reasoning
        return payload


def _cache_usage(raw: Any) -> dict[str, int]:
    # ChatOpenAI normalizes standard usage but drops DeepSeek's cache counters.
    # Retain only actual, bounded counts, including a reported zero.
    return {key: raw[key] for key in ("prompt_cache_hit_tokens", "prompt_cache_miss_tokens")
            if isinstance(raw, dict) and type(raw.get(key)) is int and 0 <= raw[key] <= 2**53 - 1}
